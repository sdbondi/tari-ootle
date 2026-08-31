//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

pub mod helpers;

use std::collections::{HashMap, HashSet};

use helpers::{create_rocksdb, create_substate_update_batch, gen_substates_for_shards};
use tari_ootle_common_types::Epoch;
use tari_ootle_storage::{
    StateStore,
    StateStoreReadTransaction,
    StateStoreWriteTransaction,
    consensus_models::{Block, SubstateDestroyed, SubstateValueFilterFlags},
};
use tari_ootle_transaction::Network;
use tari_state_tree::Version;

use crate::helpers::num_preshards;

#[test]
fn rocksdb() {
    let (db, _tmp) = create_rocksdb();

    let num_transitions = 100; // Makes double
    const EPOCH: Epoch = Epoch::zero();
    let mut tx = db.create_write_tx().unwrap();

    let zero_block = Block::zero_block(Network::LocalNet, num_preshards());
    zero_block.insert(&mut tx).unwrap();

    let mut shards = HashMap::new();
    let substates = gen_substates_for_shards(EPOCH, 1, 0..num_transitions, 0).collect::<Vec<_>>();
    shards.insert(
        1 as Version,
        (
            substates.len(),
            substates.iter().map(|s| s.shard()).collect::<HashSet<_>>(),
        ),
    );
    let batch = create_substate_update_batch(Epoch::zero(), &substates);
    tx.substates_commit_batch(batch).unwrap();

    // Add a couple for a different shard
    let substates = gen_substates_for_shards(EPOCH, 2, num_transitions..num_transitions + 2, 0).collect::<Vec<_>>();
    shards.insert(
        2,
        (
            substates.len(),
            substates.iter().map(|s| s.shard()).collect::<HashSet<_>>(),
        ),
    );
    let batch = create_substate_update_batch(Epoch::zero(), &substates);
    tx.substates_commit_batch(batch).unwrap();

    let substates = gen_substates_for_shards(EPOCH, 3, 0..num_transitions, 1).collect::<Vec<_>>();
    shards.insert(
        3,
        (
            substates.len(),
            substates.iter().map(|s| s.shard()).collect::<HashSet<_>>(),
        ),
    );
    let batch = create_substate_update_batch(Epoch::zero(), &substates);
    tx.substates_commit_batch(batch).unwrap();

    for (state_version, (num_substates, shards)) in &shards {
        for shard in shards {
            let transitions = tx
                .state_transitions_get_starting_at(*shard, *state_version, SubstateValueFilterFlags::all())
                .unwrap();
            assert_eq!(transitions.epoch, EPOCH);
            assert_eq!(transitions.state_version, *state_version);
            assert_eq!(transitions.shard, *shard);
            assert_eq!(transitions.updates.len(), *num_substates);
        }
    }
}

/// A destroyed substate keeps its value until epoch GC prunes it, so `UP_ONLY` must decide liveness by
/// whether the substate is destroyed. Deciding it by value presence would return this up - and no down
/// follows it under `UP_ONLY` - leaving the caller with a substate it believes is still live.
#[test]
fn up_only_skips_an_up_whose_substate_was_since_destroyed() {
    let (db, _tmp) = create_rocksdb();
    const EPOCH: Epoch = Epoch::zero();
    let mut tx = db.create_write_tx().unwrap();
    Block::zero_block(Network::LocalNet, num_preshards())
        .insert(&mut tx)
        .unwrap();

    let mut substate = gen_substates_for_shards(EPOCH, 1, 0..1, 0).next().unwrap();
    let shard = substate.shard();
    tx.substates_commit_batch(create_substate_update_batch(EPOCH, [&substate]))
        .unwrap();

    // The up is streamed while the substate is live.
    let up_only = SubstateValueFilterFlags::all_substates() | SubstateValueFilterFlags::UP_ONLY;
    let transitions = tx.state_transitions_get_starting_at(shard, 1, up_only).unwrap();
    assert_eq!(transitions.updates.len(), 1);

    // Destroy it at v2. substates_commit_batch marks the record destroyed but retains its value, which is
    // what epoch GC would later prune.
    substate.set_destroyed(SubstateDestroyed {
        at_epoch: EPOCH,
        at_state_version: 2,
    });
    tx.substates_commit_batch(create_substate_update_batch(EPOCH, [&substate]))
        .unwrap();

    let transitions = tx.state_transitions_get_starting_at(shard, 1, up_only).unwrap();
    assert!(
        transitions.updates.is_empty(),
        "UP_ONLY returned an up for a destroyed substate: {:?}",
        transitions.updates
    );

    // Without UP_ONLY the same up is still streamed - the full transition log is unchanged.
    let transitions = tx
        .state_transitions_get_starting_at(shard, 1, SubstateValueFilterFlags::all_substates())
        .unwrap();
    assert_eq!(transitions.updates.len(), 1);
}
