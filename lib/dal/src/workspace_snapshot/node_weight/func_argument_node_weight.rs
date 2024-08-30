use serde::{Deserialize, Serialize};
use si_events::{merkle_tree_hash::MerkleTreeHash, ulid::Ulid, ContentHash};

use crate::{
    workspace_snapshot::{
        content_address::{ContentAddress, ContentAddressDiscriminants},
        graph::{deprecated::v1::DeprecatedFuncArgumentNodeWeightV1, LineageId},
        node_weight::traits::CorrectTransforms,
        node_weight::NodeWeightResult,
        NodeWeightError,
    },
    EdgeWeightKindDiscriminants,
};

use super::NodeHash;

#[derive(Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FuncArgumentNodeWeight {
    pub id: Ulid,
    pub lineage_id: LineageId,
    content_address: ContentAddress,
    pub(super) merkle_tree_hash: MerkleTreeHash,
    name: String,
}

impl FuncArgumentNodeWeight {
    pub fn new(id: Ulid, lineage_id: Ulid, content_address: ContentAddress, name: String) -> Self {
        Self {
            id,
            lineage_id,
            content_address,
            merkle_tree_hash: MerkleTreeHash::default(),
            name,
        }
    }

    pub fn content_address(&self) -> ContentAddress {
        self.content_address
    }

    pub fn content_hash(&self) -> ContentHash {
        self.content_address.content_hash()
    }

    pub fn content_store_hashes(&self) -> Vec<ContentHash> {
        vec![self.content_address.content_hash()]
    }

    pub fn id(&self) -> Ulid {
        self.id
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn set_name(&mut self, name: impl Into<String>) -> &mut Self {
        self.name = name.into();
        self
    }

    pub fn new_content_hash(&mut self, content_hash: ContentHash) -> NodeWeightResult<()> {
        let new_address = match &self.content_address {
            ContentAddress::FuncArg(_) => ContentAddress::FuncArg(content_hash),
            other => {
                return Err(NodeWeightError::InvalidContentAddressForWeightKind(
                    Into::<ContentAddressDiscriminants>::into(other).to_string(),
                    ContentAddressDiscriminants::FuncArg.to_string(),
                ));
            }
        };

        self.content_address = new_address;

        Ok(())
    }

    pub const fn exclusive_outgoing_edges(&self) -> &[EdgeWeightKindDiscriminants] {
        &[]
    }
}

impl NodeHash for FuncArgumentNodeWeight {
    fn node_hash(&self) -> ContentHash {
        ContentHash::from(&serde_json::json![{
            "content_address": self.content_address,
            "name": self.name,
        }])
    }
}

impl std::fmt::Debug for FuncArgumentNodeWeight {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.debug_struct("FuncNodeWeight")
            .field("id", &self.id().to_string())
            .field("lineage_id", &self.lineage_id.to_string())
            .field("name", &self.name)
            .field("content_hash", &self.content_hash())
            .field("merkle_tree_hash", &self.merkle_tree_hash)
            .finish()
    }
}

impl From<DeprecatedFuncArgumentNodeWeightV1> for FuncArgumentNodeWeight {
    fn from(value: DeprecatedFuncArgumentNodeWeightV1) -> Self {
        Self {
            id: value.id,
            lineage_id: value.lineage_id,
            content_address: value.content_address,
            merkle_tree_hash: value.merkle_tree_hash,
            name: value.name,
        }
    }
}

impl CorrectTransforms for FuncArgumentNodeWeight {}
