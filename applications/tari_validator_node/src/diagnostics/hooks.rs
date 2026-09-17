//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_consensus::{
    hotstuff::{ConsensusCurrentState, ConsensusStateEvent, HotStuffError, ProposalValidationError},
    messages::HotstuffMessage,
    traits::hooks::ConsensusHooks,
};
use tari_consensus_types::BlockId;
use tari_ootle_common_types::{
    NodeHeight,
    diag_event,
    diagnostics::{DiagnosticEvent, DiagnosticLevel},
};
use tari_ootle_storage::consensus_models::{Block, NoVoteReason, ValidBlock};
use tari_ootle_transaction::TransactionId;

use crate::diagnostics::handle::DiagnosticsHandle;

/// Records the consensus events worth knowing about into the diagnostic event log. The hooks that
/// fire on the happy path (every message, every committed block, every ready transaction) stay
/// empty: only the abnormal is recorded.
#[derive(Debug, Clone)]
pub struct DiagnosticHooks {
    diagnostics: DiagnosticsHandle,
}

impl DiagnosticHooks {
    pub fn new(diagnostics: DiagnosticsHandle) -> Self {
        Self { diagnostics }
    }
}

impl ConsensusHooks for DiagnosticHooks {
    fn on_local_block_committed(&mut self, _block: &ValidBlock) {}

    fn on_blocks_committed(&mut self, _committed_blocks: &[Block]) {}

    fn on_block_validation_failed<E: ToString>(&mut self, err: &E) {
        self.diagnostics.emit(diag_event!(
            warn,
            "consensus.block_validation_failed",
            "A proposal failed validation",
            error => err.to_string()
        ));
    }

    fn on_message_received(&mut self, _message: &HotstuffMessage) {}

    fn on_error(&mut self, err: &HotStuffError) {
        let (topic, level) = classify_error(err);
        self.diagnostics
            .emit(DiagnosticEvent::new(level, topic, err.to_string()).with_field("error", err));
    }

    fn on_pacemaker_height_changed(&mut self, _height: NodeHeight) {}

    fn on_leader_timeout(&mut self, new_height: NodeHeight) {
        self.diagnostics.emit(diag_event!(
            warn,
            "consensus.leader_failure",
            "The leader failed to propose. Moving to height {new_height}",
            new_height => new_height
        ));
    }

    fn on_needs_sync(&mut self, local_height: NodeHeight, remote_qc_height: NodeHeight) {
        self.diagnostics.emit(diag_event!(
            warn,
            "consensus.needs_sync",
            "Behind the committee at height {local_height} (peer certificate at {remote_qc_height})",
            local_height => local_height,
            remote_qc_height => remote_qc_height
        ));
    }

    fn on_state_transition(
        &mut self,
        from: ConsensusCurrentState,
        to: ConsensusCurrentState,
        event: &ConsensusStateEvent,
    ) {
        let (topic, level) = classify(from, to, event);
        self.diagnostics.emit(
            DiagnosticEvent::new(level, topic, format!("Consensus moved from {from} to {to} ({event})"))
                .with_field("from", from)
                .with_field("to", to)
                .with_field("event", event),
        );
    }

    fn on_no_vote(&mut self, block_id: &BlockId, reason: &NoVoteReason) {
        self.diagnostics.emit(diag_event!(
            warn,
            "consensus.no_vote",
            "Did not vote on block {block_id}: {reason}",
            block_id => block_id,
            reason => reason
        ));
    }

    fn on_transaction_ready(&mut self, _tx_id: &TransactionId) {}

    fn on_transaction_batch_finalized(&mut self, _num_committed: usize, _num_aborted: usize) {}
}

/// Mirrors how consensus itself treats each error, so that `level = error` means something an
/// operator should look at rather than a condition consensus already handles.
///
/// `handle_hotstuff_error` reports every error it sees, including the ones it goes on to resolve by
/// catching up, and it resolves those once per proposal for as long as the node is behind.
fn classify_error(err: &HotStuffError) -> (&'static str, DiagnosticLevel) {
    // These two stall consensus on this node until an operator intervenes, and consensus raises its
    // own alarm for each.
    if matches!(
        err,
        HotStuffError::ProposalValidationError(
            ProposalValidationError::InvalidEpochHash { .. } | ProposalValidationError::InvalidProtocolVersion { .. }
        )
    ) {
        return ("consensus.error", DiagnosticLevel::Error);
    }

    // A missing justify block puts the node on the same catch-up path as an explicit
    // `FallenBehind`, even though it is not part of `is_sync_required`.
    if err.is_sync_required() ||
        matches!(
            err,
            HotStuffError::ProposalValidationError(ProposalValidationError::JustifyBlockNotFound { .. })
        )
    {
        return ("consensus.needs_sync", DiagnosticLevel::Warn);
    }

    if matches!(err, HotStuffError::ProposalValidationError(_)) {
        return ("consensus.block_validation_failed", DiagnosticLevel::Warn);
    }

    ("consensus.error", DiagnosticLevel::Error)
}

fn classify(
    from: ConsensusCurrentState,
    to: ConsensusCurrentState,
    event: &ConsensusStateEvent,
) -> (&'static str, DiagnosticLevel) {
    use tari_ootle_common_types::diagnostics::DiagnosticLevel::{Error, Info};

    match (from, to, event) {
        // `Sleeping` is only ever entered from a failure, and is where an operator looks first when
        // a node has gone quiet.
        (_, ConsensusCurrentState::Sleeping, _) => ("consensus.crashed", Error),
        (_, ConsensusCurrentState::Syncing, _) => ("sync.started", Info),
        // The arms above have already taken the failure path out of `Syncing`, so a shutdown is the
        // only remaining way to leave it other than by finishing.
        (ConsensusCurrentState::Syncing, ConsensusCurrentState::Shutdown, _) => ("consensus.state_transition", Info),
        (ConsensusCurrentState::Syncing, _, _) => ("sync.completed", Info),
        _ => ("consensus.state_transition", Info),
    }
}
