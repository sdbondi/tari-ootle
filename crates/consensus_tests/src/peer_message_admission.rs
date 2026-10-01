//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashSet,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use tari_consensus::{
    messages::{ForeignProposalRequestMessage, HotstuffMessage, MissingTransactionsRequest},
    traits::InboundMessagingError,
};
use tari_consensus_types::Decision;
use tari_ootle_common_types::{Epoch, NodeHeight};
use tari_ootle_transaction::TransactionId;
use tokio::time::sleep;

use crate::support::{Test, TestAddress, logging::setup_logger};

/// Commits blocks until the transaction pool drains, failing if it takes more than ten blocks.
async fn commit_until_pool_is_empty(test: &mut Test, address: &TestAddress) {
    loop {
        test.on_block_committed().await;
        if test.is_transaction_pool_empty() {
            break;
        }
        let leaf = test.get_validator(address).get_leaf_block();
        assert!(
            leaf.height < NodeHeight(10),
            "Transaction not committed after {} blocks",
            leaf.height
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_malformed_inbound_message_does_not_interrupt_consensus() {
    setup_logger();
    let mut test = Test::builder().add_committee(0, vec!["1"]).start().await;
    test.start_epoch(Epoch(1)).await;
    test.wait_for_all_validators_to_start_consensus().await;

    let address = TestAddress::new("1");
    let validator = test.get_validator_mut(&address);
    let mut state = validator.current_state_machine_state.clone();
    state.mark_unchanged();
    validator
        .tx_malformed_message
        .send(InboundMessagingError::InvalidMessage {
            reason: "Message is missing".to_string(),
        })
        .await
        .unwrap();

    let (tx, _, _) = test.send_transaction_to_all(Decision::Commit, 1, 1, 1).await;
    commit_until_pool_is_empty(&mut test, &address).await;

    assert!(
        !state.has_changed().unwrap(),
        "consensus state machine left Running (now {})",
        *state.borrow()
    );
    test.stop();
    test.assert_all_validators_committed(tx.id());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn requests_from_an_unregistered_peer_are_not_served() {
    setup_logger();
    let outsider = TestAddress::new("outsider");
    let replies_to_outsider = Arc::new(AtomicUsize::new(0));
    let mut test = Test::builder()
        .add_committee(0, vec!["1"])
        .add_committee(1, vec!["2"])
        .with_message_filter(Box::new({
            let outsider = outsider.clone();
            let replies_to_outsider = replies_to_outsider.clone();
            move |_from, to, _msg| {
                if *to == outsider {
                    replies_to_outsider.fetch_add(1, Ordering::SeqCst);
                    return false;
                }
                true
            }
        }))
        .start()
        .await;
    test.start_epoch(Epoch(1)).await;

    let address = TestAddress::new("1");
    test.wait_for_all_validators_to_start_consensus().await;

    let foreign_shard_group = test.get_validator(&TestAddress::new("2")).shard_group;
    let validator = test.get_validator(&address);
    let block_id = validator.get_leaf_block().block_id;
    for msg in [
        HotstuffMessage::MissingTransactionsRequest(MissingTransactionsRequest {
            request_id: 0,
            epoch: Epoch(1),
            block_id,
            transactions: HashSet::from([TransactionId::from([1u8; 32])]),
        }),
        HotstuffMessage::ForeignProposalRequest(ForeignProposalRequestMessage::ByBlockId {
            block_id,
            for_shard_group: foreign_shard_group,
            epoch: Epoch(1),
        }),
    ] {
        validator
            .tx_inbound_message
            .send((outsider.clone(), msg))
            .await
            .unwrap();
    }

    // Requests are served off the worker loop within milliseconds, so this leaves ample time for a reply.
    sleep(Duration::from_secs(2)).await;
    test.stop();

    assert_eq!(replies_to_outsider.load(Ordering::SeqCst), 0);
}
