//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Tests for the liveness state machine that decides whether leader selection skips a validator's
//! slot.

use std::time::Duration;

use ootle_byte_type::ToByteType;
use tari_consensus_types::{Decision, LastExecuted};
use tari_ootle_common_types::{Epoch, NodeHeight, optional::Optional};
use tari_ootle_storage::{
    StateStore,
    consensus_models::{BookkeepingModel, LivenessState, LivenessThresholds, ValidatorConsensusStats},
};

use crate::support::{Test, TestAddress, TestVnDestination, helpers, logging::setup_logger};

const SUSPEND_AFTER_MISSED: u64 = 2;

/// Thresholds for the tests about being skipped: the wait for a probation slot is longer than the
/// test chain, so a validator that is skipped stays skipped for the whole run.
const STAYS_SUSPENDED: LivenessThresholds = LivenessThresholds {
    suspend_after_missed: SUSPEND_AFTER_MISSED,
    probation_base_votes: 1_000,
    probation_base_blocks: 1_000,
    probation_max_backoff_exp: 6,
};

/// What `observer` has committed about `subject`. Leader selection reads this state, so what matters
/// is what another validator's store says, not what the subject thinks of itself.
fn stats_for(test: &Test, observer: &str, subject: &TestAddress) -> ValidatorConsensusStats {
    let (_, public_key) = helpers::derive_keypair_from_address(subject);
    test.validators()[&TestAddress::new(observer)]
        .state_store()
        .with_read_tx(|tx| ValidatorConsensusStats::get_by_public_key(tx, Epoch(1), &public_key.to_byte_type()))
        .optional()
        .unwrap()
        .unwrap_or_default()
}

/// The state as of what `observer` has committed, which is what leader selection reads (three views
/// further back, so the test reads it slightly ahead of the views it decides).
fn liveness_state_of(
    test: &Test,
    observer: &str,
    subject: &TestAddress,
    thresholds: &LivenessThresholds,
) -> LivenessState {
    let committed = test.validators()[&TestAddress::new(observer)]
        .state_store()
        .with_read_tx(|tx| LastExecuted::get(tx, Epoch(1)))
        .optional()
        .unwrap()
        .map(|last| last.height)
        .unwrap_or_default();
    stats_for(test, observer, subject).liveness_state(thresholds, committed)
}

fn start_test(committee: Vec<&'static str>, thresholds: LivenessThresholds) -> impl Future<Output = Test> {
    Test::builder()
        .with_test_timeout(Duration::from_secs(120))
        .modify_consensus_constants(move |constants| {
            constants.missed_proposal_suspend_threshold = thresholds.suspend_after_missed;
            constants.probation_base_votes = thresholds.probation_base_votes;
            constants.probation_base_blocks = thresholds.probation_base_blocks;
            constants.probation_max_backoff_exp = thresholds.probation_max_backoff_exp;
            constants.pacemaker_block_time = Duration::from_secs(2);
        })
        .add_committee(0, committee)
        .start()
}

/// Keeps transactions flowing so that the leaders that do propose have something to propose, and
/// returns the height committed by each round.
async fn commit_a_block(test: &mut Test) -> NodeHeight {
    let (tx, _, _) = test.send_transaction_to_all(Decision::Commit, 1, 2, 1).await;
    test.wait_for_transaction_seen(TestVnDestination::All, tx.id()).await;
    let (_, _, _, height) = test.on_block_committed().await;
    height
}

/// A validator that stops proposing is charged a missed proposal for each of its views that the
/// committee had to fill with a dummy block, and is suspended once it has missed enough of them.
/// Until its next probation slot comes due it stays suspended, whatever the rest of the committee
/// does.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_absent_validator_is_suspended_and_stays_suspended() {
    setup_logger();
    let mut test = start_test(vec!["1", "2", "3", "4", "5"], STAYS_SUSPENDED).await;

    let absent = TestAddress::new("4");
    test.network().go_offline(absent.clone()).await;
    test.start_epoch(Epoch(1)).await;

    while liveness_state_of(&test, "1", &absent, &STAYS_SUSPENDED) != LivenessState::Suspended {
        let height = commit_a_block(&mut test).await;
        assert!(
            height < NodeHeight(40),
            "{absent} was not suspended after {height} committed blocks"
        );
    }

    let suspended_at = stats_for(&test, "1", &absent);
    assert!(suspended_at.missed_proposals >= SUSPEND_AFTER_MISSED);
    assert_eq!(suspended_at.votes_since_suspended, 0);

    // Nothing the committee does lifts it: only its own participation or the wait does.
    for _ in 0..5 {
        commit_a_block(&mut test).await;
        assert_eq!(
            liveness_state_of(&test, "1", &absent, &STAYS_SUSPENDED),
            LivenessState::Suspended
        );
    }

    test.stop();
    test.assert_clean_shutdown_except(&[absent]).await;
}
