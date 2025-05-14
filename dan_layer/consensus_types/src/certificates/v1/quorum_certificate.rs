//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt::Display;

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_dan_common_types::{optional::Optional, Epoch, NodeHeight};

use super::{ProposalCertificate, TimeoutCertificate};
use crate::{
    ids::{BlockId, QcId},
    validator_signature::ValidatorSignature,
};

#[derive(Debug, Clone, Deserialize, Serialize, BorshSerialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
pub enum QuorumCertificateV1 {
    ProposalCertificate(ProposalCertificate),
    TimeoutCertificate(TimeoutCertificate),
}

impl QuorumCertificateV1 {
    pub fn epoch(&self) -> Epoch {
        match self {
            Self::ProposalCertificate(pc) => pc.epoch(),
            Self::TimeoutCertificate(tc) => tc.epoch(),
        }
    }

    pub fn height(&self) -> NodeHeight {
        match self {
            Self::ProposalCertificate(pc) => pc.block_height(),
            Self::TimeoutCertificate(tc) => tc.height(),
        }
    }

    pub fn signatures(&self) -> &[ValidatorSignature] {
        match self {
            Self::ProposalCertificate(pc) => pc.signatures(),
            Self::TimeoutCertificate(tc) => tc.signatures(),
        }
    }

    pub fn as_proposal_certificate(&self) -> Option<&ProposalCertificate> {
        match self {
            Self::ProposalCertificate(pc) => Some(pc),
            Self::TimeoutCertificate(_) => None,
        }
    }

    pub fn as_timeout_certificate(&self) -> Option<&TimeoutCertificate> {
        match self {
            Self::ProposalCertificate(_) => None,
            Self::TimeoutCertificate(tc) => Some(tc),
        }
    }
}

impl From<ProposalCertificate> for QuorumCertificateV1 {
    fn from(pc: ProposalCertificate) -> Self {
        Self::ProposalCertificate(pc)
    }
}

impl From<TimeoutCertificate> for QuorumCertificateV1 {
    fn from(tc: TimeoutCertificate) -> Self {
        Self::TimeoutCertificate(tc)
    }
}
