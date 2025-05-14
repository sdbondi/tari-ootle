//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt::Display;

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_common_types::types::FixedHash;
use tari_dan_common_types::{optional::Optional, Epoch, NodeHeight};
use tari_hashing::layer2::quorum_certificate_hasher;
use tari_sidechain::QuorumDecision;
use tari_template_lib::prelude::RistrettoPublicKeyBytes;

use crate::{
    certificates::v1::{ProposalCertificate, QuorumCertificateV1, TimeoutCertificate},
    ids::{BlockId, QcId},
    validator_signature::ValidatorSignature,
};

#[derive(Debug, Clone, Deserialize, Serialize, BorshSerialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
pub enum QuorumCertificate {
    V1(QuorumCertificateV1),
}

impl QuorumCertificate {
    pub fn new_proposal_certificate(
        header_hash: FixedHash,
        parent_id: BlockId,
        block_height: NodeHeight,
        epoch: Epoch,
        signatures: Vec<ValidatorSignature>,
        decision: QuorumDecision,
    ) -> Self {
        QuorumCertificateV1::ProposalCertificate(ProposalCertificate::new(
            header_hash,
            parent_id,
            block_height,
            epoch,
            signatures,
            decision,
        ))
        .into()
    }

    pub fn new_timeout_certificate(
        epoch: Epoch,
        height: NodeHeight,
        sidechain_id: Option<RistrettoPublicKeyBytes>,
        signatures: Vec<ValidatorSignature>,
    ) -> Self {
        QuorumCertificateV1::TimeoutCertificate(TimeoutCertificate::new(epoch, height, sidechain_id, signatures)).into()
    }

    pub fn as_proposal_certificate(&self) -> Option<&ProposalCertificate> {
        match self {
            Self::V1(qc) => match qc {
                QuorumCertificateV1::ProposalCertificate(pc) => Some(pc),
                QuorumCertificateV1::TimeoutCertificate(_) => None,
            },
        }
    }

    pub fn as_timeout_certificate(&self) -> Option<&TimeoutCertificate> {
        match self {
            Self::V1(qc) => match qc {
                QuorumCertificateV1::ProposalCertificate(_) => None,
                QuorumCertificateV1::TimeoutCertificate(tc) => Some(tc),
            },
        }
    }

    pub fn epoch(&self) -> Epoch {
        match self {
            Self::V1(qc) => match qc {
                QuorumCertificateV1::ProposalCertificate(pc) => pc.epoch(),
                QuorumCertificateV1::TimeoutCertificate(tc) => tc.epoch(),
            },
        }
    }

    pub fn height(&self) -> NodeHeight {
        match self {
            Self::V1(qc) => match qc {
                QuorumCertificateV1::ProposalCertificate(pc) => pc.block_height(),
                QuorumCertificateV1::TimeoutCertificate(tc) => tc.height(),
            },
        }
    }

    pub fn calculate_id(&self) -> QcId {
        quorum_certificate_hasher().chain(self).finalize_into_array().into()
    }
}

impl From<QuorumCertificateV1> for QuorumCertificate {
    fn from(qc: QuorumCertificateV1) -> Self {
        Self::V1(qc)
    }
}
