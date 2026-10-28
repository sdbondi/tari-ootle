//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Checks a shard snapshot, the substates a shard's tree holds at a verified version, against the shard root its
//! verified leaf commits.
//!
//! The indexer keys and hashes each leaf itself from the substate the peer sends, so a substate added, left out,
//! altered or misplaced in the order builds another root.

use ootle_network::Network;
use prost::Message;
use tari_ootle_common_types::Epoch;
use tari_ootle_p2p::proto::rpc;
use tari_ootle_storage::consensus_models::{
    SubstateCreate,
    SubstateData,
    SubstateUpdateProof,
    SubstateValueFilterFlags,
};
use tari_state_tree::{
    JmtHashScheme,
    LeafKey,
    SortedLeafRootBuilder,
    SubstateTreeChange,
    TreeHash,
    key_mapper::{DbKeyMapper, SpreadPrefixKeyMapper},
};

use crate::{
    network_state_sync::{validator_status::ProvedShardLeaf, verified_stage::streamed_tree_change},
    storage_sqlite::models::SnapshotSubstate,
};

/// A shard snapshot as it is paged in, checked against the leaf it was taken at.
pub struct ShardSnapshot {
    network: Network,
    leaf: ProvedShardLeaf,
    value_filters: SubstateValueFilterFlags,
    root: SortedLeafRootBuilder,
}

impl ShardSnapshot {
    /// A snapshot of `leaf`'s shard at its version, whose substates `value_filters` selects are indexed.
    pub fn new(network: Network, leaf: ProvedShardLeaf, value_filters: SubstateValueFilterFlags) -> Self {
        Self {
            network,
            leaf,
            value_filters,
            root: SortedLeafRootBuilder::new(JmtHashScheme::V1),
        }
    }

    /// The leaf key of the last substate added, which the next page starts above.
    pub fn last_key(&self) -> Option<&LeafKey> {
        self.root.last_key()
    }

    /// The number of substates added.
    pub fn num_substates(&self) -> usize {
        self.root.num_leaves()
    }

    /// Adds a page of the snapshot, which must continue in leaf key order from the last substate added. Returns the
    /// substates to stage: those the value filters select, sent with their value or template metadata. The others
    /// index nothing.
    pub fn add_page(&mut self, substates: Vec<rpc::SnapshotSubstate>) -> Result<Vec<SnapshotSubstate>, SnapshotError> {
        let mut staged = Vec::new();
        for substate in substates {
            let created_epoch = substate
                .created_epoch
                .map(Epoch::from)
                .ok_or_else(|| SnapshotError::Malformed("a substate has no created epoch".to_string()))?;
            let encoded = substate
                .substate
                .ok_or_else(|| SnapshotError::Malformed("a substate has no data".to_string()))?;
            let data = SubstateData::try_from(encoded.clone()).map_err(|e| SnapshotError::Malformed(e.to_string()))?;
            let substate_id = data.substate_id.clone();
            let is_indexed = self.value_filters.contains_substate(&substate_id) &&
                (data.value.value().is_some() || data.template_metadata.is_some());
            let update = SubstateUpdateProof::Create(Box::new(SubstateCreate { substate: data }));
            let SubstateTreeChange::Up { id, value_hash } = streamed_tree_change(self.network, created_epoch, &update)
                .map_err(|e| SnapshotError::Malformed(e.to_string()))?
            else {
                unreachable!("a created substate changes the tree with an up");
            };
            let leaf_key = SpreadPrefixKeyMapper::map_to_leaf_key(&id);
            self.root
                .push(leaf_key, &TreeHash::new(value_hash.into_array()))
                .map_err(|e| SnapshotError::Malformed(e.to_string()))?;
            if is_indexed {
                staged.push(SnapshotSubstate {
                    epoch: created_epoch,
                    leaf_key,
                    substate: encoded.encode_to_vec(),
                });
            }
        }
        Ok(staged)
    }

    /// Checks that the substates added rebuild the root the snapshot's leaf commits.
    pub fn finish(self) -> Result<ProvedShardLeaf, SnapshotError> {
        let num_substates = self.root.num_leaves();
        let built = self.root.finish();
        if built != self.leaf.shard_root {
            return Err(SnapshotError::RootMismatch {
                num_substates,
                built,
                committed: self.leaf.shard_root,
            });
        }
        Ok(self.leaf)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SnapshotError {
    #[error("the snapshot is malformed: {0}")]
    Malformed(String),
    #[error("the snapshot's {num_substates} substates build root {built}, but the verified leaf commits {committed}")]
    RootMismatch {
        num_substates: usize,
        built: TreeHash,
        committed: TreeHash,
    },
}

#[cfg(test)]
mod tests {
    use tari_engine_types::{
        non_fungible::NonFungibleContainer,
        published_template::{PublishedTemplate, PublishedTemplateAddress},
        substate::{SubstateId, SubstateValue, hash_substate},
    };
    use tari_ootle_common_types::{StateVersion, SubstateVersion, VersionedSubstateId, shard::Shard};
    use tari_ootle_storage::consensus_models::SubstateValueOrHash;
    use tari_state_tree::{JellyfishMerkleTree, TransitionAccumulator, TreeStoreWriter, memory_store::MemoryTreeStore};
    use tari_template_lib_types::{ComponentAddress, Hash32, ObjectKey, crypto::RistrettoPublicKeyBytes};

    use super::*;
    use crate::network_state_sync::verified_stage::VerifiedShardLeaf;

    const NETWORK: Network = Network::LocalNet;

    fn shard() -> Shard {
        Shard::from_u32(3)
    }

    fn component(seed: u8) -> (SubstateId, SubstateValue) {
        let id = SubstateId::Component(ComponentAddress::new(ObjectKey::from_array([seed; ObjectKey::LENGTH])));
        (id, SubstateValue::NonFungible(NonFungibleContainer::no_data()))
    }

    fn template(seed: u8) -> (SubstateId, SubstateValue) {
        let template = PublishedTemplate {
            template_name: "snapshot".try_into().unwrap(),
            author: RistrettoPublicKeyBytes::default(),
            binary: vec![seed; 16].try_into().unwrap(),
            at_epoch: 1,
            metadata_hash: None,
        };
        let address =
            PublishedTemplateAddress::from_author_and_binary_hash(&template.author, &template.to_binary_hash());
        (SubstateId::Template(address), SubstateValue::Template(template))
    }

    /// A substate as the responder serves it under `value_filters`, created at `epoch`.
    fn served(
        (id, value): (SubstateId, SubstateValue),
        epoch: Epoch,
        value_filters: SubstateValueFilterFlags,
    ) -> rpc::SnapshotSubstate {
        let version = SubstateVersion::ZERO;
        let state_hash = hash_substate(NETWORK, &value, version, epoch);
        let is_template = id.is_template();
        let template_metadata = (is_template && value_filters.contains(SubstateValueFilterFlags::TEMPLATE_METADATA))
            .then(|| value.as_template().unwrap().clone().into_template_metadata());
        let value = if value_filters.contains_substate(&id) && !is_template {
            SubstateValueOrHash::Value(Box::new(value))
        } else {
            SubstateValueOrHash::Hash(state_hash)
        };
        rpc::SnapshotSubstate {
            substate: Some(
                SubstateData {
                    substate_id: id,
                    version,
                    value,
                    template_metadata,
                }
                .into(),
            ),
            created_epoch: Some(epoch.into()),
        }
    }

    fn leaf_key_of(substate: &rpc::SnapshotSubstate) -> LeafKey {
        let data = SubstateData::try_from(substate.substate.clone().unwrap()).unwrap();
        SpreadPrefixKeyMapper::map_to_leaf_key(&VersionedSubstateId::new(data.substate_id, data.version))
    }

    /// The substates of a shard, in leaf key order, and the root of the tree holding them.
    fn shard_state(value_filters: SubstateValueFilterFlags) -> (Vec<rpc::SnapshotSubstate>, TreeHash) {
        let mut substates = (0..20u8)
            .map(|seed| served(component(seed), Epoch(u64::from(seed % 3) + 1), value_filters))
            .chain((0..3u8).map(|seed| served(template(seed), Epoch(2), value_filters)))
            .chain((20..25u8).map(|seed| {
                let mut hidden = served(component(seed), Epoch(1), value_filters);
                let data = SubstateData::try_from(hidden.substate.take().unwrap()).unwrap();
                hidden.substate = Some(
                    SubstateData {
                        value: SubstateValueOrHash::Hash(data.to_value_hash(NETWORK, Epoch(1))),
                        ..data
                    }
                    .into(),
                );
                hidden
            }))
            .collect::<Vec<_>>();
        substates.sort_by_key(leaf_key_of);

        let mut mem = MemoryTreeStore::new();
        let values = substates.iter().map(|substate| {
            let data = SubstateData::try_from(substate.substate.clone().unwrap()).unwrap();
            let epoch = Epoch::from(substate.created_epoch.unwrap());
            let value_hash = match &data.template_metadata {
                Some(metadata) => tari_engine_types::substate::hash_published_template_metadata(
                    NETWORK,
                    metadata,
                    data.version,
                    epoch,
                ),
                None => data.to_value_hash(NETWORK, epoch),
            };
            (
                leaf_key_of(substate),
                Some((TreeHash::new(value_hash.into_array()), ())),
            )
        });
        let (root, diff) = JellyfishMerkleTree::new(&mem, JmtHashScheme::V1)
            .batch_put_value_set(values, None, 1)
            .unwrap();
        for (key, node) in diff.node_batch {
            mem.insert_node(key, node).unwrap();
        }
        (substates, root)
    }

    fn leaf_with_root(shard_root: TreeHash) -> ProvedShardLeaf {
        ProvedShardLeaf {
            leaf: VerifiedShardLeaf {
                shard: shard(),
                state_version: StateVersion::new(9),
                transition_acc: TransitionAccumulator::new([9; 32]),
            },
            shard_root,
        }
    }

    fn filters() -> SubstateValueFilterFlags {
        SubstateValueFilterFlags::COMPONENT | SubstateValueFilterFlags::TEMPLATE_METADATA
    }

    #[test]
    fn a_snapshot_paged_in_order_rebuilds_the_committed_root() {
        let (substates, root) = shard_state(filters());
        for page_size in [1, 4, 28, 100] {
            let mut snapshot = ShardSnapshot::new(NETWORK, leaf_with_root(root), filters());
            let mut staged = Vec::new();
            for page in substates.chunks(page_size) {
                staged.extend(snapshot.add_page(page.to_vec()).unwrap());
                assert_eq!(snapshot.last_key(), Some(&leaf_key_of(page.last().unwrap())));
            }
            assert_eq!(snapshot.num_substates(), 28);
            assert_eq!(snapshot.finish().unwrap(), leaf_with_root(root));
            // The components sent with values and the templates sent with metadata are staged, the hidden ones are not
            assert_eq!(staged.len(), 23, "page size {page_size}");
        }
    }

    #[test]
    fn a_snapshot_missing_altered_or_extra_substates_does_not_match() {
        let (substates, root) = shard_state(filters());
        let check = |substates: Vec<rpc::SnapshotSubstate>| {
            let mut snapshot = ShardSnapshot::new(NETWORK, leaf_with_root(root), filters());
            snapshot.add_page(substates)?;
            snapshot.finish()
        };

        let mut missing = substates.clone();
        missing.remove(11);
        assert!(matches!(check(missing), Err(SnapshotError::RootMismatch { .. })));

        let mut altered = substates.clone();
        let index = altered
            .iter()
            .position(|substate| {
                let data = SubstateData::try_from(substate.substate.clone().unwrap()).unwrap();
                data.value.value().is_some()
            })
            .unwrap();
        altered[index].created_epoch = Some(Epoch(99).into());
        assert!(matches!(check(altered), Err(SnapshotError::RootMismatch { .. })));

        let mut extra = substates.clone();
        extra.push(served(component(200), Epoch(1), filters()));
        extra.sort_by_key(leaf_key_of);
        assert!(matches!(check(extra), Err(SnapshotError::RootMismatch { .. })));
    }

    #[test]
    fn a_snapshot_out_of_key_order_is_malformed() {
        let (mut substates, root) = shard_state(filters());
        substates.swap(3, 4);
        let mut snapshot = ShardSnapshot::new(NETWORK, leaf_with_root(root), filters());
        assert!(matches!(snapshot.add_page(substates), Err(SnapshotError::Malformed(_))));

        let (substates, root) = shard_state(filters());
        let mut snapshot = ShardSnapshot::new(NETWORK, leaf_with_root(root), filters());
        snapshot.add_page(substates[5..].to_vec()).unwrap();
        assert!(matches!(
            snapshot.add_page(substates[..5].to_vec()),
            Err(SnapshotError::Malformed(_))
        ));
    }

    #[test]
    fn an_empty_snapshot_matches_only_the_empty_root() {
        let snapshot = ShardSnapshot::new(NETWORK, leaf_with_root(TreeHash::zero()), filters());
        assert!(snapshot.finish().is_ok());
        let (_, root) = shard_state(filters());
        let snapshot = ShardSnapshot::new(NETWORK, leaf_with_root(root), filters());
        assert!(matches!(snapshot.finish(), Err(SnapshotError::RootMismatch { .. })));
    }

    #[test]
    fn a_template_whose_metadata_disagrees_with_its_hash_is_malformed() {
        let (substates, root) = shard_state(filters());
        let mut tampered = substates.clone();
        let index = tampered
            .iter()
            .position(|substate| !substate.substate.as_ref().unwrap().template_metadata.is_empty())
            .unwrap();
        let mut data = SubstateData::try_from(tampered[index].substate.take().unwrap()).unwrap();
        data.value = SubstateValueOrHash::Hash(Hash32::from_array([1; 32]));
        tampered[index].substate = Some(data.into());
        let mut snapshot = ShardSnapshot::new(NETWORK, leaf_with_root(root), filters());
        assert!(matches!(snapshot.add_page(tampered), Err(SnapshotError::Malformed(_))));
    }
}
