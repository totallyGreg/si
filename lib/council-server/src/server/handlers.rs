use std::{
    collections::{HashSet, VecDeque},
    ops::DerefMut,
    result,
    str::FromStr,
};

use council_core::{
    AttributeValueId, AttributeValueStatus, ChangeSetPk, ClientId, ClientUpdateRequest,
    DependencyGraphRequest,
};
use indexmap::IndexMap;
use naxum::{
    extract::State,
    response::{IntoResponse, Response},
    BoxError,
};
use si_data_nats::{
    async_nats::{self, jetstream},
    Subject,
};
use telemetry::prelude::*;
use thiserror::Error;

use crate::{
    app_state::{AppState, AttributeValueMaxAttempts},
    kv::KvStoreError,
    state_machine::StateMachineError,
    DependencyGraphData, DependencyGraphDataEntry, StateStore, StatusStore,
};

use super::extract::IncomingSubject;

#[remain::sorted]
#[derive(Debug, Error)]
pub enum HandlerError {
    #[error("error deserializing message: {0}")]
    Deserialize(#[source] serde_json::Error),
    #[error("invalid updates subject: {0}")]
    InvalidIncomingSubject(Subject),
    #[error("kv entry not found: {0}")]
    KvEntryNotFound(String),
    #[error("kv store error: {0}")]
    KvStore(#[from] KvStoreError),
    #[error("nats error: {0}")]
    Nats(#[from] async_nats::Error),
    #[error("state machine error: {0}")]
    StateMachine(#[from] StateMachineError),
    #[error("process timeout exceeded: {0}")]
    Timeout(#[source] BoxError),
    #[error("failed to decode ulid: {0}")]
    UlidDecode(#[from] ulid::DecodeError),
}

pub type Result<T> = result::Result<T, HandlerError>;

impl IntoResponse for HandlerError {
    fn into_response(self) -> Response {}
}

pub async fn router(
    State(state): State<AppState>,
    IncomingSubject(incoming_subject): IncomingSubject,
    msg: jetstream::Message,
) -> Result<()> {
    let mut parts = incoming_subject.split('.');

    let result = match (parts.next(), parts.next(), parts.next()) {
        // New changeset depdendency graph message
        (Some("cs"), Some(cs_str), None) => {
            let change_set_pk = ChangeSetPk::from_str(cs_str)?;
            let request: DependencyGraphRequest =
                serde_json::from_slice(&msg.payload).map_err(HandlerError::Deserialize)?;

            add_dependency_graph(state.state(), state.status(), change_set_pk, request).await
        }
        // Client update message
        (Some("client"), Some(client_str), None) => {
            let client_id = ClientId::from_str(client_str)?;
            let request: ClientUpdateRequest =
                serde_json::from_slice(&msg.payload).map_err(HandlerError::Deserialize)?;

            process_client_update(
                state.state(),
                state.status(),
                client_id,
                request,
                state.attribute_value_max_attempts(),
            )
            .await
        }
        // State machine step message
        (Some("step"), Some("find_ready_to_process"), None) => {
            find_ready_to_process(state.state(), state.status()).await
        }
        // Unrecognized subject
        _ => Err(HandlerError::InvalidIncomingSubject(msg.subject.clone())),
    };

    match result {
        Ok(_) => {
            // Sucess! Send "Ack" and confirm receipt
            msg.double_ack().await?;
        }
        Err(_) => {
            // An error occured! Send "Nack" with no delay to trigger an immediate redelivery/retry
            msg.ack_with(jetstream::AckKind::Nak(None)).await?;
        }
    };

    result
}

pub async fn error(err: HandlerError) {
    if matches!(err, HandlerError::Timeout(_)) {
        error!(error = ?err, "message took too long to process");
    } else {
        error!(error = ?err, "error occured while processing message");
    }
}

async fn add_dependency_graph(
    state: &StateStore,
    status: &StatusStore,
    change_set_pk: ChangeSetPk,
    request: DependencyGraphRequest,
) -> Result<()> {
    let (mut change_set_graph_data, revision) = state
        .get_or_create_change_set_graph(change_set_pk)
        .await?
        .unwrap_or_else(|| (DependencyGraphData::default(), 0.into()));

    for (attribute_value_id, new_depends_on_values) in request.graph.into_inner() {
        change_set_graph_data
            .deref_mut()
            .entry(attribute_value_id)
            .and_modify(|data_entry| {
                if !data_entry
                    .interested_clients_fifo
                    .contains(&request.client_id)
                {
                    data_entry
                        .interested_clients_fifo
                        .push_back(request.client_id);
                }
                data_entry.depends_on_values.extend(&new_depends_on_values);
            })
            .or_insert_with(|| DependencyGraphDataEntry {
                interested_clients_fifo: VecDeque::from(vec![request.client_id]),
                depends_on_values: new_depends_on_values.clone(),
                ..Default::default()
            });

        for depndency_attribute_value_id in new_depends_on_values {
            change_set_graph_data
                .deref_mut()
                .entry(depndency_attribute_value_id)
                .and_modify(|data_entry| {
                    if !data_entry
                        .interested_clients_fifo
                        .contains(&request.client_id)
                    {
                        data_entry
                            .interested_clients_fifo
                            .push_back(request.client_id);
                    }
                })
                .or_insert_with(|| DependencyGraphDataEntry {
                    interested_clients_fifo: VecDeque::from(vec![request.client_id]),
                    ..Default::default()
                });
        }
    }

    state
        .update_change_set_graph(change_set_pk, &change_set_graph_data, revision)
        .await?;

    find_ready_to_process(state, status).await?;

    Ok(())
}

async fn process_client_update(
    state: &StateStore,
    status: &StatusStore,
    client_id: ClientId,
    request: ClientUpdateRequest,
    attribute_value_max_attempts: AttributeValueMaxAttempts,
) -> Result<()> {
    trace!(
        si.council.client.id = %client_id,
        update = ?request,
        "client update",
    );

    match request {
        ClientUpdateRequest::AckPending {
            attribute_value_id,
            change_set_pk,
        } => status
            .set_attribute_value_processing(change_set_pk, attribute_value_id, client_id)
            .await
            .map_err(Into::into),
        ClientUpdateRequest::AckProcessed {
            change_set_pk,
            attribute_value_id,
        } => {
            set_attribute_value_processed(
                state,
                status,
                change_set_pk,
                attribute_value_id,
                client_id,
            )
            .await
        }
        ClientUpdateRequest::NackProcessed {
            change_set_pk,
            attribute_value_id,
        } => {
            handle_attribute_value_process_failed(
                state,
                status,
                change_set_pk,
                attribute_value_id,
                attribute_value_max_attempts,
            )
            .await
        }
        ClientUpdateRequest::Shutdown { change_set_pk } => {
            remove_client_from_change_set(state, status, change_set_pk, client_id).await
        }
    }
}

async fn find_ready_to_process(state: &StateStore, status: &StatusStore) -> Result<()> {
    let change_set_pks = state.change_set_pks_from_keys().await?;

    // A collection of attribute values which have a new status.
    //
    // - An attribute value will be set up for pending processing, if the following
    //   criteria are met:
    //
    //   1. the value has an empty collection of "depends on" values
    //   2. there is at least one interested client, meaning we will assign that client to
    //      process the value
    //
    // - An attribute value will be set up as orphaned if there are no interested clients
    //   (see below for orphaned attribute values).

    // We want set semantics, but preserve insertion order so the *last* next status is preserved
    let mut next_attribute_value_statuses = IndexMap::new();

    let mut active_change_set_statuses = HashSet::new();
    let mut cleanup_change_set_statuses = HashSet::new();

    for change_set_pk in change_set_pks {
        // Fetch the graph data for the change set from the kv store
        let (mut change_set_graph_data, revision) = state
            .get_change_set_graph(change_set_pk)
            .await?
            .ok_or_else(|| HandlerError::KvEntryNotFound(change_set_pk.to_string()))?;

        // Orphan values are values with an empty list of interested clients, meaning that
        // no client would be available to process the attribute value
        let mut orphan_attribute_value_ids = HashSet::new();

        // Visit all entries of the immutable change set graph (i.e. read-only, no mutations)
        for (attribute_value_id, data_entry) in change_set_graph_data.iter() {
            match data_entry.interested_clients_fifo.front() {
                // The first/oldest client will be responsible for processing the attribute
                // value
                Some(client_id) => {
                    // If the attribute value has no values it's waiting on, then we'll be
                    // setting the value up for pending processing. Otherwise the value
                    // needs to wait on other values, so we do nothing
                    if data_entry.depends_on_values.is_empty() {
                        next_attribute_value_statuses.insert(
                            ChangeSetAttributeValue {
                                change_set_pk,
                                attribute_value_id: *attribute_value_id,
                            },
                            AttributeValueStatus::Pending { client: *client_id },
                        );
                    }
                }
                // Collection of interested clients is empty, so value is an orphan
                None => {
                    orphan_attribute_value_ids.insert(*attribute_value_id);
                }
            }
        }

        // Prune any orphan attribute value ids from the change set graph by mutation.
        for orphan_attribute_value_id in orphan_attribute_value_ids.iter() {
            // Remove the map entry if one exists for the orphan
            change_set_graph_data.remove(orphan_attribute_value_id);

            // Remove any occurances of the orphan in the "depends on" collections for all
            // entries.
            //
            // Again, there is no client to process the orphan value so there is no reason
            // to wait on the orphan value.
            for (_, data_entry) in change_set_graph_data.iter_mut() {
                data_entry
                    .depends_on_values
                    .retain(|attribute_value_id| attribute_value_id != orphan_attribute_value_id);
            }

            // Mark this attribute value as orphaned so clients will be notified that no
            // further processing will be attempted. Ideally, no clients will be interested
            // in this status as no clients have expressed interest in this attribute
            // value. In this way, consider orphaned as a delete tombstone.
            next_attribute_value_statuses.insert(
                ChangeSetAttributeValue {
                    change_set_pk,
                    attribute_value_id: *orphan_attribute_value_id,
                },
                AttributeValueStatus::Orphaned,
            );
        }

        // If there were any orphans then we have mutated the change set graph data and we
        // will update it in the kv store.
        if !orphan_attribute_value_ids.is_empty() {
            state
                .update_change_set_graph(change_set_pk, &change_set_graph_data, revision)
                .await?;
        }

        if change_set_graph_data.is_empty() {
            cleanup_change_set_statuses.insert(change_set_pk);
        } else {
            active_change_set_statuses.insert(change_set_pk);
        }
    }

    for active_change_set_pk in active_change_set_statuses {
        status.mark_change_set_active(active_change_set_pk).await?;
    }

    // Publish next statuses for all attribute values, each to their own subject.
    for (key, next_status) in next_attribute_value_statuses {
        status
            .set_attribute_value_status_if_updated(
                key.change_set_pk,
                key.attribute_value_id,
                next_status,
            )
            .await?;
    }

    for cleanup_change_set_pk in cleanup_change_set_statuses {
        status
            .cleanup_change_set_statuses(cleanup_change_set_pk)
            .await?;
    }

    Ok(())
}

async fn set_attribute_value_processed(
    state: &StateStore,
    status: &StatusStore,
    change_set_pk: ChangeSetPk,
    processed_attribute_value_id: AttributeValueId,
    client_id: ClientId,
) -> Result<()> {
    let mut cleanup_change_set_statuses = false;

    if let Some((mut change_set_graph_data, revision)) =
        state.get_change_set_graph(change_set_pk).await?
    {
        // Remove any occurances of the processed attribute value in the "depends on"
        // collection for all entries
        for (_, data_entry) in change_set_graph_data.iter_mut() {
            data_entry
                .depends_on_values
                .retain(|attribute_value_id| attribute_value_id != &processed_attribute_value_id);
        }

        // Remove the entry
        change_set_graph_data.remove(&processed_attribute_value_id);

        state
            .update_change_set_graph(change_set_pk, &change_set_graph_data, revision)
            .await?;

        if change_set_graph_data.is_empty() {
            cleanup_change_set_statuses = true;
        }
    }

    status
        .set_attribute_value_status_if_updated(
            change_set_pk,
            processed_attribute_value_id,
            AttributeValueStatus::Available { client: client_id },
        )
        .await?;

    if cleanup_change_set_statuses {
        status.cleanup_change_set_statuses(change_set_pk).await?;
    }

    // Now that we've processed a value we need to look for more values that are ready to
    // process.
    find_ready_to_process(state, status).await?;

    Ok(())
}

async fn handle_attribute_value_process_failed(
    state: &StateStore,
    status: &StatusStore,
    change_set_pk: ChangeSetPk,
    failed_attribute_value_id: AttributeValueId,
    attribute_value_max_attempts: AttributeValueMaxAttempts,
) -> Result<()> {
    let mut next_status = None;

    if let Some((mut change_set_graph_data, revision)) =
        state.get_change_set_graph(change_set_pk).await?
    {
        let mut prune_value = false;

        if let Some(data_entry) = change_set_graph_data.get_mut(&failed_attribute_value_id) {
            if data_entry.failed_attempts < attribute_value_max_attempts.0.saturating_sub(1) {
                // There are still re-processing attempts left

                // Rotate the responsible client by moving the current client id to the back of the
                // fifo collection. Note that there may be only 1 client, in which case the net
                // result is no change and the *same* client will attempt to process again.
                if let Some(demoted_client_id) = data_entry.interested_clients_fifo.pop_front() {
                    data_entry
                        .interested_clients_fifo
                        .push_back(demoted_client_id);
                }

                // Increase the failed count. We'll use a saturating add on the off chance we
                // overflow the data type
                data_entry.failed_attempts = data_entry.failed_attempts.saturating_add(1);

                if let Some(next_client_id) = data_entry.interested_clients_fifo.front() {
                    // Set up the next attribute value status by setting it back to "pending" to
                    // trigger another processing attempt
                    next_status = Some(NextStatus {
                        change_set_pk,
                        attribute_value_id: failed_attribute_value_id,
                        status: AttributeValueStatus::Pending {
                            client: *next_client_id,
                        },
                    });
                } else {
                    // This scenario is almost certainly a logic bug, kisses, Fletcher ;)
                    warn!(
                        %change_set_pk,
                        attribute_value_id = %failed_attribute_value_id,
                        "no interested clients when handling a failed attribute value attempt",
                    );
                }
            } else {
                // We're tried enough times

                // TODO(fnichol): unclear whether this should be logging and at such a high level,
                // but in council's world this is an expected state in its machine. However this is
                // signalling danger, thus the log event.
                warn!(
                    %change_set_pk,
                    attribute_value_id = %failed_attribute_value_id,
                    max_attempts = attribute_value_max_attempts.0,
                    "attribute value failed after max attempts, value will be removed from graph",
                );

                // Mark this value for pruning from the graph as we can't any further progress
                prune_value = true;

                if let Some(next_client_id) = data_entry.interested_clients_fifo.front() {
                    // Set up the next attribute value status by setting it as failed so every
                    // client knows this value will never make further progress
                    next_status = Some(NextStatus {
                        change_set_pk,
                        attribute_value_id: failed_attribute_value_id,
                        status: AttributeValueStatus::Failed {
                            client: *next_client_id,
                        },
                    });
                } else {
                    // This scenario is almost certainly a logic bug, kisses, Fletcher ;)
                    warn!(
                        %change_set_pk,
                        attribute_value_id = %failed_attribute_value_id,
                        "no interested clients when marking a failed attribute value",
                    );
                }
            }
        }

        if prune_value {
            // Remove the map entry if one exists for the failed value
            change_set_graph_data.remove(&failed_attribute_value_id);

            // Remove any occurances of the failed value in the "depends on" collection for all
            // entries.
            for (_, data_entry) in change_set_graph_data.iter_mut() {
                data_entry
                    .depends_on_values
                    .retain(|attribute_value_id| attribute_value_id != &failed_attribute_value_id);
            }
        }

        // Save the change set graph state
        state
            .update_change_set_graph(change_set_pk, &change_set_graph_data, revision)
            .await?;
    }

    // If there's a next status then set it
    if let Some(next_status) = next_status {
        status
            .set_attribute_value_status_if_updated(
                next_status.change_set_pk,
                next_status.attribute_value_id,
                next_status.status,
            )
            .await?;
    }

    // Now that we've handled a value we need to look for more values that are ready to
    // process.
    find_ready_to_process(state, status).await?;

    Ok(())
}

async fn remove_client_from_change_set(
    state: &StateStore,
    status: &StatusStore,
    change_set_pk: ChangeSetPk,
    removed_client_id: ClientId,
) -> Result<()> {
    if let Some((mut change_set_graph_data, revision)) =
        state.get_change_set_graph(change_set_pk).await?
    {
        // Remove any occurances of the removed client in the "interested clients" collection
        // for all entries.
        for (_, data_entry) in change_set_graph_data.iter_mut() {
            data_entry
                .interested_clients_fifo
                .retain(|client_id| client_id != &removed_client_id);
        }

        state
            .update_change_set_graph(change_set_pk, &change_set_graph_data, revision)
            .await?;
    }

    // Note that if the client was first in the list, then it would have been responsible
    // for processing the associated attribute value. So the appropriate next step is to
    // run the "find ready to process" activity which handles orphan attribute values,
    // promoting a new interested client for attribute value processing, etc.
    find_ready_to_process(state, status).await?;

    Ok(())
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct ChangeSetAttributeValue {
    change_set_pk: ChangeSetPk,
    attribute_value_id: AttributeValueId,
}

#[derive(Clone, Debug, Hash, Eq, PartialEq)]
struct NextStatus {
    change_set_pk: ChangeSetPk,
    attribute_value_id: AttributeValueId,
    status: AttributeValueStatus,
}
