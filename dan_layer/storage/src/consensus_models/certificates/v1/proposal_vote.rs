//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::hash::{DefaultHasher, Hash, Hasher};

use tari_common_types::types::FixedHash;
use tari_dan_common_types::{Epoch, NodeHeight};
use tari_hashing::layer2::vote_signature_hasher;
use tari_sidechain::QuorumDecision;

use crate::{
    certificates::{SignedMessage, Vote},
    ids::BlockId,
    validator_signature::ValidatorSignature,
};

#[derive(Debug, Clone)]
pub struct ProposalVote {
    pub epoch: Epoch,
    pub block_id: BlockId,
    pub block_height: NodeHeight,
    pub decision: QuorumDecision,
    pub sender_leaf_hash: FixedHash,
    pub signature: ValidatorSignature,
}

impl ProposalVote {
    /// Returns a SIPHASH hash used to uniquely identify this vote
    pub fn get_hash(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.hash(&mut hasher);
        hasher.finish()
    }
}

impl Vote for ProposalVote {
    fn epoch(&self) -> Epoch {
        self.epoch
    }

    fn height(&self) -> NodeHeight {
        self.block_height
    }

    fn decision(&self) -> QuorumDecision {
        self.decision
    }

    fn sender_hash(&self) -> &FixedHash {
        &self.sender_leaf_hash
    }
}

impl SignedMessage for ProposalVote {
    fn message(&self) -> FixedHash {
        vote_signature_hasher()
            .chain(&self.block_id)
            .chain(&self.decision)
            .finalize()
            .into()
    }

    fn signature(&self) -> &ValidatorSignature {
        &self.signature
    }
}

impl Hash for ProposalVote {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.epoch.hash(state);
        self.block_id.hash(state);
        self.block_height.hash(state);
        // QuorumDecision does not implement Hash
        state.write_u8(self.decision.as_u8());
        self.sender_leaf_hash.hash(state);
        self.signature.hash(state);
    }
}
