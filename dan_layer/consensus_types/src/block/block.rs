//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::BTreeSet,
    fmt::{Debug, Display, Formatter},
};

use serde::{Deserialize, Serialize};
use tari_common::configuration::Network;
use tari_common_types::types::FixedHash;
use tari_dan_common_types::{
    committee::CommitteeInfo,
    optional::Optional,
    Epoch,
    ExtraData,
    ExtraFieldKey,
    NodeHeight,
    NumPreshards,
    ShardGroup,
};
use tari_template_lib::{prelude::SchnorrSignatureBytes, types::crypto::RistrettoPublicKeyBytes};
use tari_transaction::TransactionId;
#[cfg(feature = "ts")]
use ts_rs::TS;

use super::{BlockHeader, Command, EvictNodeAtom, TransactionAtom};
use crate::{
    certificates::{v1::TimeoutCertificate, ProposalCertificate},
    ids::{BlockId, QcId},
};

const LOG_TARGET: &str = "tari::dan::storage::consensus_types::block";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(TS), ts(export, export_to = "../../bindings/src/types/"))]
pub struct Block {
    header: BlockHeader,
    justify: ProposalCertificate,
    timeout: Option<TimeoutCertificate>,
    /// Commands in the block. These are in canonical order to ensure a deterministic block hash.
    commands: BTreeSet<Command>,
}

impl Block {
    pub fn new(
        header: BlockHeader,
        justify: ProposalCertificate,
        timeout: Option<TimeoutCertificate>,
        commands: BTreeSet<Command>,
    ) -> Self {
        Self {
            header,
            justify,
            timeout,
            commands,
        }
    }

    pub fn calculate_id(&self) -> BlockId {
        self.header.calculate_id()
    }

    pub fn header(&self) -> &BlockHeader {
        &self.header
    }

    pub fn is_genesis(&self) -> bool {
        self.header().is_genesis()
    }

    pub fn is_epoch_end(&self) -> bool {
        self.commands.iter().any(|c| c.is_epoch_end())
    }

    pub fn all_transaction_ids(&self) -> impl Iterator<Item = &TransactionId> + '_ {
        self.commands.iter().filter_map(|d| d.transaction().map(|t| t.id()))
    }

    pub fn all_transaction_ids_in_committee<'a>(
        &'a self,
        committee_info: &'a CommitteeInfo,
    ) -> impl Iterator<Item = &'a TransactionId> + Clone + 'a {
        self.commands
            .iter()
            .filter_map(|cmd| cmd.transaction())
            .filter(|t| t.evidence.has_and_not_empty(&committee_info.shard_group()))
            .map(|t| t.id())
    }

    pub fn all_committing_transactions_ids(&self) -> impl Iterator<Item = &TransactionId> + '_ {
        self.commands.iter().filter_map(|d| d.committing()).map(|t| t.id())
    }

    pub fn all_finalising_transactions_ids(&self) -> impl Iterator<Item = &TransactionId> + '_ {
        self.commands.iter().filter_map(|d| d.finalising()).map(|t| t.id())
    }

    pub fn all_aborting_transaction_ids(&self) -> impl Iterator<Item = &TransactionId> + '_ {
        self.commands.iter().filter_map(|d| d.aborting()).map(|t| t.id())
    }

    pub fn all_foreign_proposals(&self) -> impl Iterator<Item = &ForeignProposalAtom> + '_ {
        self.commands.iter().filter_map(|c| c.foreign_proposal())
    }

    pub fn all_node_evictions(&self) -> impl Iterator<Item = &EvictNodeAtom> + '_ {
        self.commands.iter().filter_map(|c| c.evict_node())
    }

    pub fn all_confidential_output_mints(&self) -> impl Iterator<Item = &MintConfidentialOutputAtom> + '_ {
        self.commands.iter().filter_map(|c| c.mint_confidential_output())
    }

    pub fn all_local_accept(&self) -> impl Iterator<Item = &TransactionAtom> + '_ {
        self.commands.iter().filter_map(|c| c.local_accept())
    }

    pub fn command_count(&self) -> usize {
        self.commands.len()
    }

    pub fn as_locked_block(&self) -> LockedBlock {
        self.header().as_locked_block()
    }

    pub fn as_last_executed(&self) -> LastExecuted {
        self.header().as_last_executed()
    }

    pub fn as_last_voted(&self) -> LastVoted {
        self.header().as_last_voted()
    }

    pub fn as_leaf_block(&self) -> LeafBlock {
        self.header().as_leaf_block()
    }

    pub fn as_last_proposed(&self) -> LastProposed {
        LastProposed {
            height: self.header.height(),
            block_id: *self.id(),
            epoch: self.header.epoch(),
        }
    }

    pub fn id(&self) -> &BlockId {
        self.header.id()
    }

    pub fn network(&self) -> Network {
        self.header.network()
    }

    pub fn parent(&self) -> &BlockId {
        self.header.parent()
    }

    pub fn justify(&self) -> &QuorumCertificate {
        &self.justify
    }

    pub fn into_justify(self) -> QuorumCertificate {
        self.justify
    }

    pub fn justifies_parent(&self) -> bool {
        self.justify.block_id() == self.parent()
    }

    pub fn height(&self) -> NodeHeight {
        self.header.height()
    }

    pub fn epoch(&self) -> Epoch {
        self.header.epoch()
    }

    pub fn shard_group(&self) -> ShardGroup {
        self.header.shard_group()
    }

    pub fn total_leader_fee(&self) -> u64 {
        self.header.total_leader_fee()
    }

    pub fn calculate_total_transaction_fee(&self) -> u64 {
        self.commands
            .iter()
            .filter_map(|c| c.committing())
            .map(|atom| atom.transaction_fee)
            .sum()
    }

    pub fn proposed_by(&self) -> &RistrettoPublicKeyBytes {
        self.header.proposed_by()
    }

    pub fn state_merkle_root(&self) -> &FixedHash {
        self.header.state_merkle_root()
    }

    pub fn command_merkle_root(&self) -> &FixedHash {
        self.header.command_merkle_root()
    }

    pub fn commands(&self) -> &BTreeSet<Command> {
        &self.commands
    }

    pub fn into_commands(self) -> BTreeSet<Command> {
        self.commands
    }

    pub fn is_dummy(&self) -> bool {
        self.header.is_dummy()
    }

    pub fn is_justified(&self) -> bool {
        self.justify_qc_id.is_some()
    }

    pub fn justify_qc_id(&self) -> Option<QcId> {
        self.justify_qc_id
    }

    pub fn is_committed(&self) -> bool {
        self.commit_qc_id.is_some()
    }

    pub fn block_time(&self) -> Option<u64> {
        self.block_time
    }

    pub fn timestamp(&self) -> u64 {
        self.header.timestamp()
    }

    pub fn signature(&self) -> Option<&SchnorrSignatureBytes> {
        self.header.signature()
    }

    pub fn epoch_hash(&self) -> &FixedHash {
        self.header.epoch_hash()
    }

    pub fn extra_data(&self) -> &ExtraData {
        self.header.extra_data()
    }

    pub fn set_justify_qc(&mut self, justify_qc_id: QcId) {
        self.justify_qc_id = Some(justify_qc_id);
    }

    pub fn set_commit_qc(&mut self, commit_qc_id: QcId) {
        self.commit_qc_id = Some(commit_qc_id);
    }
}

impl Display for Block {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        if self.is_dummy() {
            write!(f, "Dummy")?;
        }
        write!(
            f,
            "[{}, justify: {}/{} ({}), {}, {}, {} cmd(s), {}->{}]",
            self.height(),
            self.justify().block_height(),
            self.justify().epoch(),
            if self.justifies_parent() { "🟢" } else { "🟡" },
            self.epoch(),
            self.shard_group(),
            self.commands().len(),
            self.id(),
            self.parent()
        )
    }
}
