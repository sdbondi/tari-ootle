//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_dan_common_types::{Epoch, NodeHeight};
use tari_template_lib::prelude::RistrettoPublicKeyBytes;

use crate::validator_signature::ValidatorSignature;

#[derive(Debug, Clone, Deserialize, Serialize, BorshSerialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
pub struct TimeoutCertificate {
    epoch: Epoch,
    height: NodeHeight,
    /// The sidechain ID of this chain. This is required to avoid potential signature replay between chains.
    /// In the ProposalCertificate, the sidechain ID is already part of the header hash.
    sidechain_id: Option<RistrettoPublicKeyBytes>,
    /// A quorum of validator signatures that sign the timeout certificate.
    signatures: Vec<ValidatorSignature>,
}

impl TimeoutCertificate {
    pub fn new(
        epoch: Epoch,
        height: NodeHeight,
        sidechain_id: Option<RistrettoPublicKeyBytes>,
        signatures: Vec<ValidatorSignature>,
    ) -> Self {
        Self {
            epoch,
            height,
            sidechain_id,
            signatures,
        }
    }

    pub fn epoch(&self) -> Epoch {
        self.epoch
    }

    pub fn height(&self) -> NodeHeight {
        self.height
    }

    pub fn sidechain_id(&self) -> Option<RistrettoPublicKeyBytes> {
        self.sidechain_id
    }

    pub fn signatures(&self) -> &[ValidatorSignature] {
        &self.signatures
    }
}
