use std::{
    collections::{HashMap, VecDeque},
    ops, result,
    str::{self, FromStr},
};

use council_core::{
    subject, AttributeValueId, AttributeValueStatus, ChangeSetPk, ClientId, OperationEntry,
};
use futures::TryStreamExt;
use serde::{Deserialize, Serialize};
use si_data_nats::async_nats::jetstream;
use telemetry::prelude::*;
use thiserror::Error;

#[remain::sorted]
#[derive(Debug, Error)]
pub enum KvStoreError {
    #[error("error deserializing kv value: {0}")]
    Deserialize(#[source] serde_json::Error),
    #[error("entry error: {0}")]
    Entry(#[from] jetstream::kv::EntryError),
    #[error("history error: {0}")]
    History(#[source] jetstream::kv::WatchError),
    #[error("purging key '{0}' with expected revision {1}, but server reported revision {2}")]
    PurgeRevision(String, u64, u64),
    #[error("put error: {0}")]
    Put(#[from] jetstream::kv::PutError),
    #[error("error serializing kv value: {0}")]
    Serialize(#[source] serde_json::Error),
    #[error("failed to decode ulid: {0}")]
    UlidDecode(#[from] ulid::DecodeError),
    #[error("update error: {0}")]
    Update(#[from] jetstream::kv::UpdateError),
    #[error("watcher error: {0}")]
    Watcher(#[source] jetstream::kv::WatcherError),
}

pub type Result<T> = result::Result<T, KvStoreError>;

#[derive(Clone, Debug)]
pub struct StateStore {
    inner: jetstream::kv::Store,
}

impl StateStore {
    pub fn new(inner: jetstream::kv::Store) -> Self {
        Self { inner }
    }

    pub async fn get_or_create_change_set_graph(
        &self,
        change_set_pk: ChangeSetPk,
    ) -> Result<Option<(DependencyGraphData, KvRevision)>> {
        match self
            .inner
            .entry(subject::key::state::change_set(change_set_pk))
            .await?
            // Convert into an `OperationEntry` to guarentee the Op is considered
            .map(OperationEntry::from)
        {
            Some(entry) => {
                match entry {
                    // Value is set
                    OperationEntry::Put(entry) => {
                        let change_set_graph_data: DependencyGraphData =
                            serde_json::from_slice(&entry.value)
                                .map_err(KvStoreError::Deserialize)?;
                        Ok(Some((change_set_graph_data, KvRevision(entry.revision))))
                    }
                    // Otherwise value has been deleted/purged and we must return the revision as
                    // it will be greater than `0`
                    OperationEntry::Delete(entry) | OperationEntry::Purge(entry) => Ok(Some((
                        DependencyGraphData::default(),
                        KvRevision(entry.revision),
                    ))),
                }
            }
            None => Ok(None),
        }
    }

    /// Fetches and returns a [`DependencyGraphData`] for a given change set.
    pub async fn get_change_set_graph(
        &self,
        change_set_pk: ChangeSetPk,
    ) -> Result<Option<(DependencyGraphData, KvRevision)>> {
        match self
            .inner
            .entry(subject::key::state::change_set(change_set_pk))
            .await?
            // Convert into an `OperationEntry` to guarentee the Op is considered
            .map(OperationEntry::from)
        {
            Some(entry) => {
                match entry {
                    // Value is set
                    OperationEntry::Put(entry) => {
                        let change_set_graph_data: DependencyGraphData =
                            serde_json::from_slice(&entry.value)
                                .map_err(KvStoreError::Deserialize)?;
                        Ok(Some((change_set_graph_data, KvRevision(entry.revision))))
                    }
                    // Otherwise value has been deleted/purged and so we return `None`
                    OperationEntry::Delete(_) | OperationEntry::Purge(_) => Ok(None),
                }
            }
            None => Ok(None),
        }
    }

    pub async fn update_change_set_graph(
        &self,
        change_set_pk: ChangeSetPk,
        updated: &DependencyGraphData,
        revision: KvRevision,
    ) -> Result<()> {
        let key = subject::key::state::change_set(change_set_pk);

        if updated.is_empty() {
            // Read the value from the store just before purging to ensure that our revision
            // number matches that of the store.
            //
            // Note that if there was a `kv.purge_for_revision(key, revision)` method, this
            // would be used instead.
            match self.inner.entry(&key).await? {
                Some(entry) => {
                    // Persisted value has a different revision so we should fail to purge and
                    // try again
                    if entry.revision != revision.0 {
                        return Err(KvStoreError::PurgeRevision(key, revision.0, entry.revision));
                    }
                }
                None => {
                    debug!(
                        key = &key,
                        "value not present for key while attempting to purge"
                    );
                    return Ok(());
                }
            }

            debug!(key = &key, "purging empty graph data");

            self.inner.purge(key).await?;
        } else {
            let updated_bytes = serde_json::to_vec(updated).map_err(KvStoreError::Serialize)?;

            debug!(
                key = &key,
                value = str::from_utf8(&updated_bytes).unwrap_or("<invalid>"),
                "updating change set graph data",
            );

            self.inner
                .update(
                    subject::key::state::change_set(change_set_pk),
                    updated_bytes.into(),
                    revision.0,
                )
                .await?;
        }

        Ok(())
    }

    pub async fn change_set_pks_from_keys(&self) -> Result<Vec<ChangeSetPk>> {
        let key_prefix = format!("{}.", subject::key::state::STATE_CS_PREFIX);

        // Fetch the current collection of all change sets in the kv store under state keys
        let change_set_pks = self
            .inner
            .keys()
            .await
            .map_err(KvStoreError::History)?
            .try_collect::<Vec<_>>()
            .await
            .map_err(KvStoreError::Watcher)?
            .into_iter()
            .filter_map(|key_name| {
                key_name
                    .strip_prefix(&key_prefix)
                    .map(|id_str| ChangeSetPk::from_str(id_str).map_err(Into::into))
            })
            .collect::<Result<Vec<_>>>()?;

        Ok(change_set_pks)
    }
}

#[derive(Clone, Debug)]
pub struct StatusStore {
    inner: jetstream::kv::Store,
}

impl StatusStore {
    pub fn new(inner: jetstream::kv::Store) -> Self {
        Self { inner }
    }

    pub async fn attribute_value_status_keys_from_status_keys_by_change_set(
        &self,
        change_set_pk: ChangeSetPk,
    ) -> Result<Vec<String>> {
        let key_prefix = format!(
            "{}.{change_set_pk}.av.",
            subject::key::status::STATUS_CS_PREFIX
        );

        // Fetch the current collection of all attribute value status subjects in the kv store
        // under status keys
        let key_names = self
            .inner
            .keys()
            .await
            .map_err(KvStoreError::History)?
            .try_collect::<Vec<_>>()
            .await
            .map_err(KvStoreError::Watcher)?
            .into_iter()
            .filter(|key_name| key_name.starts_with(&key_prefix))
            .collect::<Vec<_>>();

        Ok(key_names)
    }

    pub async fn set_attribute_value_status_if_updated(
        &self,
        change_set_pk: ChangeSetPk,
        attribute_value_id: AttributeValueId,
        status: AttributeValueStatus,
    ) -> Result<()> {
        if let Some(av_status) = self
            .get_attribute_value_status(change_set_pk, attribute_value_id)
            .await?
        {
            if av_status == status {
                return Ok(());
            }
        }

        self.set_attribute_value_status(change_set_pk, attribute_value_id, status)
            .await
    }

    pub async fn set_attribute_value_processing(
        &self,
        change_set_pk: ChangeSetPk,
        attribute_value_id: AttributeValueId,
        client_id: ClientId,
    ) -> Result<()> {
        self.set_attribute_value_status(
            change_set_pk,
            attribute_value_id,
            AttributeValueStatus::Processing { client: client_id },
        )
        .await
    }

    pub async fn cleanup_change_set_statuses(
        &self,
        cleanup_change_set_pk: ChangeSetPk,
    ) -> Result<()> {
        let key = subject::key::status::change_set_active(cleanup_change_set_pk);
        debug!(key = &key, "purging change set status entry");
        self.inner.purge(key).await?;

        let purge_keys = self
            .attribute_value_status_keys_from_status_keys_by_change_set(cleanup_change_set_pk)
            .await?;

        for purge_key in purge_keys {
            debug!(key = &purge_key, "purging status entry");
            self.inner.purge(purge_key).await?;
        }

        Ok(())
    }

    pub async fn mark_change_set_active(&self, change_set_pk: ChangeSetPk) -> Result<()> {
        self.inner
            .put(
                subject::key::status::change_set_active(change_set_pk),
                Default::default(),
            )
            .await?;

        Ok(())
    }

    async fn get_attribute_value_status(
        &self,
        change_set_pk: ChangeSetPk,
        attribute_value_id: AttributeValueId,
    ) -> Result<Option<AttributeValueStatus>> {
        let maybe = self
            .inner
            .get(subject::key::status::attribute_value(
                change_set_pk,
                attribute_value_id,
            ))
            .await?;

        match maybe {
            Some(bytes) => {
                let s = serde_json::from_slice(&bytes).map_err(KvStoreError::Deserialize)?;
                Ok(s)
            }
            None => Ok(None),
        }
    }

    async fn set_attribute_value_status(
        &self,
        change_set_pk: ChangeSetPk,
        attribute_value_id: AttributeValueId,
        status: AttributeValueStatus,
    ) -> Result<()> {
        let key = subject::key::status::attribute_value(change_set_pk, attribute_value_id);
        let bytes = serde_json::to_vec(&status).map_err(KvStoreError::Serialize)?;

        debug!(
            key = &key,
            value = str::from_utf8(&bytes).unwrap_or("<invalid>"),
            "setting status for attribute value",
        );

        self.inner.put(key, bytes.into()).await?;

        Ok(())
    }
}

impl From<jetstream::kv::Store> for StatusStore {
    fn from(value: jetstream::kv::Store) -> Self {
        Self { inner: value }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct KvRevision(u64);

impl From<u64> for KvRevision {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DependencyGraphData(HashMap<AttributeValueId, DependencyGraphDataEntry>);

impl ops::Deref for DependencyGraphData {
    type Target = HashMap<AttributeValueId, DependencyGraphDataEntry>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl ops::DerefMut for DependencyGraphData {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct DependencyGraphDataEntry {
    // Interested clients as a first-in-first-out queue. In other words:
    //
    // - new interested clients are pushed on the end
    // - next interested client that might perform work is pulled from the front (it is the
    // oldest)
    #[serde(rename = "clients")] // keeps kv data structure names stable
    pub(crate) interested_clients_fifo: VecDeque<ClientId>,
    // Attribute values which must be processed before the associated attribute value can be
    // processed.
    //
    // If the collection is empty, then the associated value is free to be processed
    #[serde(rename = "deps")] // keeps kv data structure names stable
    pub(crate) depends_on_values: Vec<AttributeValueId>,
    // Tracks the number of failed processing attempts have been made for this attribute value.
    //
    // The system will have an upper bound on the number of retry attempts before determining that
    // the attribute value will not make further progress in its current state (that is, the
    // clients available, the state of the network/database/etc.)
    #[serde(rename = "failed")] // keeps kv data structure names stable
    pub(crate) failed_attempts: u16,
}
