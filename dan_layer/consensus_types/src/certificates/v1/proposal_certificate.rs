//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt::Display;

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_common_types::types::FixedHash;
use tari_crypto::tari_utilities::ByteArray;
use tari_dan_common_types::{Epoch, NodeHeight};
use tari_hashing::layer2::quorum_certificate_hasher;
use tari_sidechain::QuorumDecision;

use crate::{
    bookkeeping::{HighQc, LeafBlock},
    ids::{BlockId, QcId},
    validator_signature::ValidatorSignature,
};

#[derive(Debug, Clone, Deserialize, Serialize, BorshSerialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
pub struct ProposalCertificate {
    block_height: NodeHeight,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    #[serde(with = "serde_with::hex")]
    header_hash: FixedHash,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    parent_id: BlockId,
    epoch: Epoch,
    signatures: Vec<ValidatorSignature>,
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    decision: QuorumDecision,
}

impl ProposalCertificate {
    pub fn new(
        header_hash: FixedHash,
        parent_id: BlockId,
        block_height: NodeHeight,
        epoch: Epoch,
        signatures: Vec<ValidatorSignature>,
        decision: QuorumDecision,
    ) -> Self {
        Self {
            header_hash,
            parent_id,
            block_height,
            epoch,
            signatures,
            decision,
        }
    }

    pub fn genesis(epoch: Epoch) -> Self {
        Self {
            header_hash: FixedHash::zero(),
            parent_id: BlockId::zero(),
            block_height: NodeHeight::zero(),
            epoch,
            signatures: vec![],
            decision: QuorumDecision::Accept,
        }
    }

    pub fn calculate_id(&self) -> QcId {
        quorum_certificate_hasher().chain(self).finalize_into_array().into()
    }
}

impl ProposalCertificate {
    pub fn justifies_zero_block(&self) -> bool {
        self.header_hash.as_bytes().iter().all(|b| *b == 0)
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn signatures(&self) -> &[ValidatorSignature] {
        &self.signatures
    }

    pub fn block_height(&self) -> NodeHeight {
        self.block_height
    }

    pub fn decision(&self) -> QuorumDecision {
        self.decision
    }

    pub fn calculate_block_id(&self) -> BlockId {
        BlockId::from_parent_and_header_hash(&self.parent_id, &self.header_hash)
    }

    pub fn header_hash(&self) -> &FixedHash {
        &self.header_hash
    }

    pub fn parent_id(&self) -> &BlockId {
        &self.parent_id
    }

    pub fn as_high_qc(&self) -> HighQc {
        HighQc {
            block_id: self.calculate_block_id(),
            block_height: self.block_height,
            epoch: self.epoch,
            qc_id: self.calculate_id(),
        }
    }

    pub fn as_leaf_block(&self) -> LeafBlock {
        LeafBlock {
            block_id: self.calculate_block_id(),
            height: self.block_height,
            epoch: self.epoch,
        }
    }
}

impl Display for ProposalCertificate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "ProposalCertificate(block: {} {}, qc_id: {}, epoch: {}, {} signatures)",
            self.block_height,
            self.calculate_block_id(),
            self.calculate_id(),
            self.epoch,
            self.signatures.len()
        )
    }
}
