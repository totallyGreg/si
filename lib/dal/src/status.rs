//! Status system that can send real time updates for a multi-step activity to external consumers,
//! such as the web frontend.

#![warn(missing_docs, clippy::missing_errors_doc, clippy::missing_panics_doc)]

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};
use si_data_pg::{PgError, PgPoolError};
use telemetry::prelude::*;
use thiserror::Error;

use crate::{
    pk, standard_model, AttributeValue, AttributeValueError, AttributeValueId, ComponentId,
    DalContext, ExternalProvider, ExternalProviderError, InternalProvider, InternalProviderError,
    Prop, PropError, PropId, SchemaVariant, SocketId, StandardModel, StandardModelError, Timestamp,
    WriteTenancy, WsEvent, WsEventError, WsPayload,
};

const MODEL_TABLE: &str = "status_updates";

const UPDATE_DATA: &str = include_str!("./queries/status_update_update_data.sql");

/// A possible error that can be returned when working with a [`StatusUpdate`].
#[derive(Error, Debug)]
pub enum StatusUpdateError {
    /// When an attibute value metadata entry is not found
    #[error("attribute value metadata not found for: {0}")]
    AttributeValueMetadataNotFound(AttributeValueId),
    /// When a pg error is returned
    #[error("pg error: {0}")]
    Pg(#[from] PgError),
    /// When a pg pool error is returned
    #[error("pg pool error: {0}")]
    PgPool(#[source] Box<PgPoolError>),
    /// When a JSON serialize/deserialize error is returned
    #[error("error serializing/deserializing json: {0}")]
    SerdeJson(#[from] serde_json::Error),
    /// When a standard model error is returned
    #[error("standard model error: {0}")]
    StandardModelError(#[from] StandardModelError),
}

impl From<PgPoolError> for StatusUpdateError {
    fn from(value: PgPoolError) -> Self {
        Self::PgPool(Box::new(value))
    }
}

/// A useful [`Result`] alias when working with a [`StatusUpdate`].
pub type StatusUpdateResult<T> = Result<T, StatusUpdateError>;

pk!(
    /// A primary key for a [`StatusUpdate`].
    StatusUpdatePk
);

/// The internal state data of a [`StatusUpdate`].
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StatusUpdateData {
    // TODO: add change set id
    attribute_value_id: AttributeValueId,
    dependent_values_metadata: HashMap<AttributeValueId, AttributeValueMetadata>,
    queued_dependent_value_ids: HashSet<AttributeValueId>,
    running_dependent_value_ids: HashSet<AttributeValueId>,
    completed_dependent_value_ids: HashSet<AttributeValueId>,
}

impl postgres_types::ToSql for StatusUpdateData {
    fn to_sql(
        &self,
        ty: &postgres_types::Type,
        out: &mut postgres_types::private::BytesMut,
    ) -> Result<postgres_types::IsNull, Box<dyn std::error::Error + Sync + Send>>
    where
        Self: Sized,
    {
        let json = serde_json::to_value(self)?;
        postgres_types::ToSql::to_sql(&json, ty, out)
    }

    fn accepts(ty: &postgres_types::Type) -> bool
    where
        Self: Sized,
    {
        ty == &postgres_types::Type::JSONB
    }

    fn to_sql_checked(
        &self,
        ty: &postgres_types::Type,
        out: &mut postgres_types::private::BytesMut,
    ) -> Result<postgres_types::IsNull, Box<dyn std::error::Error + Sync + Send>> {
        let json = serde_json::to_value(self)?;
        postgres_types::ToSql::to_sql(&json, ty, out)
    }
}

/// A `StatusUpdate` tracks the progress of a complex event which has more than one phase or step.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct StatusUpdate {
    pk: StatusUpdatePk,
    #[serde(flatten)]
    tenancy: WriteTenancy,
    #[serde(flatten)]
    timestamp: Timestamp,
    data: StatusUpdateData,
}

impl StatusUpdate {
    /// Creates and persists a new initialized `StatusUpdate`.
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if the datastore is unable to persist the new object.
    pub async fn new(
        ctx: &DalContext,
        attribute_value_id: AttributeValueId,
    ) -> StatusUpdateResult<Self> {
        let row = ctx
            .pg_pool()
            .get()
            .await?
            .query_one(
                "SELECT object FROM status_update_create_v1($1, $2)",
                &[&attribute_value_id, ctx.write_tenancy()],
            )
            .await?;
        let json: serde_json::Value = row.try_get("object")?;
        let object: Self = serde_json::from_value(json)?;
        Ok(object)
    }

    /// Fetches and returns a `StatusUpdate` by its primary key.
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if there is a connection issue or if the object was not found.
    pub async fn get_by_pk(ctx: &DalContext, pk: StatusUpdatePk) -> StatusUpdateResult<Self> {
        standard_model::get_by_pk(ctx, MODEL_TABLE, &pk)
            .await
            .map_err(Into::into)
    }

    /// Returns the initial [`AttributeValueId`].
    pub fn attribute_value_id(&self) -> AttributeValueId {
        self.data.attribute_value_id
    }

    /// Returns a map of all attribute value metadata, keyed by [`AttributeValueId`].
    pub fn dependent_values_metadata(&self) -> &HashMap<AttributeValueId, AttributeValueMetadata> {
        &self.data.dependent_values_metadata
    }

    /// Sets all dependent value metadata information and persists the update.
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if there is a connection issue or if the update fails.
    pub async fn set_dependent_values_metadata(
        &mut self,
        ctx: &DalContext,
        dependent_values_metadata: HashMap<AttributeValueId, AttributeValueMetadata>,
    ) -> StatusUpdateResult<()> {
        self.data.dependent_values_metadata = dependent_values_metadata;
        self.persist_data_to_db(ctx).await
    }

    /// Returns a set of all currently queued [`AttributeValueId`s](AttributeValueId).
    pub fn queued_dependent_value_ids(&self) -> &HashSet<AttributeValueId> {
        &self.data.queued_dependent_value_ids
    }

    /// Sets all queued dependent value ids and persists the update.
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if there is a connection issue or if the update fails.
    pub async fn set_queued_dependent_value_ids(
        &mut self,
        ctx: &DalContext,
        queued_dependent_value_ids: HashSet<AttributeValueId>,
    ) -> StatusUpdateResult<Vec<AttributeValueMetadata>> {
        self.data.queued_dependent_value_ids = queued_dependent_value_ids;
        self.persist_data_to_db(ctx).await?;

        self.metadata_from_value_ids(&self.data.queued_dependent_value_ids)
    }

    /// Returns a set of all currently running [`AttributeValueId`s](AttributeValueId).
    pub fn running_dependent_value_ids(&self) -> &HashSet<AttributeValueId> {
        &self.data.running_dependent_value_ids
    }

    /// Sets all running dependent value ids and persists the update.
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if there is a connection issue or if the update fails.
    pub async fn set_running_dependent_value_ids(
        &mut self,
        ctx: &DalContext,
        running_dependent_value_ids: Vec<AttributeValueId>,
    ) -> StatusUpdateResult<Vec<AttributeValueMetadata>> {
        for value_id in running_dependent_value_ids.iter() {
            self.data.queued_dependent_value_ids.remove(value_id);
        }
        self.data.running_dependent_value_ids = running_dependent_value_ids.into_iter().collect();
        self.persist_data_to_db(ctx).await?;

        self.metadata_from_value_ids(&self.data.running_dependent_value_ids)
    }

    /// Returns a set of all currently completed [`AttributeValueId`s](AttributeValueId).
    pub fn completed_dependent_value_ids(&self) -> &HashSet<AttributeValueId> {
        &self.data.completed_dependent_value_ids
    }

    /// Sets all completed dependent value ids and persists the update.
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if there is a connection issue or if the update fails.
    pub async fn set_completed_dependent_value_ids(
        &mut self,
        ctx: &DalContext,
        completed_dependent_value_ids: Vec<AttributeValueId>,
    ) -> StatusUpdateResult<Vec<AttributeValueMetadata>> {
        for value_id in completed_dependent_value_ids.iter() {
            self.data.running_dependent_value_ids.remove(value_id);
        }
        self.data.completed_dependent_value_ids =
            completed_dependent_value_ids.into_iter().collect();
        self.persist_data_to_db(ctx).await?;

        self.metadata_from_value_ids(&self.data.completed_dependent_value_ids)
    }

    async fn persist_data_to_db(&mut self, ctx: &DalContext) -> StatusUpdateResult<()> {
        let row = ctx
            .pg_pool()
            .get()
            .await?
            .query_one(UPDATE_DATA, &[&self.pk, &self.data])
            .await?;
        let updated_at = row.try_get("updated_at").map_err(|_| {
            StandardModelError::ModelMissing(MODEL_TABLE.to_string(), self.pk.to_string())
        })?;
        self.timestamp.updated_at = updated_at;

        Ok(())
    }

    fn metadata_from_value_ids(
        &self,
        value_ids: &HashSet<AttributeValueId>,
    ) -> StatusUpdateResult<Vec<AttributeValueMetadata>> {
        value_ids
            .iter()
            .map(|id| {
                self.dependent_values_metadata()
                    .get(id)
                    .ok_or(StatusUpdateError::AttributeValueMetadataNotFound(*id))
                    .map(|e| *e)
            })
            .collect::<Result<Vec<_>, _>>()
    }
}

/// The state of a status update message.
#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StatusMessageState {
    /// A message which has newly queued entries
    Queued,
    /// A message which has newly running entries
    Running,
    /// A message which has newly completed entries
    Completed,
}

/// A status message which encapsulates a new status for some subset of entries.
#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StatusMessage {
    status: StatusMessageState,
    values: Vec<AttributeValueMetadata>,
}

/// A representation of the kind of attribute value that is being processed.
#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq, Copy, Hash)]
#[serde(rename_all = "camelCase", tag = "kind", content = "id")]
enum AttributeValueKind {
    /// Represents a raw attribute value with associated `PropId`
    Attribute(PropId),
    /// Represents a value resulting from a code generation function
    CodeGen,
    /// Represents a value resulting from a qualification function
    Qualification,
    /// Represents a value that is internal to a component
    Internal,
    /// Represents a value used as an input socket with associated `SocketId`
    InputSocket(SocketId),
    /// Represents a value used as an output socket with associated `SocketId`
    OutputSocket(SocketId),
}

/// A computed set of metadata relating to an [`AttributeValue`].
#[derive(Deserialize, Serialize, Debug, Clone, Copy, Eq, Hash, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct AttributeValueMetadata {
    value_id: AttributeValueId,
    component_id: ComponentId,
    value_kind: AttributeValueKind,
}

impl AttributeValueMetadata {
    fn new(
        value_id: AttributeValueId,
        component_id: ComponentId,
        value_kind: AttributeValueKind,
    ) -> Self {
        Self {
            value_id,
            component_id,
            value_kind,
        }
    }
}

/// A possible error that can be returned when working with a [`StatusUpdater`].
#[derive(Debug, Error)]
pub enum StatusUpdaterError {
    /// When an attribute value metadata fails to be created
    #[error("attribute value metadata error {0}")]
    AttributeValueMetadata(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    /// When a realtime update fails to send
    #[error("error publishing realtime update")]
    PublishRealtime(#[from] WsEventError),
    /// When a status update error is returned
    #[error(transparent)]
    StatusUpdate(#[from] StatusUpdateError),
    /// When there are unprocessed values remaining once an update has completed
    #[error("unprocessed values remain upon completion: {0:?}")]
    UnprocessedValuesRemaining(Vec<AttributeValueId>),
}

impl StatusUpdaterError {
    /// Creates a new `StatusUpdateError` when creating an [`AttributeValueMetadata`].
    pub fn metadata(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::AttributeValueMetadata(Box::new(source))
    }
}

/// Tracks and maintains the persisted and realtime state of a [`StatusUpdate`].
#[derive(Clone, Debug)]
pub struct StatusUpdater {
    model: StatusUpdate,
}

impl StatusUpdater {
    /// Intializes a `StatusUpdater` with an underlying [`StatusUpdate`].
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if the datastore is unable to persist the new object.
    pub async fn initialize(
        ctx: &DalContext,
        attribute_value_id: AttributeValueId,
    ) -> Result<Self, StatusUpdaterError> {
        // TODO(fnichol): send a message?

        let model = StatusUpdate::new(ctx, attribute_value_id).await?;
        Ok(Self { model })
    }

    /// Updates the [`StatusUpdate`] with a new set of queued values, represented with their
    /// [`AttributeValueId`s](AttributeValueId).
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if the datastore is unable to update the status update.
    pub async fn values_queued(
        &mut self,
        ctx: &DalContext,
        value_ids: Vec<AttributeValueId>,
    ) -> Result<(), StatusUpdaterError> {
        let now = std::time::Instant::now();

        let mut dependent_values_metadata: HashMap<AttributeValueId, AttributeValueMetadata> =
            HashMap::new();

        for value_id in value_ids {
            let attribute_value = AttributeValue::get_by_id(ctx, &value_id)
                .await
                .map_err(StatusUpdaterError::metadata)?
                .ok_or_else(|| {
                    StatusUpdaterError::metadata(AttributeValueError::NotFound(
                        value_id,
                        *ctx.visibility(),
                    ))
                })?;
            let component_id = attribute_value.context.component_id();

            let mut value_kind;

            // does this value look like an output socket?
            if attribute_value
                .context
                .is_least_specific_field_kind_external_provider()
                .map_err(StatusUpdaterError::metadata)?
            {
                let external_provider = ExternalProvider::get_by_id(
                    ctx,
                    &attribute_value.context.external_provider_id(),
                )
                .await
                .map_err(StatusUpdaterError::metadata)?
                .ok_or_else(|| {
                    StatusUpdaterError::metadata(ExternalProviderError::NotFound(
                        attribute_value.context.external_provider_id(),
                    ))
                })?;
                let socket = external_provider
                    .sockets(ctx)
                    .await
                    .map_err(StatusUpdaterError::metadata)?
                    .pop()
                    .expect("no sockets in vec");
                value_kind = AttributeValueKind::OutputSocket(*socket.id());

            // does this value look like an input socket?
            } else if attribute_value
                .context
                .is_least_specific_field_kind_internal_provider()
                .map_err(StatusUpdaterError::metadata)?
            {
                let internal_provider = InternalProvider::get_by_id(
                    ctx,
                    &attribute_value.context.internal_provider_id(),
                )
                .await
                .map_err(StatusUpdaterError::metadata)?
                .ok_or_else(|| {
                    StatusUpdaterError::metadata(InternalProviderError::NotFound(
                        attribute_value.context.internal_provider_id(),
                    ))
                })?;
                if internal_provider.prop_id().is_none() {
                    let socket = internal_provider
                        .sockets(ctx)
                        .await
                        .map_err(StatusUpdaterError::metadata)?
                        .pop()
                        .expect("no sockets in vec");
                    value_kind = AttributeValueKind::InputSocket(*socket.id());
                } else {
                    value_kind = AttributeValueKind::Internal;
                }

            // does this value correspond to a code generation function?
            } else if attribute_value.context.prop_id().is_some() {
                value_kind = AttributeValueKind::Attribute(attribute_value.context.prop_id());

                let root_prop =
                    SchemaVariant::root_prop(ctx, attribute_value.context.schema_variant_id())
                        .await
                        .map_err(StatusUpdaterError::metadata)?;
                let prop = Prop::get_by_id(ctx, &attribute_value.context.prop_id())
                    .await
                    .map_err(StatusUpdaterError::metadata)?
                    .ok_or_else(|| {
                        StatusUpdaterError::metadata(PropError::NotFound(
                            attribute_value.context.prop_id(),
                            *ctx.visibility(),
                        ))
                    })?;
                if let Some(parent_prop) = prop
                    .parent_prop(ctx)
                    .await
                    .map_err(StatusUpdaterError::metadata)?
                {
                    if let Some(grandparent_prop) = parent_prop
                        .parent_prop(ctx)
                        .await
                        .map_err(StatusUpdaterError::metadata)?
                    {
                        if grandparent_prop.id() == &root_prop.code_prop_id {
                            value_kind = AttributeValueKind::CodeGen;
                        }
                    }
                }
            } else {
                unreachable!("unexpectedly found a value that is not internal but has no prop id")
            }

            dependent_values_metadata.insert(
                value_id,
                AttributeValueMetadata::new(value_id, component_id, value_kind),
            );
        }

        self.model
            .set_dependent_values_metadata(ctx, dependent_values_metadata)
            .await?;

        let queued_values = self
            .model
            .set_queued_dependent_value_ids(
                ctx,
                self.model
                    .dependent_values_metadata()
                    .keys()
                    .copied()
                    .collect(),
            )
            .await?;

        let elapsed_time = now.elapsed();
        info!("StatusUpdater.values_queued took {:?}", elapsed_time);

        WsEvent::status_update(
            ctx,
            StatusMessageState::Queued,
            queued_values.into_iter().collect(),
        )
        .publish_immediately(ctx)
        .await?;

        Ok(())
    }

    /// Updates the [`StatusUpdate`] with a new set of running values, represented with their
    /// [`AttributeValueId`s](AttributeValueId).
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if the datastore is unable to update the status update.
    pub async fn values_running(
        &mut self,
        ctx: &DalContext,
        value_ids: Vec<AttributeValueId>,
    ) -> Result<(), StatusUpdaterError> {
        let running_values = self
            .model
            .set_running_dependent_value_ids(ctx, value_ids)
            .await?;

        WsEvent::status_update(ctx, StatusMessageState::Running, running_values)
            .publish_immediately(ctx)
            .await?;

        Ok(())
    }

    /// Updates the [`StatusUpdate`] with a new set of completed values, represented with their
    /// [`AttributeValueId`s](AttributeValueId).
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if the datastore is unable to update the status update.
    pub async fn values_completed(
        &mut self,
        ctx: &DalContext,
        value_ids: Vec<AttributeValueId>,
    ) -> Result<(), StatusUpdaterError> {
        let finished_values = self
            .model
            .set_completed_dependent_value_ids(ctx, value_ids)
            .await?;

        WsEvent::status_update(ctx, StatusMessageState::Completed, finished_values)
            .publish_immediately(ctx)
            .await?;

        Ok(())
    }

    /// Finalizes the update, ensuring that there are no unprocessed values.
    ///
    /// # Errors
    ///
    /// Returns [`Err`] if there are unprocessed values.
    pub fn update_complete(self) -> Result<(), StatusUpdaterError> {
        let all_value_ids = self
            .model
            .dependent_values_metadata()
            .keys()
            .copied()
            .collect::<HashSet<AttributeValueId>>();
        let completed_value_ids = self.model.completed_dependent_value_ids();
        if &all_value_ids != completed_value_ids {
            return Err(StatusUpdaterError::UnprocessedValuesRemaining(
                all_value_ids
                    .difference(completed_value_ids)
                    .copied()
                    .collect(),
            ));
        }

        if !self.model.queued_dependent_value_ids().is_empty() {
            return Err(StatusUpdaterError::UnprocessedValuesRemaining(
                self.model
                    .queued_dependent_value_ids()
                    .iter()
                    .copied()
                    .collect(),
            ));
        }
        if !self.model.running_dependent_value_ids().is_empty() {
            return Err(StatusUpdaterError::UnprocessedValuesRemaining(
                self.model
                    .running_dependent_value_ids()
                    .iter()
                    .copied()
                    .collect(),
            ));
        }

        Ok(())
    }
}

impl WsEvent {
    /// Creates a new `WsEvent` for a [`StatusUpdate`].
    pub fn status_update(
        ctx: &DalContext,
        status: StatusMessageState,
        values: Vec<AttributeValueMetadata>,
    ) -> Self {
        WsEvent::new(
            ctx,
            WsPayload::StatusUpdate(StatusMessage { status, values }),
        )
    }
}
