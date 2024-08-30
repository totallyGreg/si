use std::collections::{hash_map::Entry, HashMap, HashSet};

use petgraph::{prelude::*, visit::DfsEvent};

use crate::{
    workspace_snapshot::content_address::ContentAddressDiscriminants, ComponentId,
    EdgeWeightKindDiscriminants, InputSocketId, OutputSocketId, SchemaVariantId,
    WorkspaceSnapshotGraphV2,
};

use super::node_weight::{ArgumentTargets, NodeWeight};

#[derive(Debug, Clone, Copy)]
pub enum FamilyConnection {
    DirectParentage,
    IndirectParentage,
}

pub struct ComponentFamilyTree {
    family: StableDiGraph<ComponentId, FamilyConnection>,
    component_id_to_index: HashMap<ComponentId, NodeIndex>,
}

fn get_component_id(
    snapshot_graph: &WorkspaceSnapshotGraphV2,
    component_idx: NodeIndex,
) -> Option<ComponentId> {
    snapshot_graph
        .graph()
        .node_weight(component_idx)
        .and_then(|weight| match weight {
            NodeWeight::Component(component) => Some(component.id().into()),
            _ => None,
        })
}

fn get_output_socket_for_apa(
    snapshot_graph: &WorkspaceSnapshotGraphV2,
    apa_idx: NodeIndex,
) -> Option<OutputSocketId> {
    let graph = snapshot_graph.graph();
    graph
        .edges_directed(apa_idx, Outgoing)
        .filter(|edge_ref| {
            EdgeWeightKindDiscriminants::PrototypeArgumentValue == edge_ref.weight().kind().into()
        })
        .filter_map(|edge_ref| {
            graph
                .node_weight(edge_ref.target())
                .and_then(|node_weight| match node_weight {
                    NodeWeight::Content(content_inner) => {
                        if content_inner.content_address_discriminants()
                            == ContentAddressDiscriminants::OutputSocket
                        {
                            Some(content_inner.id().into())
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
        })
        .next()
}

fn get_input_socket_for_apa(
    snapshot_graph: &WorkspaceSnapshotGraphV2,
    apa_idx: NodeIndex,
) -> Option<InputSocketId> {
    let graph = snapshot_graph.graph();
    // APA <-- PrototypeArgument -- Content::AttributePrototype <-- Prototype -- Content::InputSocket

    graph
        .edges_directed(apa_idx, Incoming)
        .filter(|edge_ref| {
            EdgeWeightKindDiscriminants::PrototypeArgument == edge_ref.weight().kind().into()
        })
        .next()
        .and_then(|edge_ref| {
            graph
                .node_weight(edge_ref.source())
                .and_then(|prototype_weight| match prototype_weight {
                    NodeWeight::Content(content_inner) => {
                        if content_inner.content_address_discriminants()
                            == ContentAddressDiscriminants::AttributePrototype
                        {
                            graph
                                .edges_directed(edge_ref.source(), Incoming)
                                .filter(|edge_ref| {
                                    EdgeWeightKindDiscriminants::Prototype
                                        == edge_ref.weight().kind().into()
                                })
                                .filter_map(|edge_ref| match graph.node_weight(edge_ref.source()) {
                                    Some(NodeWeight::Content(content_inner)) => {
                                        if content_inner.content_address_discriminants()
                                            == ContentAddressDiscriminants::InputSocket
                                        {
                                            Some(content_inner.id().into())
                                        } else {
                                            None
                                        }
                                    }
                                    _ => None,
                                })
                                .next()
                        } else {
                            None
                        }
                    }
                    _ => None,
                })
        })
}

impl ComponentFamilyTree {
    fn new() -> Self {
        Self {
            family: StableDiGraph::new(),
            component_id_to_index: HashMap::new(),
            explicitly_connected: HashSet::new(),
            component_variants: HashMap::new(),
        }
    }

    fn add_component(&mut self, component_id: ComponentId) -> NodeIndex {
        match self.component_id_to_index.entry(component_id) {
            Entry::Occupied(entry) => *entry.get(),
            Entry::Vacant(vacant) => {
                let new_idx = self.family.add_node(component_id);
                vacant.insert(new_idx);
                new_idx
            }
        }
    }

    fn add_parentage(&mut self, parent_id: ComponentId, child_id: ComponentId) {
        let parent_idx = self.add_component(parent_id);
        let child_idx = self.add_component(child_id);

        self.family
            .update_edge(parent_idx, child_idx, FamilyConnection::DirectParentage);

        // This will find all direct and indirect grandparents, and add them as
        // indirect parentage
        let grandparents: Vec<NodeIndex> = self
            .family
            .neighbors_directed(parent_idx, Incoming)
            .collect();

        for grandparent_idx in grandparents {
            self.family.update_edge(
                grandparent_idx,
                child_idx,
                FamilyConnection::IndirectParentage,
            );
        }
    }

    pub fn generate(snapshot_graph: &WorkspaceSnapshotGraphV2) -> Self {
        let graph = snapshot_graph.graph();

        let mut family = Self::new();
        let mut explicitly_connected: HashSet<(ComponentId, InputSocketId)> = HashSet::new();
        let mut component_variants: HashMap<ComponentId, SchemaVariantId> = HashMap::new();
        let mut variant_input_sockets: HashMap<SchemaVariantId, HashSet<InputSocketId>> =
            HashMap::new();
        let mut variant_output_sockets: HashMap<SchemaVariantId, HashSet<OutputSocketId>> =
            HashMap::new();

        petgraph::visit::depth_first_search(
            graph,
            Some(snapshot_graph.root()),
            |event| match event {
                DfsEvent::BackEdge(source_idx, target_idx)
                | DfsEvent::CrossForwardEdge(source_idx, target_idx)
                | DfsEvent::TreeEdge(source_idx, target_idx) => {
                    if let Some(edge_idx) = graph.find_edge(source_idx, target_idx) {
                        if Some(EdgeWeightKindDiscriminants::FrameContains)
                            == graph
                                .edge_weight(edge_idx)
                                .map(|edge_weight| edge_weight.kind().into())
                        {
                            let parent_id = get_component_id(snapshot_graph, source_idx);
                            let child_id = get_component_id(snapshot_graph, target_idx);
                            if let (Some(parent_id), Some(child_id)) = (parent_id, child_id) {
                                family.add_parentage(parent_id, child_id);
                            }
                        }
                    }
                }
                DfsEvent::Discover(node_idx, _) => match graph.node_weight(node_idx) {
                    Some(NodeWeight::AttributePrototypeArgument(apa_inner)) => {
                        if let Some(ArgumentTargets {
                            source_component_id,
                            destination_component_id,
                        }) = apa_inner.targets()
                        {
                            dbg!(source_component_id, destination_component_id);
                            let input_socket = get_input_socket_for_apa(snapshot_graph, node_idx);
                            dbg!(input_socket);
                            let output_socket = get_output_socket_for_apa(snapshot_graph, node_idx);
                            dbg!(output_socket);
                        }
                    }
                    _ => {}
                },
                _ => {}
            },
        );

        family
    }

    pub fn direct_parents_of(&self, component_id: ComponentId) -> Vec<ComponentId> {
        if let Some(component_idx) = self.component_id_to_index.get(&component_id) {
            self.family
                .edges_directed(*component_idx, Incoming)
                .filter(|edge_ref| matches!(edge_ref.weight(), FamilyConnection::DirectParentage))
                .filter_map(|edge_ref| self.family.node_weight(edge_ref.source()).copied())
                .collect()
        } else {
            vec![]
        }
    }

    pub fn all_parents_of(&self, component_id: ComponentId) -> Vec<ComponentId> {
        if let Some(component_idx) = self.component_id_to_index.get(&component_id) {
            self.family
                .neighbors_directed(*component_idx, Incoming)
                .filter_map(|source_idx| self.family.node_weight(source_idx).copied())
                .collect()
        } else {
            vec![]
        }
    }
}
