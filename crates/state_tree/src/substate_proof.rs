//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use serde::{Deserialize, Serialize};
use tari_engine_types::ProtocolVersion;
use tari_jellyfish::{SparseMerkleProofExt, TreeHash, Version};
use tari_ootle_common_types::VersionedSubstateId;

use crate::{
    key_mapper::{DbKeyMapper, HashIdentityKeyMapper, SpreadPrefixKeyMapper},
    shard_state_leaf,
};

/// A two-level Merkle proof binding a single substate to a shard-group state Merkle root (the
/// `state_merkle_root` committed in an L2 block header):
///   1. the substate leaf is included in (or excluded from) its shard's JMT, rooted at `shard_root`;
///   2. the shard's leaf (see [`shard_state_leaf`]) is a leaf of the shard-group root tree, rooted at the block
///      header's `state_merkle_root` (an ephemeral tree over the `[global, shard_0, shard_1, ...]` leaves).
///
/// Verifying both levels against a *trusted* group root proves the substate's committed value (via
/// [`SubstateValueProof::verify_inclusion`]) or its absence (via
/// [`SubstateValueProof::verify_exclusion`]) without trusting the node that produced the proof. The
/// caller is responsible for obtaining a trusted `group_root` (e.g. from a verified committed block
/// proof) and, for inclusion, for binding `value_hash` to the returned substate value.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubstateValueProof {
    /// The substate's shard JMT root.
    pub shard_root: TreeHash,
    /// Proof that the shard's leaf is a leaf of the shard-group root tree.
    pub shard_root_proof: SparseMerkleProofExt,
    /// Proof for the substate leaf within its shard JMT (inclusion or exclusion).
    pub leaf_proof: SparseMerkleProofExt,
    /// The shard's state version at `shard_root`, which the shard's leaf commits to from
    /// [`ProtocolVersion::V1`].
    #[serde(default)]
    pub shard_state_version: Version,
}

impl SubstateValueProof {
    pub fn new(
        shard_root: TreeHash,
        shard_state_version: Version,
        shard_root_proof: SparseMerkleProofExt,
        leaf_proof: SparseMerkleProofExt,
    ) -> Self {
        Self {
            shard_root,
            shard_root_proof,
            leaf_proof,
            shard_state_version,
        }
    }

    /// Verifies that `versioned_id` is committed with leaf value hash `value_hash` under the trusted
    /// shard-group `group_root`, committed under `protocol_version`.
    pub fn verify_inclusion(
        &self,
        protocol_version: ProtocolVersion,
        group_root: &TreeHash,
        versioned_id: &VersionedSubstateId,
        value_hash: &TreeHash,
    ) -> Result<(), SubstateValueProofError> {
        self.verify_shard_root(protocol_version, group_root)?;
        let leaf_key = SpreadPrefixKeyMapper::map_to_leaf_key(versioned_id);
        self.leaf_proof
            .verify_inclusion(&self.shard_root, &leaf_key, value_hash)
            .map_err(|e| SubstateValueProofError::LeafProof(e.to_string()))
    }

    /// Verifies that `versioned_id` is absent (destroyed or never created) under the trusted
    /// shard-group `group_root`, committed under `protocol_version`.
    pub fn verify_exclusion(
        &self,
        protocol_version: ProtocolVersion,
        group_root: &TreeHash,
        versioned_id: &VersionedSubstateId,
    ) -> Result<(), SubstateValueProofError> {
        self.verify_shard_root(protocol_version, group_root)?;
        let leaf_key = SpreadPrefixKeyMapper::map_to_leaf_key(versioned_id);
        self.leaf_proof
            .verify_exclusion(&self.shard_root, &leaf_key)
            .map_err(|e| SubstateValueProofError::LeafProof(e.to_string()))
    }

    /// Level 2: prove the shard's leaf is a leaf of the shard-group root tree. That tree stores each
    /// leaf hash as both the source of its key and its value (see `compute_proof_for_hashes`).
    fn verify_shard_root(
        &self,
        protocol_version: ProtocolVersion,
        group_root: &TreeHash,
    ) -> Result<(), SubstateValueProofError> {
        let leaf = shard_state_leaf(protocol_version, &self.shard_root, self.shard_state_version);
        let leaf_key = HashIdentityKeyMapper::map_to_leaf_key(&leaf);
        self.shard_root_proof
            .verify_inclusion(group_root, &leaf_key, &leaf)
            .map_err(|e| SubstateValueProofError::ShardRootProof(e.to_string()))
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SubstateValueProofError {
    #[error("shard-root-in-group-root proof is invalid: {0}")]
    ShardRootProof(String),
    #[error("substate-leaf-in-shard-root proof is invalid: {0}")]
    LeafProof(String),
}
