//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Tests for transactions that cannot create one of their outputs, because the output exists or another
//! transaction creates it first.

use tari_common_types::types::PrivateKey;
use tari_consensus_types::Decision;
use tari_crypto::tari_utilities::ByteArray;
use tari_engine_types::substate::SubstateId;
use tari_ootle_common_types::{Epoch, InputDeclaration, NodeHeight, SubstateLockType, VersionedSubstateId};
use tari_ootle_storage::consensus_models::TransactionRecord;
use tari_ootle_transaction::Transaction;

use crate::support::{
    ExecuteSpec,
    Test,
    TestAddress,
    TestVnDestination,
    build_substate_id_for_committee,
    logging::setup_logger,
};

const FULL_FEE: u64 = 38;
const FEE_INTENT_FEE: u64 = 19;

/// A local-only transaction that writes `vault` and creates `output`, settling a fee-intent fallback.
fn creating(test: &Test, signer: u8, vault: &VersionedSubstateId, output: &SubstateId) -> TransactionRecord {
    let transaction = Transaction::builder_localnet(Epoch(1))
        .add_input(InputDeclaration::write_versioned(
            vault.substate_id().clone(),
            vault.version(),
        ))
        .build_and_seal(&PrivateKey::from_canonical_bytes(&[signer; 32]).unwrap());
    test.add_execution_at_destination(TestVnDestination::All, ExecuteSpec {
        transaction: transaction.clone(),
        decision: Decision::Commit,
        fee: FULL_FEE,
        input_locks: vec![(vault.substate_id().clone(), SubstateLockType::Write)],
        new_outputs: vec![output.clone()],
        validator_fee_withdrawals: vec![],
        calls_template: None,
        fee_intent_fallback_fee: Some(FEE_INTENT_FEE),
    });
    TransactionRecord::new(transaction)
}

async fn commit_pool(test: &mut Test) {
    loop {
        test.on_block_committed().await;
        if test.is_transaction_pool_empty() {
            break;
        }
        let leaf = test.get_validator(&TestAddress::new("1")).get_leaf_block();
        if leaf.height > NodeHeight(40) {
            test.dump_pool_info();
            panic!("Not all transactions committed after {} blocks", leaf.height);
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
    let creator = creating(&test, 1, &vaults[0], &output);
    let duplicate = creating(&test, 2, &vaults[1], &output);

    test.send_transaction_to_destination(TestVnDestination::All, creator.clone())
        .await;
    test.start_epoch(Epoch(1)).await;
    commit_pool(&mut test).await;
    test.send_transaction_to_destination(TestVnDestination::All, duplicate.clone())
        .await;
    commit_pool(&mut test).await;
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
    let first = creating(&test, 1, &vaults[0], &output);
    let second = creating(&test, 2, &vaults[1], &output);

    test.send_transaction_to_destination(TestVnDestination::All, first.clone())
        .await;
    test.send_transaction_to_destination(TestVnDestination::All, second.clone())
        .await;
    test.start_epoch(Epoch(1)).await;
    commit_pool(&mut test).await;
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
