//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine_types::{SubstateVersion, substate::SubstateId};
use tari_jellyfish::{TreeHash, Version};
use tari_ootle_common_types::VersionedSubstateId;
use tari_state_tree::{
    IndexedTreeDiff,
    SpreadPrefixStateTree,
    StagedTreeStore,
    StateTreePayload,
    SubstateTreeChange,
    memory_store::MemoryTreeStore,
};
use tari_template_lib_types::{ComponentAddress, Hash32, ObjectKey};

fn id(n: u32) -> VersionedSubstateId {
    let mut key = [0u8; ObjectKey::LENGTH];
    key[..4].copy_from_slice(&n.to_be_bytes());
    key[4..8].copy_from_slice(&n.wrapping_mul(0x9E37_79B9).to_be_bytes());
    VersionedSubstateId::new(
        SubstateId::Component(ComponentAddress::new(ObjectKey::from_array(key))),
        SubstateVersion::ZERO,
    )
}

fn up(n: u32, value: u8) -> SubstateTreeChange {
    SubstateTreeChange::Up {
        id: id(n),
        value_hash: Hash32::from_array([value; 32]),
    }
}

fn down(n: u32) -> SubstateTreeChange {
    SubstateTreeChange::Down { id: id(n) }
}

/// Version 1 is committed. The pending versions after it add leaves, replace leaves, delete leaves an earlier pending
/// version added, and delete enough of one subtree that its internal nodes collapse.
fn versions() -> Vec<Vec<SubstateTreeChange>> {
    vec![
        (0..300).map(|n| up(n, 1)).collect(),
        (300..400).map(|n| up(n, 2)).chain((0..50).map(|n| up(n, 2))).collect(),
        (300..350).map(down).chain((400..450).map(|n| up(n, 3))).collect(),
        (50..290).map(down).chain((350..360).map(|n| up(n, 4))).collect(),
        (0..10).map(|n| up(n, 5)).chain((400..440).map(down)).collect(),
    ]
}

fn version(index: usize) -> Version {
    Version::try_from(index).unwrap() + 1
}

/// The roots of each version when every version is committed before the next is computed.
fn committed_roots() -> Vec<TreeHash> {
    let mut store = MemoryTreeStore::<StateTreePayload>::new();
    let mut roots = vec![];
    for (i, changes) in versions().into_iter().enumerate() {
        let current = i.checked_sub(1).map(version);
        let root = SpreadPrefixStateTree::new(&mut store)
            .put_substate_changes(current, version(i), changes)
            .unwrap();
        roots.push(root);
    }
    roots
}

#[test]
fn pending_diffs_give_the_roots_committing_each_version_gives() {
    let mut committed = MemoryTreeStore::<StateTreePayload>::new();
    let mut versions = versions().into_iter();
    let mut roots = vec![
        SpreadPrefixStateTree::new(&mut committed)
            .put_substate_changes(None, version(0), versions.next().unwrap())
            .unwrap(),
    ];

    let mut pending = Vec::<IndexedTreeDiff<StateTreePayload>>::new();
    for (i, changes) in versions.enumerate().map(|(i, changes)| (i + 1, changes)) {
        let mut store = StagedTreeStore::new(&committed);
        for diff in &pending {
            store.apply_pending_diff(diff.clone());
        }
        let root = SpreadPrefixStateTree::new(&mut store)
            .put_substate_changes(Some(version(i - 1)), version(i), changes)
            .unwrap();
        roots.push(root);
        pending.push(IndexedTreeDiff::new(store.into_diff()));
    }

    assert_eq!(roots, committed_roots());
}
