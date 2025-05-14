//  Copyright 2022 The Tari Project
//  SPDX-License-Identifier: BSD-3-Clause

use log::*;
use tari_dan_common_types::{committee::Committee, displayable::Displayable, optional::Optional, NodeHeight};
use tari_dan_storage::{
    consensus_models::{Block, HighQc, LastSentVote, LeafBlock, QuorumCertificate},
    StateStore,
};
use tari_sidechain::QuorumDecision;

use crate::{
    hotstuff::{
        calculate_last_dummy_block,
        epoch_state::EpochState,
        get_next_block_height_and_leader,
        pacemaker_handle::PaceMakerHandle,
        HotStuffError,
    },
    messages::{HotstuffMessage, NewViewMessage, VoteMessage},
    traits::{ConsensusSpec, OutboundMessaging, VoteSignatureService},
};

const LOG_TARGET: &str = "tari::dan::consensus::hotstuff::on_next_sync_view";

pub struct OnNextSyncViewHandler<TConsensusSpec: ConsensusSpec> {
    store: TConsensusSpec::StateStore,
    outbound_messaging: TConsensusSpec::OutboundMessaging,
    leader_strategy: TConsensusSpec::LeaderStrategy,
    signature_service: TConsensusSpec::SignatureService,
    pacemaker: PaceMakerHandle,
}

impl<TConsensusSpec: ConsensusSpec> OnNextSyncViewHandler<TConsensusSpec> {
    pub fn new(
        store: TConsensusSpec::StateStore,
        outbound_messaging: TConsensusSpec::OutboundMessaging,
        leader_strategy: TConsensusSpec::LeaderStrategy,
        signature_service: TConsensusSpec::SignatureService,
        pacemaker: PaceMakerHandle,
    ) -> Self {
        Self {
            store,
            outbound_messaging,
            leader_strategy,
            signature_service,
            pacemaker,
        }
    }

    pub async fn handle(
        &mut self,
        epoch_state: &EpochState<TConsensusSpec::Addr>,
        current_height: NodeHeight,
    ) -> Result<(), HotStuffError> {
        let epoch = epoch_state.epoch();
        let (new_height, next_leader, leaf_block, high_qc, last_sent_vote) = self.store.with_read_tx(|tx| {
            let leaf_block = LeafBlock::get(tx, epoch)?.get_block(tx)?;
            let (next_height, next_leader, _) = get_next_block_height_and_leader(
                tx,
                &epoch_state.local_committee,
                &self.leader_strategy,
                leaf_block.id(),
                // Leader failure at current height, so we use the next height
                current_height + NodeHeight(1),
            )?;
            let high_qc = HighQc::get(tx, epoch)?.get_quorum_certificate(tx)?;
            let last_sent_vote = LastSentVote::get(tx)
                .optional()?
                .filter(|vote| high_qc.epoch() == vote.epoch)
                .filter(|vote| high_qc.block_height() < vote.block_height);
            Ok::<_, HotStuffError>((next_height, next_leader, leaf_block, high_qc, last_sent_vote))
        })?;

        if leaf_block.height() == new_height {
            info!(target: LOG_TARGET, "❓️ Leader failure occurred just before we completed processing of the leaf block {leaf_block}. Ignoring.");
            return Ok(());
        }

        // We're moving to a new height i.e. we'll never vote for a previous epoch/height.
        self.pacemaker
            .update_view(epoch, new_height, high_qc.block_height())
            .await?;

        let last_vote = last_sent_vote.map(VoteMessage::from);
        info!(
            target: LOG_TARGET,
            "🌟 Send NEWVIEW {new_height} Vote[{}] HighQC: {high_qc} to {next_leader}",
            last_vote.display()
        );

        let message = generate_new_view::<TConsensusSpec>(
            &self.leader_strategy,
            &self.signature_service,
            epoch_state.local_committee(),
            new_height,
            &leaf_block,
            &high_qc,
            last_vote,
        )?;

        self.outbound_messaging
            .send(next_leader.clone(), HotstuffMessage::NewView(message))
            .await?;

        Ok(())
    }
}

pub fn generate_new_view<TConsensusSpec: ConsensusSpec>(
    leader_strategy: &TConsensusSpec::LeaderStrategy,
    signature_service: &TConsensusSpec::SignatureService,
    local_committee: &Committee<TConsensusSpec::Addr>,
    new_height: NodeHeight,
    leaf_block: &Block,
    high_qc: &QuorumCertificate,
    last_vote: Option<VoteMessage>,
) -> Result<NewViewMessage, HotStuffError> {
    let dummy = calculate_last_dummy_block(
        leaf_block.height(),
        new_height,
        leaf_block.network(),
        leaf_block.epoch(),
        leaf_block.shard_group(),
        *leaf_block.id(),
        high_qc,
        *leaf_block.state_merkle_root(),
        leader_strategy,
        local_committee,
        leaf_block.timestamp(),
        *leaf_block.epoch_hash(),
    )
    .ok_or_else(|| {
        HotStuffError::InvariantError(format!(
            "No dummy block for new height {new_height} with leaf block {leaf_block}"
        ))
    })?;

    let signature = signature_service.sign_vote(dummy.block_id(), QuorumDecision::Accept);

    let message = NewViewMessage {
        high_qc: high_qc.clone(),
        new_height,
        dummy_signature: signature,
        last_vote,
    };
    Ok(message)
}
