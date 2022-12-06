use std::{collections::HashMap, fmt, sync::Arc};

use serde::{Deserialize, Serialize};
use telemetry::prelude::*;
use thiserror::Error;
use tokio::sync::Mutex;
use uuid::Uuid;

use crate::{
    AttributeValue, AttributeValueError, AttributeValueId, ComponentId, DalContext,
    ExternalProvider, ExternalProviderError, InternalProvider, InternalProviderError, Prop,
    PropError, PropId, SchemaVariant, SocketId, StandardModel, WsEvent, WsEventError, WsPayload,
};

#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct StatusUpdate {
    status: StatusState,
    values: Vec<AttributeValueMetadata>,
}

#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub enum StatusState {
    Queued,
    Running,
    Completed,
}

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

impl WsEvent {
    pub fn status_update(
        ctx: &DalContext,
        status: StatusState,
        values: Vec<AttributeValueMetadata>,
    ) -> Self {
        WsEvent::new(
            ctx,
            WsPayload::StatusUpdate(StatusUpdate { status, values }),
        )
    }
}

#[derive(Clone, Deserialize, Serialize, Debug, PartialEq, Eq, Copy, Hash)]
#[serde(rename_all = "camelCase", tag = "kind", content = "id")]
enum AttributeValueKind {
    Attribute(PropId),
    CodeGen,
    Qualification,
    Internal,
    InputSocket(SocketId),
    OutputSocket(SocketId),
}

#[derive(Debug, Error)]
pub enum StatusUpdaterError {
    #[error("attribute value metadata error {0}")]
    AttributeValueMetadata(#[source] Box<dyn std::error::Error + Send + Sync + 'static>),
    #[error("error publishing realtime update")]
    PublishRealtime(#[from] WsEventError),
    #[error("status update id not tracked: {0}")]
    StatusUpdateNotTracked(StatusUpdateId),
    #[error("unprocessed values remain upon completion: {0:?}")]
    UnprocessedValuesRemaining(Vec<AttributeValueId>),
    #[error("values already queued for status upadte id: {0}")]
    ValuesAlreadyQueued(StatusUpdateId),
}

impl StatusUpdaterError {
    pub fn metadata(source: impl std::error::Error + Send + Sync + 'static) -> Self {
        Self::AttributeValueMetadata(Box::new(source))
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct StatusUpdateId(Uuid);

impl fmt::Display for StatusUpdateId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Clone, Debug)]
pub struct StatusUpdater {
    status_updates:
        Arc<Mutex<HashMap<StatusUpdateId, HashMap<AttributeValueId, AttributeValueMetadata>>>>,
}

impl Default for StatusUpdater {
    fn default() -> Self {
        Self::new()
    }
}

impl StatusUpdater {
    pub fn new() -> Self {
        Self {
            status_updates: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn initialize(
        &self,
        _attribute_value_id: AttributeValueId,
    ) -> Result<StatusUpdateId, StatusUpdaterError> {
        // TODO(fnichol): send a message?

        Ok(StatusUpdateId(Uuid::new_v4()))
    }

    pub async fn values_queued(
        &self,
        ctx: &DalContext,
        id: StatusUpdateId,
        value_ids: Vec<AttributeValueId>,
    ) -> Result<(), StatusUpdaterError> {
        if self.status_updates.lock().await.contains_key(&id) {
            return Err(StatusUpdaterError::ValuesAlreadyQueued(id));
        }

        let now = std::time::Instant::now();

        let mut values_to_components: HashMap<AttributeValueId, AttributeValueMetadata> =
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

            values_to_components.insert(
                value_id,
                AttributeValueMetadata::new(value_id, component_id, value_kind),
            );
        }

        let elapsed_time = now.elapsed();
        info!("StatusUpdater.values_queued took {:?}", elapsed_time);

        let queued_values = values_to_components.values().copied().collect();

        self.status_updates
            .lock()
            .await
            .insert(id, values_to_components);

        WsEvent::status_update(ctx, StatusState::Queued, queued_values)
            .publish_immediately(ctx)
            .await?;

        Ok(())
    }

    pub async fn values_running(
        &self,
        ctx: &DalContext,
        id: StatusUpdateId,
        value_ids: Vec<AttributeValueId>,
    ) -> Result<(), StatusUpdaterError> {
        let running_values = {
            let status_updates = self.status_updates.lock().await;

            let values_to_components = match status_updates.get(&id) {
                Some(hash) => hash,
                None => return Err(StatusUpdaterError::StatusUpdateNotTracked(id)),
            };

            let mut running_values = Vec::new();
            for value_id in value_ids {
                running_values.push(
                    *values_to_components
                        .get(&value_id)
                        // All attribute values have been iterated their component ids should be in
                        // the hashmap, otherwise this is a programmer bug!
                        .expect("component id not found for value id"),
                );
            }
            running_values
        };

        WsEvent::status_update(ctx, StatusState::Running, running_values)
            .publish_immediately(ctx)
            .await?;

        Ok(())
    }

    pub async fn values_completed(
        &self,
        ctx: &DalContext,
        id: StatusUpdateId,
        value_ids: Vec<AttributeValueId>,
    ) -> Result<(), StatusUpdaterError> {
        let finished_values = {
            let mut status_updates = self.status_updates.lock().await;

            let values_to_components = match status_updates.get_mut(&id) {
                Some(hash) => hash,
                None => return Err(StatusUpdaterError::StatusUpdateNotTracked(id)),
            };

            let mut finished_values = Vec::new();
            for value_id in value_ids {
                finished_values.push(
                    values_to_components
                        .remove(&value_id)
                        // All attribute values have been iterated their component ids should be in
                        // the hashmap, otherwise this is a programmer bug!
                        .expect("component id not found for value id"),
                );
            }
            finished_values
        };

        WsEvent::status_update(ctx, StatusState::Completed, finished_values)
            .publish_immediately(ctx)
            .await?;

        Ok(())
    }

    pub async fn update_complete(&self, id: StatusUpdateId) -> Result<(), StatusUpdaterError> {
        let values_to_components = match self.status_updates.lock().await.remove(&id) {
            Some(hash) => hash,
            None => return Err(StatusUpdaterError::StatusUpdateNotTracked(id)),
        };

        if !values_to_components.is_empty() {
            return Err(StatusUpdaterError::UnprocessedValuesRemaining(
                values_to_components.keys().copied().collect(),
            ));
        }

        Ok(())
    }
}
