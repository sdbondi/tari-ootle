//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt::{Display, Formatter};

use minicbor::{CborLen, Decode, Encode};
use serde::{Deserialize, Serialize};
use tari_consensus_types::{ProposalVote, TimeoutVote};
use tari_ootle_common_types::{Epoch, NodeHeight, diagnostics::unix_millis_now};
use tari_template_lib_types::crypto::RistrettoPublicKeyBytes;

/// The two conflicting votes, kept whole so that the record stands on its own: a third party can
/// rebuild both signed preimages from it and check the signatures without consulting any other
/// state. Both votes carry the same signer, epoch and height; they differ in what they attest to.
#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode, CborLen)]
pub enum EquivocatingVotes {
    #[n(0)]
    Proposal {
        #[n(0)]
        first: ProposalVote,
        #[n(1)]
        second: ProposalVote,
    },
    #[n(1)]
    Timeout {
        #[n(0)]
        first: TimeoutVote,
        #[n(1)]
        second: TimeoutVote,
    },
}

impl EquivocatingVotes {
    pub fn kind(&self) -> VoteEquivocationKind {
        match self {
            Self::Proposal { .. } => VoteEquivocationKind::Proposal,
            Self::Timeout { .. } => VoteEquivocationKind::Timeout,
        }
    }
}

/// Which vote stream the equivocation was found in. A validator can equivocate on both at one
/// height, so this is part of the record's identity, not just a label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Encode, Decode, CborLen)]
pub enum VoteEquivocationKind {
    #[n(0)]
    Proposal,
    #[n(1)]
    Timeout,
}

impl Display for VoteEquivocationKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Proposal => write!(f, "proposal"),
            Self::Timeout => write!(f, "timeout"),
        }
    }
}

/// Evidence that one validator signed two different votes for the same view.
///
/// Both votes were signature-checked against the signer before this record was built, so the pair
/// is proof of misbehaviour rather than a report of it. Nothing consumes it yet — there is no
/// in-protocol penalty — so it is written for operators and for a future slashing path.
#[derive(Debug, Clone, Serialize, Deserialize, Encode, Decode, CborLen)]
pub struct VoteEquivocation {
    #[n(0)]
    pub epoch: Epoch,
    #[n(1)]
    pub height: NodeHeight,
    #[n(2)]
    pub public_key: RistrettoPublicKeyBytes,
    /// Unix milliseconds at which this node noticed the second vote.
    #[n(3)]
    pub detected_at: u64,
    #[n(4)]
    pub votes: EquivocatingVotes,
}

impl VoteEquivocation {
    pub fn new(
        epoch: Epoch,
        height: NodeHeight,
        public_key: RistrettoPublicKeyBytes,
        votes: EquivocatingVotes,
    ) -> Self {
        Self {
            epoch,
            height,
            public_key,
            detected_at: unix_millis_now(),
            votes,
        }
    }

    pub fn kind(&self) -> VoteEquivocationKind {
        self.votes.kind()
    }
}

impl Display for VoteEquivocation {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} vote equivocation by {} at {}/{}",
            self.kind(),
            self.public_key,
            self.epoch,
            self.height
        )
    }
}
