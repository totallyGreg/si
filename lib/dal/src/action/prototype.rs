use petgraph::{Direction::Incoming, Outgoing};
use serde::{Deserialize, Serialize};
use si_pkg::ActionFuncSpecKind;
use strum::Display;
use thiserror::Error;
use veritech_client::OutputStream;

use crate::{
    diagram::{DiagramError, SummaryDiagramComponent},
    func::{
        backend::js_action::DeprecatedActionRunResult,
        binding::{return_value::FuncBindingReturnValueError, FuncBinding, FuncBindingError},
        execution::FuncExecutionPk,
    },
    implement_add_edge_to,
    secret::{before_funcs_for_component, BeforeFuncError},
    workspace_snapshot::node_weight::{ActionPrototypeNodeWeight, NodeWeight, NodeWeightError},
    ActionPrototypeId, ChangeSetError, Component, ComponentError, ComponentId, DalContext,
    EdgeWeightError, EdgeWeightKind, EdgeWeightKindDiscriminants, FuncId, HelperError,
    SchemaVariant, SchemaVariantError, SchemaVariantId, TransactionsError, WorkspaceSnapshotError,
    WsEvent, WsEventError,
};

#[remain::sorted]
#[derive(Debug, Error)]
pub enum ActionPrototypeError {
    #[error("before func error: {0}")]
    BeforeFunc(#[from] BeforeFuncError),
    #[error("Change Set error: {0}")]
    ChangeSet(#[from] ChangeSetError),
    #[error("component error: {0}")]
    Component(#[from] ComponentError),
    #[error("diagram error: {0}")]
    Diagram(#[from] DiagramError),
    #[error("Edge Weight error: {0}")]
    EdgeWeight(#[from] EdgeWeightError),
    #[error("func binding error: {0}")]
    FuncBinding(#[from] FuncBindingError),
    #[error("func binding return value error: {0}")]
    FuncBindingReturnValue(#[from] FuncBindingReturnValueError),
    #[error("func not found for prototype: {0}")]
    FuncNotFoundForPrototype(ActionPrototypeId),
    #[error("Helper error: {0}")]
    Helper(#[from] HelperError),
    #[error("Node Weight error: {0}")]
    NodeWeight(#[from] NodeWeightError),
    #[error("schema variant error: {0}")]
    SchemaVariant(#[from] SchemaVariantError),
    #[error("schema variant not found for prototype: {0}")]
    SchemaVariantFoundForPrototype(ActionPrototypeId),
    #[error("serde json error: {0}")]
    SerdeJson(#[from] serde_json::Error),
    #[error("Transactions error: {0}")]
    Transactions(#[from] TransactionsError),
    #[error("Workspace Snapshot error: {0}")]
    WorkspaceSnapshot(#[from] WorkspaceSnapshotError),
    #[error("ws event error: {0}")]
    WsEvent(#[from] WsEventError),
}

pub type ActionPrototypeResult<T> = Result<T, ActionPrototypeError>;

#[remain::sorted]
#[derive(Debug, Copy, Clone, Deserialize, Serialize, PartialEq, Eq, Display)]
pub enum ActionKind {
    /// Create the "outside world" version of the modeled object.
    Create,
    /// Destroy the "outside world" version of the modeled object referenced in the resource.
    Destroy,
    /// This [`Action`][crate::Action] will only ever be manually queued.
    Manual,
    /// Refresh the resource to reflect the current state of the modeled object in the "outside
    /// world".
    Refresh,
    /// Update the version of the modeled object in the "outside world" to match the state of the
    /// model.
    Update,
}

impl From<ActionFuncSpecKind> for ActionKind {
    fn from(value: ActionFuncSpecKind) -> Self {
        match value {
            ActionFuncSpecKind::Create => ActionKind::Create,
            ActionFuncSpecKind::Refresh => ActionKind::Refresh,
            ActionFuncSpecKind::Other => ActionKind::Manual,
            ActionFuncSpecKind::Delete => ActionKind::Destroy,
            ActionFuncSpecKind::Update => ActionKind::Update,
        }
    }
}

impl From<ActionKind> for ActionFuncSpecKind {
    fn from(value: ActionKind) -> Self {
        match value {
            ActionKind::Create => ActionFuncSpecKind::Create,
            ActionKind::Destroy => ActionFuncSpecKind::Delete,
            ActionKind::Manual => ActionFuncSpecKind::Other,
            ActionKind::Refresh => ActionFuncSpecKind::Refresh,
            ActionKind::Update => ActionFuncSpecKind::Update,
        }
    }
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ActionPrototype {
    pub id: ActionPrototypeId,
    pub kind: ActionKind,
    pub name: String,
    pub description: Option<String>,
}

impl From<ActionPrototypeNodeWeight> for ActionPrototype {
    fn from(value: ActionPrototypeNodeWeight) -> Self {
        Self {
            id: value.id().into(),
            kind: value.kind(),
            name: value.name().to_owned(),
            description: value.description().map(str::to_string),
        }
    }
}

impl ActionPrototype {
    pub fn id(&self) -> ActionPrototypeId {
        self.id
    }

    pub async fn new(
        ctx: &DalContext,
        kind: ActionKind,
        name: String,
        description: Option<String>,
        schema_variant_id: SchemaVariantId,
        func_id: FuncId,
    ) -> ActionPrototypeResult<Self> {
        let change_set = ctx.change_set()?;
        let new_id: ActionPrototypeId = change_set.generate_ulid()?.into();
        let node_weight =
            NodeWeight::new_action_prototype(change_set, new_id.into(), kind, name, description)?;
        ctx.workspace_snapshot()?.add_node(node_weight).await?;

        Self::add_edge_to_func(ctx, new_id, func_id, EdgeWeightKind::new_use()).await?;

        SchemaVariant::add_edge_to_action_prototype(
            ctx,
            schema_variant_id,
            new_id,
            EdgeWeightKind::ActionPrototype,
        )
        .await?;

        let new_prototype: Self = ctx
            .workspace_snapshot()?
            .get_node_weight_by_id(new_id)
            .await?
            .get_action_prototype_node_weight()?
            .into();

        Ok(new_prototype)
    }

    pub fn name(&self) -> &String {
        &self.name
    }

    pub fn description(&self) -> &Option<String> {
        &self.description
    }

    implement_add_edge_to!(
        source_id: ActionPrototypeId,
        destination_id: FuncId,
        add_fn: add_edge_to_func,
        discriminant: EdgeWeightKindDiscriminants::Use,
        result: ActionPrototypeResult,
    );

    pub async fn get_by_id(ctx: &DalContext, id: ActionPrototypeId) -> ActionPrototypeResult<Self> {
        let prototype: Self = ctx
            .workspace_snapshot()?
            .get_node_weight_by_id(id)
            .await?
            .get_action_prototype_node_weight()?
            .into();
        Ok(prototype)
    }

    pub async fn func_id(ctx: &DalContext, id: ActionPrototypeId) -> ActionPrototypeResult<FuncId> {
        for (_, _tail_node_idx, head_node_idx) in ctx
            .workspace_snapshot()?
            .edges_directed_for_edge_weight_kind(id, Outgoing, EdgeWeightKindDiscriminants::Use)
            .await?
        {
            if let NodeWeight::Func(node_weight) = ctx
                .workspace_snapshot()?
                .get_node_weight(head_node_idx)
                .await?
            {
                return Ok(node_weight.id().into());
            }
        }

        Err(ActionPrototypeError::FuncNotFoundForPrototype(id))
    }

    async fn schema_variant_id(
        ctx: &DalContext,
        id: ActionPrototypeId,
    ) -> ActionPrototypeResult<SchemaVariantId> {
        for (_, tail_node_idx, _head_node_idx) in ctx
            .workspace_snapshot()?
            .edges_directed_for_edge_weight_kind(
                id,
                Incoming,
                EdgeWeightKindDiscriminants::ActionPrototype,
            )
            .await?
        {
            if let NodeWeight::Content(node_weight) = ctx
                .workspace_snapshot()?
                .get_node_weight(tail_node_idx)
                .await?
            {
                return Ok(node_weight.id().into());
            }
        }
        Err(ActionPrototypeError::SchemaVariantFoundForPrototype(id))
    }

    pub async fn run(
        ctx: &DalContext,
        id: ActionPrototypeId,
        component_id: ComponentId,
    ) -> ActionPrototypeResult<(
        FuncExecutionPk,
        Vec<OutputStream>,
        Option<DeprecatedActionRunResult>,
    )> {
        let component = Component::get_by_id(ctx, component_id).await?;
        let component_view = component.view(ctx).await?;

        let before = before_funcs_for_component(ctx, component_id).await?;

        let (_, return_value) = FuncBinding::create_and_execute(
            ctx,
            serde_json::json!({ "properties" : component_view }),
            Self::func_id(ctx, id).await?,
            before,
        )
        .await?;

        let func_execution_pk = return_value.func_execution_pk();

        let mut logs = vec![];
        for stream_part in return_value
            .get_output_stream(ctx)
            .await?
            .unwrap_or_default()
        {
            logs.push(stream_part);
        }

        logs.sort_by_key(|log| log.timestamp);

        let value = match return_value.value() {
            Some(value) => {
                let run_result: DeprecatedActionRunResult = serde_json::from_value(value.clone())?;
                component.set_resource(ctx, run_result.clone()).await?;

                let payload: SummaryDiagramComponent =
                    SummaryDiagramComponent::assemble(ctx, &component).await?;
                WsEvent::resource_refreshed(ctx, payload)
                    .await?
                    .publish_on_commit(ctx)
                    .await?;

                Some(run_result)
            }
            None => None,
        };
        Ok((func_execution_pk, logs, value))
    }

    pub async fn for_variant(
        ctx: &DalContext,
        schema_variant_id: SchemaVariantId,
    ) -> ActionPrototypeResult<Vec<Self>> {
        let mut prototypes = Vec::new();
        for (_, _tail_node_idx, head_node_idx) in ctx
            .workspace_snapshot()?
            .edges_directed_for_edge_weight_kind(
                schema_variant_id,
                Outgoing,
                EdgeWeightKindDiscriminants::ActionPrototype,
            )
            .await?
        {
            if let NodeWeight::ActionPrototype(node_weight) = ctx
                .workspace_snapshot()?
                .get_node_weight(head_node_idx)
                .await?
            {
                prototypes.push(node_weight.into());
            }
        }

        Ok(prototypes)
    }

    pub async fn get_prototypes_to_trigger(
        ctx: &DalContext,
        id: ActionPrototypeId,
    ) -> ActionPrototypeResult<Vec<ActionPrototypeId>> {
        // for now we are only defaulting to triggering a
        // refresh when a create action succeeds
        // in the future, this will be configurable and we'll look up edges
        let mut triggered_actions = vec![];
        let action_prototype = Self::get_by_id(ctx, id).await?;
        if action_prototype.kind == ActionKind::Create {
            // find refresh func for schema variant
            let schema_variant_id = Self::schema_variant_id(ctx, id).await?;
            let prototypes = Self::for_variant(ctx, schema_variant_id).await?;
            for prototype in prototypes {
                if prototype.kind == ActionKind::Refresh {
                    triggered_actions.push(prototype.id());
                }
            }
        }
        Ok(triggered_actions)
    }
    pub async fn remove(ctx: &DalContext, id: ActionPrototypeId) -> ActionPrototypeResult<()> {
        let change_set = ctx.change_set()?;

        ctx.workspace_snapshot()?
            .remove_node_by_id(change_set, id)
            .await?;

        Ok(())
    }
}
