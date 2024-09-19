use std::collections::HashMap;

use petgraph::prelude::*;
use telemetry::prelude::*;
use thiserror::Error;

use crate::{
    socket::{input::InputSocketError, output::OutputSocketError},
    Component, ComponentError, ComponentId, DalContext, InputSocket, InputSocketId, OutputSocket,
    OutputSocketId, SchemaVariant, SocketArity, WorkspaceSnapshotError,
};

#[remain::sorted]
#[derive(Debug, Error)]
pub enum FlorpError {
    #[error("Component error: {0}")]
    Component(#[from] ComponentError),
    #[error("InputSocket error: {0}")]
    InputSocket(#[from] InputSocketError),
    #[error("Orphaned Component")]
    OrphanedComponent(ComponentId),
    #[error("OutputSocket error: {0}")]
    OutputSocket(#[from] OutputSocketError),
    #[error("WorkspaceSnapshot error: {0}")]
    WorkspaceSnapshot(#[from] WorkspaceSnapshotError),
}

pub type FlorpResult<T> = Result<T, FlorpError>;

#[derive(Debug, Clone)]
pub struct InferredFlorp {
    down_component_graph: StableDiGraph<InferredFlorpNodeWeight, ()>,
    up_component_graph: StableDiGraph<InferredFlorpNodeWeight, ()>,
    index_by_component_id: HashMap<ComponentId, NodeIndex>,
    index_by_input_socket_id: HashMap<InputSocketId, NodeIndex>,
    index_by_output_socket_id: HashMap<OutputSocketId, NodeIndex>,
}

#[derive(Debug, Clone)]
struct InferredFlorpNodeWeight {
    component: Component,
    input_sockets: Vec<InputSocket>,
    output_sockets: Vec<OutputSocket>,
}

#[remain::sorted]
#[derive(Debug, Copy, Clone, Eq, PartialEq)]
pub enum FlorpDirection {
    Down,
    Up,
}

impl InferredFlorp {
    #[instrument(
        name = "component.inferred_connection_graph_new",
        level = "debug",
        skip(ctx)
    )]
    pub async fn new(ctx: &DalContext) -> FlorpResult<Self> {
        let mut down_component_graph = StableDiGraph::new();
        let mut index_by_component_id = HashMap::new();
        let mut index_by_input_socket_id = HashMap::new();
        let mut index_by_output_socket_id = HashMap::new();

        for component in Component::list(ctx).await? {
            let component_id = component.id();
            let schema_variant_id = ctx
                .workspace_snapshot()?
                .schema_variant_id_for_component_id(component_id)
                .await?;
            let input_sockets = InputSocket::list(ctx, schema_variant_id).await?;
            let output_sockets = OutputSocket::list(ctx, schema_variant_id).await?;
            let input_socket_ids: Vec<InputSocketId> =
                input_sockets.iter().map(|is| is.id()).collect();
            let output_socket_ids: Vec<OutputSocketId> =
                output_sockets.iter().map(|os| os.id()).collect();

            let component_weight = InferredFlorpNodeWeight {
                component,
                input_sockets,
                output_sockets,
            };

            let node_index = down_component_graph.add_node(component_weight);
            index_by_component_id.insert(component_id, node_index);
            for input_socket_id in input_socket_ids {
                index_by_input_socket_id.insert(input_socket_id, node_index);
            }
            for output_socket_id in output_socket_ids {
                index_by_output_socket_id.insert(output_socket_id, node_index);
            }
        }
        // Gather the "frame contains" information for all Components to build the edges of the
        // graph.
        for (&component_id, &source_node_index) in &index_by_component_id {
            if let Some(target_component_ids) = ctx
                .workspace_snapshot()?
                .frame_contains_components(component_id)
                .await?
            {
                for target_component_id in target_component_ids {
                    let destination_node_index =
                        *index_by_component_id
                            .get(&target_component_id)
                            .ok_or_else(|| FlorpError::OrphanedComponent(target_component_id))?;
                    down_component_graph.add_edge(source_node_index, destination_node_index, ());
                }
            }
        }

        let mut up_component_graph = down_component_graph.clone();
        up_component_graph.reverse();

        Ok(Self {
            down_component_graph,
            up_component_graph,
            index_by_component_id,
            index_by_input_socket_id,
            index_by_output_socket_id,
        })
    }
}
