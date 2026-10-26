//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Tests for transactions that cannot create one of their outputs, because the output exists or another
//! transaction creates it first.

use std::{collections::HashSet, iter};

use tari_common_types::types::PrivateKey;
use tari_consensus_types::Decision;
use tari_engine_types::substate::SubstateId;
use tari_ootle_common_types::{
    Epoch,
    InputDeclaration,
    NodeHeight,
    ShardGroup,
    SubstateLockType,
    SubstateVersion,
    VersionedSubstateId,
    VersionedSubstateIdRef,
};
use tari_ootle_storage::consensus_models::TransactionRecord;
use tari_ootle_transaction::Transaction;

use crate::support::{
    ExecuteSpec,
    TEST_NUM_PRESHARDS,
    Test,
    TestAddress,
    TestVnDestination,
    build_substate_id_for_committee,
    helpers::random_substate_in_shard_group,
    logging::setup_logger,
};

const FULL_FEE: u64 = 38;
const FEE_INTENT_FEE: u64 = 19;

/// A local-only transaction that writes `vault` and creates `outputs`, settling a fee-intent fallback.
fn creating(test: &Test, signer: u8, vault: &VersionedSubstateId, outputs: &[SubstateId]) -> TransactionRecord {
    let transaction = Transaction::builder_localnet(Epoch(1))
        .add_input(InputDeclaration::write_versioned(
            vault.substate_id().clone(),
            vault.version(),
        ))
        .build_and_seal(&PrivateKey::from(u64::from(signer)));
    test.add_execution_at_destination(TestVnDestination::All, ExecuteSpec {
        transaction: transaction.clone(),
        decision: Decision::Commit,
        fee: FULL_FEE,
        input_locks: vec![(vault.substate_id().clone(), SubstateLockType::Write)],
        new_outputs: outputs.to_vec(),
        validator_fee_withdrawals: vec![],
        calls_template: None,
        fee_intent_fallback_fee: Some(FEE_INTENT_FEE),
    });
    TransactionRecord::new(transaction)
}

/// Waits until every validator has finalized every one of `transactions`.
async fn wait_for_finalized(test: &mut Test, transactions: &[&TransactionRecord]) {
    wait_for_finalized_at(test, TestVnDestination::All, transactions).await;
}

/// Waits until every validator at `destination` has finalized every one of `transactions`.
async fn wait_for_finalized_at(test: &mut Test, destination: TestVnDestination, transactions: &[&TransactionRecord]) {
    loop {
        test.on_block_committed().await;
        if transactions.iter().all(|transaction| {
            test.validators_iter()
                .filter(|validator| destination.is_for_vn(validator))
                .all(|validator| validator.has_finalized(transaction.id()))
        }) {
            return;
        }
        let leaf = test.get_validator(&TestAddress::new("1")).get_leaf_block();
        if leaf.height > NodeHeight(40) {
            test.dump_pool_info();
            panic!("Not all transactions finalized after {} blocks", leaf.height);
        }
    }
}

/// Whether every validator finalized `transaction` committing only its fee intent.
fn is_fee_intent_commit_everywhere(test: &Test, transaction: &TransactionRecord) -> bool {
    test.validators_iter().all(|validator| {
        let execution = validator.get_transaction_execution(transaction.id());
        let is_fee_only = execution.result().finalize.is_fee_only();
        if is_fee_only {
            assert_eq!(execution.transaction_fee(), FEE_INTENT_FEE, "{}", validator.address);
        }
        is_fee_only
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transaction_creating_an_output_that_exists_commits_its_fee_intent() {
    setup_logger();
    let mut test = Test::builder().add_committee(0, vec!["1", "2", "3", "4"]).start().await;
    let vaults = test.create_substates_on_vns(TestVnDestination::All, 2);
    let output = build_substate_id_for_committee(0, 1);
    let creator = creating(&test, 1, &vaults[0], std::slice::from_ref(&output));
    let duplicate = creating(&test, 2, &vaults[1], std::slice::from_ref(&output));

    test.send_transaction_to_destination(TestVnDestination::All, creator.clone())
        .await;
    test.start_epoch(Epoch(1)).await;
    wait_for_finalized(&mut test, &[&creator]).await;
    test.send_transaction_to_destination(TestVnDestination::All, duplicate.clone())
        .await;
    wait_for_finalized(&mut test, &[&duplicate]).await;
    test.stop();

    test.assert_all_validators_have_decision(creator.id(), Decision::Commit)
        .await;
    test.assert_all_validators_have_decision(duplicate.id(), Decision::Commit)
        .await;
    assert!(!is_fee_intent_commit_everywhere(&test, &creator));
    assert!(is_fee_intent_commit_everywhere(&test, &duplicate));

    test.assert_clean_shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn of_two_transactions_creating_one_output_together_the_second_commits_its_fee_intent() {
    setup_logger();
    let mut test = Test::builder().add_committee(0, vec!["1", "2", "3", "4"]).start().await;
    let vaults = test.create_substates_on_vns(TestVnDestination::All, 2);
    let output = build_substate_id_for_committee(0, 1);
    let first = creating(&test, 1, &vaults[0], std::slice::from_ref(&output));
    let second = creating(&test, 2, &vaults[1], std::slice::from_ref(&output));

    test.send_transaction_to_destination(TestVnDestination::All, first.clone())
        .await;
    test.send_transaction_to_destination(TestVnDestination::All, second.clone())
        .await;
    test.start_epoch(Epoch(1)).await;
    wait_for_finalized(&mut test, &[&first, &second]).await;
    test.stop();

    test.assert_all_validators_have_decision(first.id(), Decision::Commit)
        .await;
    test.assert_all_validators_have_decision(second.id(), Decision::Commit)
        .await;
    // Whichever the block sequences first creates the output, and the other pays for its execution
    let fee_intent_only = [&first, &second]
        .into_iter()
        .filter(|transaction| is_fee_intent_commit_everywhere(&test, transaction))
        .count();
    assert_eq!(fee_intent_only, 1);

    test.assert_clean_shutdown().await;
}

/// A transaction the proposer leaves out of a block holds no locks in it, as on replicas, which never prepare it.
/// Otherwise its OUTPUT lock would make the proposer derive a fee-intent commit for a later transaction creating the
/// same output, which replicas commit in full, and the block would get no votes.
///
/// A proposal prepares its transactions in a random order, so each pair only exercises this when the oversized one
/// comes first. With enough pairs, nearly every proposal has one that does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_transaction_the_proposer_defers_does_not_take_its_outputs_from_the_block() {
    const NUM_PAIRS: usize = 10;
    setup_logger();
    let mut test = Test::builder()
        .add_committee(0, vec!["1", "2", "3", "4"])
        .with_max_block_shard_output_bytes(1_000)
        .start()
        .await;
    let vaults = test.create_substates_on_vns(TestVnDestination::All, 2 * NUM_PAIRS);
    let mut creators = Vec::with_capacity(NUM_PAIRS);
    for (pair, vaults) in vaults.chunks(2).enumerate() {
        let output = build_substate_id_for_committee(0, 1);
        // Every output of the oversized transaction lands in the shard of `output`, so its diff never fits the budget
        let shard = VersionedSubstateIdRef::new(&output, SubstateVersion::ZERO).to_shard(TEST_NUM_PRESHARDS);
        let oversized_outputs = iter::once(output.clone())
            .chain(
                iter::repeat_with(|| random_substate_in_shard_group(ShardGroup::new(shard, shard), TEST_NUM_PRESHARDS))
                    .take(40),
            )
            .collect::<Vec<_>>();
        let signer = u8::try_from(2 * pair + 1).unwrap();
        let oversized = creating(&test, signer, &vaults[0], &oversized_outputs);
        let creator = creating(&test, signer + 1, &vaults[1], std::slice::from_ref(&output));
        test.send_transaction_to_destination(TestVnDestination::All, oversized)
            .await;
        test.send_transaction_to_destination(TestVnDestination::All, creator.clone())
            .await;
        creators.push(creator);
    }

    test.start_epoch(Epoch(1)).await;
    wait_for_finalized(&mut test, &creators.iter().collect::<Vec<_>>()).await;
    test.stop();

    for creator in &creators {
        for validator in test.validators_iter() {
            let execution = validator.get_transaction_execution(creator.id());
            assert!(
                execution.result().finalize.is_full_accept(),
                "{}: {}",
                validator.address,
                execution.result().finalize.result
            );
        }
    }

    test.assert_clean_shutdown().await;
}

/// A shard group that finds a transaction's output already exists commits its fee intent, and so does every other
/// shard group involved, including one that accepted the full transaction first.
async fn assert_a_multishard_collision_commits_the_fee_intent_everywhere(colliding_committee: u32) {
    setup_logger();
    let mut test = Test::builder()
        .add_committee(0, vec!["1", "2"])
        .add_committee(1, vec!["3", "4"])
        .start()
        .await;
    let creator_vault = test
        .create_substates_on_vns(TestVnDestination::Committee(colliding_committee), 1)
        .remove(0);
    let vault = test
        .create_substates_on_vns(TestVnDestination::Committee(0), 1)
        .remove(0);
    let output = test.build_outputs_for_committee(colliding_committee, 1).remove(0);
    let fresh_output = test.build_outputs_for_committee(1, 1).remove(0);
    let creator = creating(&test, 1, &creator_vault, std::slice::from_ref(&output));
    let duplicate = creating(&test, 2, &vault, &[output, fresh_output]);

    test.send_transaction_to_destination(TestVnDestination::All, creator.clone())
        .await;
    test.start_epoch(Epoch(1)).await;
    wait_for_finalized_at(&mut test, TestVnDestination::Committee(colliding_committee), &[
        &creator,
    ])
    .await;
    test.send_transaction_to_destination(TestVnDestination::All, duplicate.clone())
        .await;
    wait_for_finalized(&mut test, &[&duplicate]).await;
    test.stop();

    test.assert_all_validators_have_decision(duplicate.id(), Decision::Commit)
        .await;
    assert!(is_fee_intent_commit_everywhere(&test, &duplicate));
    let reject_reasons = test
        .validators_iter()
        .map(|validator| {
            let execution = validator.get_transaction_execution(duplicate.id());
            execution.result().finalize.result.any_reject().map(ToString::to_string)
        })
        .collect::<HashSet<_>>();
    assert_eq!(reject_reasons.len(), 1, "{reject_reasons:?}");

    test.assert_clean_shutdown().await;
}

/// The output-only shard group finds the collision when it prepares, after the input shard group prepared the full
/// transaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_output_only_shard_group_creating_an_output_that_exists_commits_the_fee_intent_everywhere() {
    assert_a_multishard_collision_commits_the_fee_intent_everywhere(1).await;
}

/// The input shard group finds the collision at LocalAccept, which the output-only shard group may already have sent
/// for the full transaction.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_input_shard_group_creating_an_output_that_exists_commits_the_fee_intent_everywhere() {
    assert_a_multishard_collision_commits_the_fee_intent_everywhere(0).await;
}
