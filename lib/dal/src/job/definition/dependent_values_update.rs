use std::{
    collections::{HashMap, HashSet},
    convert::TryFrom,
};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use council_client::{Acker, ChangeSetCollaboration, Client, Status};
use futures::TryStreamExt as _;
use serde::{Deserialize, Serialize};
use telemetry::prelude::*;
use tokio_util::{sync::CancellationToken, task::TaskTracker};

use crate::{
    diagram,
    job::{
        consumer::{
            JobConsumer, JobConsumerError, JobConsumerMetadata, JobConsumerResult, JobInfo,
        },
        producer::{JobProducer, JobProducerResult},
    },
    property_editor,
    tasks::{StatusReceiverClient, StatusReceiverRequest},
    AccessBuilder, AttributeValue, AttributeValueError, AttributeValueId, AttributeValueResult,
    ChangeSetPk, ComponentId, DalContext, FuncBindingReturnValue, InternalProvider, Prop,
    StandardModel, StatusUpdater, Visibility, WsEvent,
};

#[derive(Debug, Deserialize, Serialize)]
struct DependentValuesUpdateArgs {
    attribute_values: Vec<AttributeValueId>,
}

impl From<DependentValuesUpdate> for DependentValuesUpdateArgs {
    fn from(value: DependentValuesUpdate) -> Self {
        Self {
            attribute_values: value.attribute_values,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DependentValuesUpdate {
    attribute_values: Vec<AttributeValueId>,
    access_builder: AccessBuilder,
    visibility: Visibility,
    job: Option<JobInfo>,
}

impl DependentValuesUpdate {
    pub fn new(
        access_builder: AccessBuilder,
        visibility: Visibility,
        attribute_values: Vec<AttributeValueId>,
    ) -> Box<Self> {
        // TODO(nick,paulo,zack,jacob): ensure we do not _have_ to force non deleted visibility in the future.
        let visibility = visibility.to_non_deleted();

        Box::new(Self {
            attribute_values,
            access_builder,
            visibility,
            job: None,
        })
    }

    fn job_id(&self) -> Option<String> {
        self.job.as_ref().map(|j| j.id.clone())
    }
}

impl JobProducer for DependentValuesUpdate {
    fn arg(&self) -> JobProducerResult<serde_json::Value> {
        Ok(serde_json::to_value(DependentValuesUpdateArgs::from(
            self.clone(),
        ))?)
    }
}

impl JobConsumerMetadata for DependentValuesUpdate {
    fn type_name(&self) -> String {
        "DependentValuesUpdate".to_string()
    }

    fn access_builder(&self) -> AccessBuilder {
        self.access_builder
    }

    fn visibility(&self) -> Visibility {
        self.visibility
    }
}

#[async_trait]
impl JobConsumer for DependentValuesUpdate {
    #[instrument(
        name = "dependent_values_update.run",
        skip_all,
        level = "info",
        fields(
            si.workspace.pk = Empty,
            si.change_set.pk = %self.visibility().change_set_pk,
            si.council.client.id = Empty,
            si.attribute_value.ids = ?self.attribute_values,
        )
    )]
    async fn run(&self, ctx: &mut DalContext) -> JobConsumerResult<()> {
        let span = Span::current();
        if let Some(workspace_pk) = ctx.tenancy().workspace_pk() {
            span.record("si.workspace.pk", workspace_pk.to_string());
        }

        let council_client = {
            let nats_client = ctx.nats_conn().clone();
            let subject_prefix = nats_client
                .metadata()
                .subject_prefix()
                .map(|str| str.to_owned());
            Client::from_services(nats_client, subject_prefix).await?
        };

        // TODO(nick,paulo,zack,jacob): ensure we do not _have_ to do this in the future.
        ctx.update_without_deleted_visibility();

        let mut status_updater = StatusUpdater::initialize(ctx).await;

        let mut dependency_graph =
            AttributeValue::dependent_value_graph(ctx, &self.attribute_values).await?;

        // The dependent_value_graph is read-only, so we can safely rollback the inner txns, to
        // make sure we don't hold open txns unnecessarily
        ctx.rollback().await?;

        // NOTE(nick,jacob): uncomment this for debugging.
        // Save printed output to a file and execute the following: "dot <file> -Tsvg -o <newfile>.svg"
        // println!("{}", dependency_graph_to_dot(ctx, &dependency_graph).await?);

        // Any of our initial inputs that aren't using one of the `si:set*`, or `si:unset`
        // functions need to be evaluated, since we might be populating the initial functions of a
        // `Component` during `Component` creation.
        let avs_with_dynamic_functions: HashSet<AttributeValueId> = HashSet::from_iter(
            AttributeValue::ids_using_dynamic_functions(ctx, &self.attribute_values)
                .await?
                .iter()
                .copied(),
        );
        for id in &self.attribute_values {
            if avs_with_dynamic_functions.contains(id) {
                dependency_graph.entry(*id).or_insert_with(Vec::new);
            }
        }

        debug!(?dependency_graph, "Generated dependency graph");

        if dependency_graph.is_empty() {
            return Ok(());
        }

        // Cache the original dependency graph to send the status receiver.
        let original_dependency_graph = dependency_graph.clone();

        let collaboration = council_client
            .collaborate_on_change_set(
                self.visibility().change_set_pk,
                DependencyGraph(dependency_graph.clone()),
            )
            .await?;

        span.record("si.council.client.id", collaboration.id().to_string());

        let mut enqueued: Vec<AttributeValueId> = dependency_graph.keys().copied().collect();
        enqueued.extend(dependency_graph.values().flatten().copied());
        status_updater.values_queued(ctx, enqueued).await;

        // Status updater reads from the database and uses its own connection from the pg_pool to
        // do writes
        ctx.rollback().await?;

        let res = self
            .inner_run(
                ctx,
                status_updater,
                &collaboration,
                dependency_graph,
                original_dependency_graph,
            )
            .await;

        collaboration.shutdown().await?;

        res
    }
}

impl DependentValuesUpdate {
    async fn inner_run(
        &self,
        ctx: &mut DalContext,
        mut status_updater: StatusUpdater,
        collaboration: &ChangeSetCollaboration,
        mut dependency_graph: HashMap<AttributeValueId, Vec<AttributeValueId>>,
        original_dependency_graph: HashMap<AttributeValueId, Vec<AttributeValueId>>,
    ) -> JobConsumerResult<()> {
        let task_tracker = TaskTracker::new();
        let cancel_token = CancellationToken::new();

        let ctx_builder = ctx.to_builder();

        let mut statuses = collaboration.statuses().await?;
        let acker = collaboration.clone_acker();

        while let Some(status) = statuses.try_next().await? {
            debug!(?status, "incoming status");

            match status {
                // We receive a request for our client to process an attribute value
                Status::Process(attribute_value_id) => {
                    acker.ack_pending(attribute_value_id).await?;

                    status_updater
                        .values_running(ctx, vec![attribute_value_id.into()])
                        .await;
                    // Status updater reads from the database and uses its own connection
                    // from the pg_pool to do writes
                    ctx.rollback().await?;

                    let task_ctx = ctx_builder
                        .build(self.access_builder().build(self.visibility()))
                        .await?;
                    let acker = collaboration.clone_acker();
                    let attribute_value_id = Into::<AttributeValueId>::into(attribute_value_id);
                    let change_set_pk = task_ctx.visibility().change_set_pk;
                    let cancel_token = cancel_token.clone();
                    let client_id = collaboration.id();
                    let this_span = Span::current();

                    task_tracker.spawn(async move {
                        tokio::select! {
                            result = update_value(
                                task_ctx,
                                acker,
                                attribute_value_id,
                                this_span,
                            ) => {
                                if let Err(err) = result {
                                    warn!(
                                        si.council.client.id = %client_id,
                                        si.change_set.pk = %change_set_pk,
                                        si.attribute_value.id = %attribute_value_id,
                                        error = ?err,
                                        "error updating value",
                                    );
                                }
                            }
                            _ = cancel_token.cancelled() => {
                                debug!(
                                    si.council.client.id = %client_id,
                                    si.change_set.pk = %change_set_pk,
                                    si.attribute_value.id = %attribute_value_id,
                                    "cancelled update value",
                                );
                            }
                        }
                    });
                }
                // A status that tells us an attribute value has been processed and can be removed
                // from the dependency graph
                Status::Available(attribute_value_id) => {
                    dependency_graph.remove(&attribute_value_id.into());

                    // Send a completed status for this value and *remove* it from the hash
                    status_updater
                        .values_completed(ctx, vec![attribute_value_id.into()])
                        .await;
                    // Status updater reads from the database and uses its own connection from
                    // the pg_pool to do writes
                    ctx.rollback().await?;
                }
                // A status that tells us an attribute value has failed to be processed and will
                // never be re-attempted in this collaboration.
                Status::Failed(attribute_value_id) => {
                    dependency_graph.remove(&attribute_value_id.into());

                    // Send a completed status for this value and *remove* it from the hash
                    status_updater
                        .values_completed(ctx, vec![attribute_value_id.into()])
                        .await;
                    // Status updater reads from the database and uses its own connection from
                    // the pg_pool to do writes
                    ctx.rollback().await?;
                }
                // A status that tells us an attribute value has been marked orphaned by council
                Status::Orphaned(attribute_value_id) => {
                    if dependency_graph.get(&attribute_value_id.into()).is_some() {
                        warn!(
                            si.council.client.id = %collaboration.id(),
                            si.change_set.pk = %ctx.visibility().change_set_pk,
                            si.attribute_value.id = %attribute_value_id,
                            "council reported orphan, but value is present in local graph",
                        );

                        // Proceed as if "available" or "failed"

                        dependency_graph.remove(&attribute_value_id.into());

                        // Send a completed status for this value and *remove* it from the hash
                        status_updater
                            .values_completed(ctx, vec![attribute_value_id.into()])
                            .await;
                        // Status updater reads from the database and uses its own connection from
                        // the pg_pool to do writes
                        ctx.rollback().await?;
                    }
                }
                // A status that tells us an attribute value is being processed by another client
                Status::ProcessingByAnother(attribute_value_id, client_id) => {
                    trace!(
                        si.council.client.id = %collaboration.id(),
                        si.change_set.pk = %ctx.visibility().change_set_pk,
                        si.attribute_value.id = %attribute_value_id,
                        "value processing by another client ({})",
                        client_id,
                    );
                }
            }
        }

        task_tracker.close();
        task_tracker.wait().await;

        // No matter what, we need to finish the updater
        status_updater.finish(ctx).await;

        let client = StatusReceiverClient::new(ctx.nats_conn().clone()).await;
        if let Err(e) = client
            .publish(&StatusReceiverRequest {
                visibility: *ctx.visibility(),
                tenancy: *ctx.tenancy(),
                dependent_graph: original_dependency_graph,
            })
            .await
        {
            error!("could not publish status receiver request: {:?}", e);
        }

        ctx.commit().await?;

        Ok(())
    }
}

/// Wrapper around `AttributeValue.update_from_prototype_function(&ctx)` to get it to
/// play more nicely with being spawned into a `JoinSet`.
#[instrument(
    name = "dependent_values_update.update_value",
    parent = &parent_span,
    skip_all,
    level = "info",
    fields(
        si.change_set.pk = %ctx.visibility().change_set_pk,
        si.attribute_value.id = %attribute_value_id,
        si.council.client.id = %council_acker.id(),
    )
)]
async fn update_value(
    ctx: DalContext,
    council_acker: Acker,
    attribute_value_id: AttributeValueId,
    parent_span: Span,
) -> JobConsumerResult<()> {
    let mut attribute_value = AttributeValue::get_by_id(&ctx, &attribute_value_id)
        .await?
        .ok_or_else(|| AttributeValueError::NotFound(attribute_value_id, *ctx.visibility()))?;

    let update_result = attribute_value.update_from_prototype_function(&ctx).await;
    // We don't propagate the error up, because we want the rest of the nodes in the graph to make
    // progress if they are able to.
    if update_result.is_err() {
        error!(
            si.attribute_value.id = %attribute_value.id(),
            ?update_result,
            "Error updating AttributeValue",
        );
        council_acker.nack_processed(attribute_value_id).await?;
        ctx.rollback().await?;
    }

    // If this is for an internal provider corresponding to a root prop for the schema variant of
    // an existing component, then we want to update summary tables.
    let value = if let Some(fbrv) =
        FuncBindingReturnValue::get_by_id(&ctx, &attribute_value.func_binding_return_value_id())
            .await?
    {
        if let Some(component_value_json) = fbrv.unprocessed_value() {
            if !attribute_value.context.is_component_unset()
                && !attribute_value.context.is_internal_provider_unset()
                && InternalProvider::is_for_root_prop(
                    &ctx,
                    attribute_value.context.internal_provider_id(),
                )
                .await
                .unwrap()
            {
                update_summary_tables(
                    &ctx,
                    component_value_json,
                    attribute_value.context.component_id(),
                )
                .await?;
            }
            component_value_json.clone()
        } else {
            serde_json::Value::Null
        }
    } else {
        serde_json::Value::Null
    };

    ctx.commit().await?;

    if update_result.is_ok() {
        council_acker.ack_processed(attribute_value_id).await?;
    }

    Prop::run_validation(
        &ctx,
        attribute_value.context.prop_id(),
        attribute_value.context.component_id(),
        attribute_value.key(),
        value,
    )
    .await;

    ctx.commit().await?;

    Ok(())
}

#[instrument(
    name = "dependent_values_update.update_summary_tables",
    skip_all,
    level = "info",
    fields(
        component.id = %component_id,
    )
)]
async fn update_summary_tables(
    ctx: &DalContext,
    component_value_json: &serde_json::Value,
    component_id: ComponentId,
) -> JobConsumerResult<()> {
    // Qualification summary table - if we add more summary tables, this should be extracted to its
    // own method.
    let mut total: i64 = 0;
    let mut warned: i64 = 0;
    let mut succeeded: i64 = 0;
    let mut failed: i64 = 0;
    let mut name: String = String::new();
    let mut color: String = String::new();
    let mut component_type: String = String::new();
    let mut has_resource: bool = false;
    let mut deleted_at: Option<String> = None;
    let mut deleted_at_datetime: Option<DateTime<Utc>> = None;
    if let Some(ref deleted_at) = deleted_at {
        let deleted_at_datetime_inner: DateTime<Utc> = deleted_at.parse()?;
        deleted_at_datetime = Some(deleted_at_datetime_inner);
    }

    if let Some(component_name) = component_value_json.pointer("/si/name") {
        if let Some(component_name_str) = component_name.as_str() {
            name = String::from(component_name_str);
        }
    }

    if let Some(component_color) = component_value_json.pointer("/si/color") {
        if let Some(component_color_str) = component_color.as_str() {
            color = String::from(component_color_str);
        }
    }

    if let Some(component_type_json) = component_value_json.pointer("/si/type") {
        if let Some(component_type_str) = component_type_json.as_str() {
            component_type = String::from(component_type_str);
        }
    }

    if let Some(_resource) = component_value_json.pointer("/resource/payload") {
        has_resource = true;
    }

    if let Some(deleted_at_value) = component_value_json.pointer("/deleted_at") {
        if let Some(deleted_at_str) = deleted_at_value.as_str() {
            deleted_at = Some(deleted_at_str.into());
        }
    }

    if let Some(qualification_map_value) = component_value_json.pointer("/qualification") {
        if let Some(qualification_map) = qualification_map_value.as_object() {
            for qual_result_map_value in qualification_map.values() {
                if let Some(qual_result_map) = qual_result_map_value.as_object() {
                    if let Some(qual_result) = qual_result_map.get("result") {
                        if let Some(qual_result_string) = qual_result.as_str() {
                            total += 1;
                            match qual_result_string {
                                "success" => succeeded += 1,
                                "warning" => warned += 1,
                                "failure" => failed += 1,
                                &_ => (),
                            }
                        }
                    }
                }
            }
        }
    }
    let _row = ctx
        .txns()
        .await?
        .pg()
        .query_one(
            "SELECT object FROM summary_qualification_update_v2($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            &[
                ctx.tenancy(),
                ctx.visibility(),
                &component_id,
                &name,
                &total,
                &warned,
                &succeeded,
                &failed,
                &deleted_at_datetime,
            ],
        )
        .await?;

    diagram::summary_diagram::component_update(
        ctx,
        &component_id,
        name,
        color,
        component_type,
        has_resource,
        deleted_at,
    )
    .await?;

    property_editor::values_summary::PropertyEditorValuesSummary::create_or_update_component_entry(
        ctx,
        component_id,
    )
    .await?;

    WsEvent::component_updated(ctx, component_id)
        .await?
        .publish_on_commit(ctx)
        .await?;

    Ok(())
}

impl TryFrom<JobInfo> for DependentValuesUpdate {
    type Error = JobConsumerError;

    fn try_from(job: JobInfo) -> Result<Self, Self::Error> {
        let args = DependentValuesUpdateArgs::deserialize(&job.arg)?;
        Ok(Self {
            attribute_values: args.attribute_values,
            access_builder: job.access_builder,
            visibility: job.visibility,
            job: Some(job),
        })
    }
}

#[allow(unused)]
async fn dependency_graph_to_dot(
    ctx: &DalContext,
    graph: &HashMap<AttributeValueId, Vec<AttributeValueId>>,
) -> AttributeValueResult<String> {
    let mut node_definitions = String::new();
    for attr_val_id in graph.keys() {
        let attr_val = AttributeValue::get_by_id(ctx, attr_val_id)
            .await?
            .ok_or_else(|| AttributeValueError::NotFound(*attr_val_id, *ctx.visibility()))?;
        let prop_id = attr_val.context.prop_id();
        let internal_provider_id = attr_val.context.internal_provider_id();
        let external_provider_id = attr_val.context.external_provider_id();
        let component_id = attr_val.context.component_id();
        node_definitions.push_str(&format!(
            "\"{attr_val_id}\"[label=\"\\lAttribute Value: {attr_val_id}\\n\\lProp: {prop_id}\\lInternal Provider: {internal_provider_id}\\lExternal Provider: {external_provider_id}\\lComponent: {component_id}\"];",
        ));
    }

    let mut node_graph = String::new();
    for (attr_val, inputs) in graph {
        let dependencies = format!(
            "{{{dep_list}}}",
            dep_list = inputs
                .iter()
                .map(|i| format!("\"{i}\""))
                .collect::<Vec<String>>()
                .join(" ")
        );
        let dependency_line = format!("{dependencies} -> \"{attr_val}\";",);
        node_graph.push_str(&dependency_line);
    }

    let dot_digraph = format!("digraph G {{{node_definitions}{node_graph}}}");

    Ok(dot_digraph)
}

impl From<ChangeSetPk> for council_client::ChangeSetPk {
    fn from(value: ChangeSetPk) -> Self {
        value.into_inner().into()
    }
}

impl From<council_client::ChangeSetPk> for ChangeSetPk {
    fn from(value: council_client::ChangeSetPk) -> Self {
        value.into_inner().into()
    }
}

impl From<AttributeValueId> for council_client::AttributeValueId {
    fn from(value: AttributeValueId) -> Self {
        value.into_inner().into()
    }
}

impl From<council_client::AttributeValueId> for AttributeValueId {
    fn from(value: council_client::AttributeValueId) -> Self {
        value.into_inner().into()
    }
}

struct DependencyGraph(HashMap<AttributeValueId, Vec<AttributeValueId>>);

impl From<DependencyGraph> for council_client::DependencyGraph {
    fn from(value: DependencyGraph) -> Self {
        let map: HashMap<council_client::AttributeValueId, Vec<council_client::AttributeValueId>> =
            value
                .0
                .into_iter()
                .map(|(key, value)| (key.into(), value.into_iter().map(Into::into).collect()))
                .collect();
        map.into()
    }
}
