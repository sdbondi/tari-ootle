//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashSet, time::Duration};

use tari_consensus::hotstuff::{HotStuffError, commit_proofs::state_version_commit_proof};
use tari_consensus_types::{BlockId, Decision};
use tari_ootle_common_types::{Epoch, NodeHeight, optional::Optional};
use tari_ootle_storage::{
    ShardScopedTreeStoreReader,
    StateStore,
    StateStoreReadTransaction,
    StateStoreWriteTransaction,
    StorageError,
    consensus_models::{
        Block,
        CommittedBlockProof,
        StateVersionProofSource,
        SubstateValueFilterFlags,
        verify_state_version_leaf,
    },
};
use tari_ootle_transaction::Network;
use tari_state_tree::{
    SPARSE_MERKLE_PLACEHOLDER_HASH,
    SpreadPrefixStateTree,
    key_mapper::SpreadPrefixKeyMapper,
    memory_store::MemoryTreeStore,
};

use crate::support::{TEST_NUM_PRESHARDS, Test, TestAddress, logging::setup_logger};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn check_state_transitions() {
    setup_logger();
    let mut test = Test::builder()
        .modify_config(|config_mut| {
            // Epoch change ASAP
            config_mut.epoch_end_grace_period = Duration::from_secs(0);
        })
        .modify_consensus_constants(|config| {
            config.pacemaker_block_time = Duration::from_millis(500);
        })
        .add_committee(0, vec!["1"])
        .start()
        .await;
    let _ignore = test.send_transaction_to_all(Decision::Commit, 100, 1, 10).await;
    let _ignore = test.send_transaction_to_all(Decision::Commit, 200, 1, 1).await;
    let _ignore = test.send_transaction_to_all(Decision::Commit, 1, 1, 1).await;
    let _ignore = test.send_transaction_to_all(Decision::Commit, 100, 1, 10).await;
    test.start_epoch(Epoch(1)).await;

    loop {
        test.on_block_committed().await;

        if test.is_transaction_pool_empty() {
            break;
        }
        let leaf = test.get_validator(&TestAddress::new("1")).get_leaf_block();
        if leaf.height >= NodeHeight(10) {
            panic!("Not all transactions committed after {} blocks", leaf.height);
        }
    }
    // The budget below is relative to where the loop above left off: a commit trails the leaf by a
    // three-chain, so the first block committed in Epoch(2) always arrives several blocks after the
    // epoch starts.
    let epoch_2_deadline = test.get_validator(&TestAddress::new("1")).get_leaf_block().height + NodeHeight(10);
    test.start_epoch(Epoch(2)).await;
    loop {
        let (_, _, epoch, height) = test.on_block_committed().await;

        if epoch == Epoch(2) {
            break;
        }
        if height >= epoch_2_deadline {
            panic!("Epoch(2) not reached by block {height}");
        }
    }

    test.stop();

    test.get_validator(&TestAddress::new("1"))
        .state_store
        .with_read_tx(|tx| {
            let checkpoint = tx
                .epoch_checkpoint_get_all_from_epoch(Epoch(1), 1)
                .unwrap()
                .pop()
                .unwrap();

            for shard in TEST_NUM_PRESHARDS.all_shards_iter() {
                let mut all_transitions = vec![];
                let mut next_state_version = 1;
                while let Some(transitions) = tx
                    .state_transitions_get_starting_at(
                        shard,
                        next_state_version,
                        SubstateValueFilterFlags::all_substates(),
                    )
                    .optional()
                    .unwrap()
                {
                    if transitions.epoch > checkpoint.epoch() {
                        break;
                    }

                    next_state_version = transitions.state_version + 1;
                    all_transitions.push(transitions);
                }
                log::info!(
                    "Shard {}: Found {} transitions until state version {}",
                    shard,
                    all_transitions.len(),
                    next_state_version
                );

                let shard_root = checkpoint.get_shard_root(shard);
                // No state changes
                if shard_root == SPARSE_MERKLE_PLACEHOLDER_HASH {
                    assert!(
                        all_transitions.is_empty(),
                        "Shard {} should have no state transitions",
                        shard
                    );
                } else {
                    assert!(
                        !all_transitions.is_empty(),
                        "Shard {} should have state transitions",
                        shard
                    );
                }

                let mut store = MemoryTreeStore::new();
                let mut tree = tari_state_tree::StateTree::<_, SpreadPrefixKeyMapper>::new(&mut store);
                let values = all_transitions.iter().flat_map(|t| {
                    t.updates
                        .iter()
                        .map(move |u| u.to_tree_change(Network::LocalNet, t.epoch))
                });
                let root = tree.put_substate_changes(None, 1, values).unwrap();
                assert_eq!(root, shard_root, "Shard {} root hash mismatch", shard);
            }

            Ok::<_, HotStuffError>(())
        })
        .unwrap();
    test.assert_clean_shutdown().await;
}

/// Commits a few transactions on a single validator in epoch 1, then stops it.
async fn commit_state_in_epoch_one() -> Test {
    setup_logger();
    let mut test = Test::builder()
        .modify_config(|config_mut| {
            config_mut.epoch_end_grace_period = Duration::from_secs(0);
        })
        .modify_consensus_constants(|config| {
            config.pacemaker_block_time = Duration::from_millis(500);
        })
        .add_committee(0, vec!["1"])
        .start()
        .await;
    let _ignore = test.send_transaction_to_all(Decision::Commit, 100, 1, 10).await;
    let _ignore = test.send_transaction_to_all(Decision::Commit, 200, 1, 1).await;
    test.start_epoch(Epoch(1)).await;

    loop {
        test.on_block_committed().await;
        if test.is_transaction_pool_empty() {
            break;
        }
        let leaf = test.get_validator(&TestAddress::new("1")).get_leaf_block();
        if leaf.height >= NodeHeight(10) {
            panic!("Not all transactions committed after {} blocks", leaf.height);
        }
    }
    test.stop();
    test
}

/// Checks that every committed version of every shard has a proof that this node can serve, and that the proof
/// verifies against the commit proof it is served with. Returns the blocks the proofs name.
fn assert_every_state_version_is_provable<TTx: StateStoreReadTransaction>(tx: &TTx) -> HashSet<BlockId> {
    let mut blocks = HashSet::new();
    for shard in TEST_NUM_PRESHARDS.all_shards_iter() {
        let Some(latest) = tx.state_tree_versions_get_latest(shard).unwrap() else {
            continue;
        };
        let proofs = tx.state_version_proofs_get_range(shard, 1, latest).unwrap();
        assert_eq!(
            proofs.iter().map(|p| p.state_version).collect::<Vec<_>>(),
            (1..=latest).collect::<Vec<_>>(),
            "{shard} must hold a proof for every version a block produced"
        );
        for proof in proofs {
            let StateVersionProofSource::Committed { block_id } = proof.source else {
                panic!("{shard} v{} was not indexed at commit", proof.state_version);
            };
            blocks.insert(block_id);
            let commit_proof = state_version_commit_proof(tx, proof.source)
                .unwrap_or_else(|e| panic!("{shard} v{} cannot be served: {e}", proof.state_version));
            let commit_proof = CommittedBlockProof::from_bytes(&commit_proof).unwrap();
            let mut store = ShardScopedTreeStoreReader::new(tx, shard);
            let shard_root = SpreadPrefixStateTree::new(&mut store)
                .get_root_hash(proof.state_version)
                .unwrap();
            verify_state_version_leaf(
                Network::LocalNet,
                &commit_proof,
                shard,
                proof.state_version,
                &shard_root,
                &proof.shard_root_proof,
            )
            .unwrap();
        }
    }
    assert!(!blocks.is_empty(), "the test committed no state");
    blocks
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn every_committed_state_version_is_provable_against_its_block() {
    let test = commit_state_in_epoch_one().await;
    test.get_validator(&TestAddress::new("1"))
        .state_store
        .with_read_tx(|tx| {
            assert_every_state_version_is_provable(tx);
            Ok::<_, StorageError>(())
        })
        .unwrap();
}

/// A syncing peer asks for proofs of versions from the start of a shard's history, long after epoch GC has pruned the
/// blocks that produced them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn committed_state_versions_stay_provable_after_their_blocks_are_pruned() {
    let test = commit_state_in_epoch_one().await;
    let store = &test.get_validator(&TestAddress::new("1")).state_store;

    // Far enough past epoch 1 that every retention window has passed.
    store.with_write_tx(|tx| tx.epoch_cleanup(Epoch(100))).unwrap();

    store
        .with_read_tx(|tx| {
            let blocks = assert_every_state_version_is_provable(tx);
            for block_id in blocks {
                assert!(
                    Block::get(tx, &block_id).optional()?.is_none(),
                    "block {block_id} was not pruned, so the test does not exercise a pruned block"
                );
            }
            Ok::<_, StorageError>(())
        })
        .unwrap();
}
