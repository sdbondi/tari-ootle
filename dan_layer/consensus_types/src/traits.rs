//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_common_types::types::FixedHash;
use tari_dan_common_types::{Epoch, NodeHeight};
use tari_sidechain::QuorumDecision;

use crate::validator_signature::ValidatorSignature;

pub trait Certificate {
    type Vote: Vote;
}

pub trait SignedMessage {
    fn message(&self) -> FixedHash;
    fn signature(&self) -> &ValidatorSignature;

    fn is_valid(&self) -> bool {
        self.signature().verify(self.message())
    }
}

pub trait Vote: SignedMessage {
    fn epoch(&self) -> Epoch;
    fn height(&self) -> NodeHeight;
    fn decision(&self) -> QuorumDecision;
}
