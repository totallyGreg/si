use std::{
    collections::HashMap,
    pin::Pin,
    str::FromStr,
    sync::Arc,
    task::{Context, Poll},
};

use council_core::{
    council_incoming_stream, council_kv, subject, AttributeValueStatus, ClientUpdateRequest,
    DependencyGraphRequest, OperationEntry,
};
use futures::{Future, Stream, StreamExt};
use parking_lot::Mutex;
use si_data_nats::{
    async_nats::jetstream::{
        self,
        consumer::{pull::MessagesError, StreamError},
        context::CreateKeyValueError,
        kv::{Operation, WatchError, WatcherError},
        stream::ConsumerError,
    },
    NatsClient,
};
use telemetry::prelude::*;
use thiserror::Error;

pub use council_core::{AttributeValueId, ChangeSetPk, ClientId, DependencyGraph};

#[remain::sorted]
#[derive(Debug, Error)]
pub enum ClientError {
    #[error("stream consumer error: {0}")]
    Consumer(#[from] ConsumerError),
    #[error("consumer stream error: {0}")]
    ConsumerStream(#[from] StreamError),
    #[error("kv create error: {0}")]
    CreateKeyValue(#[from] CreateKeyValueError),
    #[error("failed to get or create stream: {0}")]
    CreateStream(#[from] jetstream::context::CreateStreamError),
    #[error("failed deserialize entry: {0}")]
    Deserialize(#[source] serde_json::Error),
    #[error("kv watch error: {0}")]
    KvWatch(#[from] WatchError),
    #[error("kv watcher error: {0}")]
    KvWatcher(#[from] WatcherError),
    #[error("messages error: {0}")]
    Messages(#[from] MessagesError),
    #[error("failed to publish message: {0}")]
    Publish(#[source] jetstream::context::PublishError),
    #[error("failed to serialze: {0}")]
    Serialize(#[source] serde_json::Error),
}

type Result<T> = std::result::Result<T, ClientError>;

#[derive(Debug, Clone)]
pub struct Client {
    subject_prefix: Arc<Option<String>>,
    jetstream: jetstream::Context,
    kv: jetstream::kv::Store,
}

impl Client {
    pub async fn from_services(nats: NatsClient, subject_prefix: Option<String>) -> Result<Self> {
        let jetstream = jetstream::new(nats.as_inner().clone());
        // Ensure that the incoming stream is created
        let _ = council_incoming_stream(&jetstream, subject_prefix.as_deref()).await?;

        let kv = council_kv(&jetstream, subject_prefix.as_deref()).await?;

        Ok(Self {
            subject_prefix: Arc::new(subject_prefix),
            jetstream,
            kv,
        })
    }

    pub async fn collaborate_on_change_set(
        &self,
        change_set_pk: impl Into<ChangeSetPk>,
        dependency_graph: impl Into<DependencyGraph>,
    ) -> Result<ChangeSetCollaboration> {
        let change_set_pk = change_set_pk.into();
        let client_id = ClientId::default();

        let subject = subject::incoming_for_change_set(self.prefix(), change_set_pk);

        let payload = {
            let request = DependencyGraphRequest {
                client_id,
                graph: dependency_graph.into(),
            };
            let payload = serde_json::to_vec(&request).map_err(ClientError::Serialize)?;

            // TODO(fnichol): this can be removed when the jetstream clients are instrumented
            debug!(
                messaging.destination.name = %subject,
                messaging.operation = MessagingOperation::Publish.as_str(),
                ?request,
            );

            payload
        };

        self.jetstream
            .publish(subject, payload.into())
            .await
            .map_err(ClientError::Publish)?
            .await
            .map_err(ClientError::Publish)?;

        Ok(ChangeSetCollaboration {
            acker: Acker {
                prefix: self.subject_prefix.clone(),
                client_id,
                change_set_pk,
                jetstream: self.jetstream.clone(),
                seen: SeenMap::default(),
            },
            kv: self.kv.clone(),
        })
    }

    #[inline]
    fn prefix(&self) -> Option<&str> {
        self.subject_prefix.as_deref()
    }
}

#[derive(Debug)]
pub struct ChangeSetCollaboration {
    acker: Acker,
    kv: jetstream::kv::Store,
}

impl ChangeSetCollaboration {
    pub fn id(&self) -> ClientId {
        self.acker.client_id
    }

    pub fn acker(&self) -> &Acker {
        &self.acker
    }

    pub fn clone_acker(&self) -> Acker {
        self.acker.clone()
    }

    pub async fn statuses(&self) -> Result<Statuses> {
        let subjects = subject::key::status::change_set_statuses(self.acker.change_set_pk);

        debug!(
            si.council.client.id = %self.acker.client_id,
            si.change_set.pk = %self.acker.change_set_pk,
            subjects = subjects.as_str(),
            "watching kv store",
        );

        let watch = self.kv.watch_with_history(subjects).await?;

        Ok(Statuses {
            acker: self.acker.clone(),
            watch,
        })
    }

    pub async fn shutdown(self) -> Result<()> {
        let request = ClientUpdateRequest::Shutdown {
            change_set_pk: self.acker.change_set_pk,
        };
        send_update(
            &self.acker.jetstream,
            self.acker.prefix.as_deref(),
            self.acker.client_id,
            &request,
        )
        .await
    }
}

impl Drop for ChangeSetCollaboration {
    fn drop(&mut self) {
        let request = ClientUpdateRequest::Shutdown {
            change_set_pk: self.acker.change_set_pk,
        };
        let jetstream = self.acker.jetstream.clone();
        let prefix = self.acker.prefix.clone();
        let client_id = self.acker.client_id;

        trace!(
            si.council.client.id = %self.acker.client_id,
            si.change_set.pk = %self.acker.change_set_pk,
            "dropping ChangeSetCollaboration",
        );

        tokio::spawn(async move {
            if let Err(err) = send_update(&jetstream, prefix.as_deref(), client_id, &request).await
            {
                warn!(error = ?err, "failed to shutdown change set collaboration client");
            }
        });
    }
}

#[remain::sorted]
#[derive(Debug)]
pub enum Status {
    Available(AttributeValueId),
    Failed(AttributeValueId),
    Orphaned(AttributeValueId),
    Process(AttributeValueId),
    ProcessingByAnother(AttributeValueId, ClientId),
}

pub struct Statuses<'a> {
    acker: Acker,
    watch: jetstream::kv::Watch<'a>,
}

impl<'a> Statuses<'a> {
    #[inline]
    pub async fn ack_pending(&self, attribute_value_id: impl Into<AttributeValueId>) -> Result<()> {
        self.acker.ack_pending(attribute_value_id).await
    }

    #[inline]
    pub async fn ack_processed(
        &self,
        attribute_value_id: impl Into<AttributeValueId>,
    ) -> Result<()> {
        self.acker.ack_processed(attribute_value_id).await
    }

    #[inline]
    pub async fn nack_processed(
        &self,
        attribute_value_id: impl Into<AttributeValueId>,
    ) -> Result<()> {
        self.acker.nack_processed(attribute_value_id).await
    }

    pub async fn shutdown(self) -> Result<()> {
        let request = ClientUpdateRequest::Shutdown {
            change_set_pk: self.acker.change_set_pk,
        };
        send_update(
            &self.acker.jetstream,
            self.acker.prefix.as_deref(),
            self.acker.client_id,
            &request,
        )
        .await
    }

    fn process_attribute_value_status_entry(
        &self,
        entry: OperationEntry,
        attribute_value_id: AttributeValueId,
    ) -> Option<Poll<Option<Result<Status>>>> {
        let my_client_id = self.acker.client_id;

        match entry {
            OperationEntry::Put(entry) => {
                let attribute_value_status: AttributeValueStatus =
                    match serde_json::from_slice(&entry.value).map_err(ClientError::Deserialize) {
                        Ok(val) => val,
                        Err(err) => return Some(Poll::Ready(Some(Err(err)))),
                    };

                trace!(
                    si.council.client.id = %self.acker.client_id,
                    si.change_set.pk = %self.acker.change_set_pk,
                    si.attribute_value.id = %attribute_value_id,
                    status = ?attribute_value_status,
                );

                match attribute_value_status {
                    AttributeValueStatus::Pending { client } => {
                        if client == my_client_id
                            && entry.operation == Operation::Put
                            && self.acker.seen.is_not(attribute_value_id, Seen::Pending)
                        {
                            self.acker.seen.set(attribute_value_id, Seen::Pending);

                            debug!(
                                si.council.client.id = %self.acker.client_id,
                                si.change_set.pk = %self.acker.change_set_pk,
                                si.attribute_value.id = %attribute_value_id,
                                status = "process",
                                "stream returns status",
                            );

                            return Some(Poll::Ready(Some(Ok(Status::Process(
                                attribute_value_id,
                            )))));
                        }
                    }
                    AttributeValueStatus::Available { .. } => {
                        if entry.operation == Operation::Put
                            && self.acker.seen.is_not(attribute_value_id, Seen::Available)
                        {
                            self.acker.seen.set(attribute_value_id, Seen::Available);

                            debug!(
                                si.council.client.id = %self.acker.client_id,
                                si.change_set.pk = %self.acker.change_set_pk,
                                si.attribute_value.id = %attribute_value_id,
                                status = "available",
                                "stream returns status",
                            );

                            return Some(Poll::Ready(Some(Ok(Status::Available(
                                attribute_value_id,
                            )))));
                        }
                    }
                    AttributeValueStatus::Failed { .. } => {
                        if entry.operation == Operation::Put
                            && self.acker.seen.is_not(attribute_value_id, Seen::Failed)
                        {
                            self.acker.seen.set(attribute_value_id, Seen::Failed);

                            debug!(
                                si.council.client.id = %self.acker.client_id,
                                si.change_set.pk = %self.acker.change_set_pk,
                                si.attribute_value.id = %attribute_value_id,
                                status = "failed",
                                "stream returns status",
                            );

                            return Some(Poll::Ready(Some(Ok(Status::Failed(attribute_value_id)))));
                        }
                    }
                    AttributeValueStatus::Orphaned => {
                        if entry.operation == Operation::Put
                            && self.acker.seen.is_not(attribute_value_id, Seen::Orphaned)
                        {
                            self.acker.seen.set(attribute_value_id, Seen::Orphaned);

                            debug!(
                                si.council.client.id = %self.acker.client_id,
                                si.change_set.pk = %self.acker.change_set_pk,
                                si.attribute_value.id = %attribute_value_id,
                                status = "orphaned",
                                "stream returns status",
                            );

                            return Some(Poll::Ready(Some(Ok(Status::Orphaned(
                                attribute_value_id,
                            )))));
                        }
                    }
                    AttributeValueStatus::Processing { client } => {
                        if client != my_client_id
                            && entry.operation == Operation::Put
                            && self
                                .acker
                                .seen
                                .is_not_with(attribute_value_id, |val| match val {
                                    Seen::ProcessingByAnother(seen_client) => {
                                        *seen_client == client
                                    }
                                    _ => false,
                                })
                        {
                            self.acker
                                .seen
                                .set(attribute_value_id, Seen::ProcessingByAnother(client));

                            debug!(
                                si.council.client.id = %self.acker.client_id,
                                si.change_set.pk = %self.acker.change_set_pk,
                                si.attribute_value.id = %attribute_value_id,
                                status = "processedbyanother",
                                "stream returns status",
                            );

                            return Some(Poll::Ready(Some(Ok(Status::ProcessingByAnother(
                                attribute_value_id,
                                client,
                            )))));
                        }
                    }
                }
            }
            OperationEntry::Delete(_) => {
                trace!(
                    si.change_set.pk = %self.acker.change_set_pk,
                    si.attribute.value.id = %attribute_value_id,
                    si.council.client.id = %my_client_id,
                    "saw deleted attribute value status",
                );
            }
            OperationEntry::Purge(_) => {
                trace!(
                    si.change_set.pk = %self.acker.change_set_pk,
                    si.attribute.value.id = %attribute_value_id,
                    si.council.client.id = %my_client_id,
                    "saw purged attribute value status",
                );
            }
        }

        None
    }

    fn process_change_set_status_entry(
        &self,
        entry: OperationEntry,
    ) -> Option<Poll<Option<Result<Status>>>> {
        match entry {
            OperationEntry::Delete(_) | OperationEntry::Purge(_) => {
                debug!(
                    si.council.client.id = %self.acker.client_id,
                    si.change_set.pk = %self.acker.change_set_pk,
                    status = "closed",
                    "stream is finished",
                );

                // This is the clean terminating condition of the collaboration, so
                // close the stream
                Some(Poll::Ready(None))
            }
            OperationEntry::Put(_) => {
                trace!(
                    si.council.client.id = %self.acker.client_id,
                    si.change_set.pk = %self.acker.change_set_pk,
                    "saw active change set status",
                );

                None
            }
        }
    }
}

impl<'a> Stream for Statuses<'a> {
    type Item = Result<Status>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        loop {
            let poll = Pin::new(&mut self.watch.next()).poll(cx);

            match poll {
                // We successfully got a kv entry
                Poll::Ready(Some(Ok(entry))) => {
                    // Convert into an `OperationEntry` to guarentee the Op is considered
                    let entry = OperationEntry::from(entry);

                    let mut key_parts = entry.key().split('.').skip(3);
                    match (key_parts.next(), key_parts.next(), key_parts.next()) {
                        // av status
                        (Some("av"), Some(attribute_value_id_str), None) => {
                            let attribute_value_id =
                                AttributeValueId::from_str(attribute_value_id_str)
                                    .expect("TODO: av parse");

                            if let Some(poll) =
                                self.process_attribute_value_status_entry(entry, attribute_value_id)
                            {
                                return poll;
                            }
                        }
                        // cs status
                        (Some("active"), None, None) => {
                            if let Some(poll) = self.process_change_set_status_entry(entry) {
                                return poll;
                            }
                        }
                        _ => todo!("TODO: invalid key"),
                    }
                }
                Poll::Ready(Some(Err(_err))) => todo!(),
                // If the upstream closes, then we do too
                Poll::Ready(None) => return Poll::Ready(None),
                // Not ready, so...not ready!
                Poll::Pending => return Poll::Pending,
            }
        }
    }
}

#[derive(Clone, Debug)]
pub struct Acker {
    prefix: Arc<Option<String>>,
    client_id: ClientId,
    change_set_pk: ChangeSetPk,
    jetstream: jetstream::Context,
    seen: SeenMap,
}

impl Acker {
    pub fn id(&self) -> ClientId {
        self.client_id
    }

    pub async fn ack_pending(&self, attribute_value_id: impl Into<AttributeValueId>) -> Result<()> {
        let attribute_value_id = attribute_value_id.into();
        let request = ClientUpdateRequest::AckPending {
            change_set_pk: self.change_set_pk,
            attribute_value_id,
        };
        send_update(
            &self.jetstream,
            self.prefix.as_deref(),
            self.client_id,
            &request,
        )
        .await
    }

    pub async fn ack_processed(
        &self,
        attribute_value_id: impl Into<AttributeValueId>,
    ) -> Result<()> {
        let attribute_value_id = attribute_value_id.into();
        let request = ClientUpdateRequest::AckProcessed {
            change_set_pk: self.change_set_pk,
            attribute_value_id,
        };
        send_update(
            &self.jetstream,
            self.prefix.as_deref(),
            self.client_id,
            &request,
        )
        .await
    }

    pub async fn nack_processed(
        &self,
        attribute_value_id: impl Into<AttributeValueId>,
    ) -> Result<()> {
        let attribute_value_id = attribute_value_id.into();
        let request = ClientUpdateRequest::NackProcessed {
            change_set_pk: self.change_set_pk,
            attribute_value_id,
        };
        self.seen.forget(attribute_value_id);
        send_update(
            &self.jetstream,
            self.prefix.as_deref(),
            self.client_id,
            &request,
        )
        .await
    }
}

async fn send_update(
    jetstream: &jetstream::context::Context,
    prefix: Option<&str>,
    client_id: ClientId,
    request: &ClientUpdateRequest,
) -> Result<()> {
    let payload = serde_json::to_vec(request).map_err(ClientError::Serialize)?;
    let subject = subject::incoming_for_client(prefix, client_id);

    // TODO(fnichol): this can be removed when the jetstream clients are instrumented
    debug!(
        messaging.destination.name = %subject,
        messaging.operation = MessagingOperation::Publish.as_str(),
        ?request,
    );

    jetstream
        .publish(subject, payload.into())
        .await
        .map_err(ClientError::Publish)?
        .await
        .map_err(ClientError::Publish)?;

    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Seen {
    Pending,
    ProcessingByAnother(ClientId),
    Available,
    Failed,
    Orphaned,
}

#[derive(Clone, Debug, Default)]
struct SeenMap {
    inner: Arc<Mutex<HashMap<AttributeValueId, Seen>>>,
}

impl SeenMap {
    fn set(&self, av: AttributeValueId, seen: Seen) {
        self.inner.lock().insert(av, seen);
    }

    fn forget(&self, av: AttributeValueId) {
        self.inner.lock().remove(&av);
    }

    fn is_not(&self, av: AttributeValueId, seen: Seen) -> bool {
        self.is_not_with(av, |val| *val == seen)
    }

    fn is_not_with<F>(&self, av: AttributeValueId, f: F) -> bool
    where
        F: FnOnce(&Seen) -> bool,
    {
        !self.inner.lock().get(&av).is_some_and(f)
    }
}
