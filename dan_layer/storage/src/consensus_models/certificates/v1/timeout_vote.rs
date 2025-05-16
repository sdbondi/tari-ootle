//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::hash::Hash;

use tari_common_types::types::FixedHash;
use tari_dan_common_types::{Epoch, NodeHeight};
use tari_hashing::layer2::timeout_signature_hasher;
use tari_sidechain::QuorumDecision;
use tari_template_lib::prelude::RistrettoPublicKeyBytes;

use crate::{
    certificates::{SignedMessage, Vote},
    validator_signature::ValidatorSignature,
};

#[derive(Debug, Clone)]
pub struct TimeoutVote {
    pub epoch: Epoch,
    pub height: NodeHeight,
    pub sidechain_id: Option<RistrettoPublicKeyBytes>,
    pub signature: ValidatorSignature,
}

impl TimeoutVote {
    pub fn signature(&self) -> &ValidatorSignature {
        &self.signature
    }
}

impl Vote for TimeoutVote {
    fn epoch(&self) -> Epoch {
        self.epoch
    }

    fn height(&self) -> NodeHeight {
        self.height
    }

    fn decision(&self) -> QuorumDecision {
        QuorumDecision::Accept
    }
}

impl SignedMessage for TimeoutVote {
    fn message(&self) -> FixedHash {
        timeout_signature_hasher()
            .chain(&self.epoch)
            .chain(&self.height)
            .chain(&self.sidechain_id)
            .finalize()
            .into()
    }

    fn signature(&self) -> &ValidatorSignature {
        &self.signature
    }
}
