//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_consensus_types::certificates::{v1::TimeoutVote, QuorumCertificate};
use tari_dan_common_types::NodeHeight;

use super::VoteMessage;

#[derive(Debug, Clone)]
pub struct NewViewMessage {
    pub timeout: TimeoutVote,
    pub high_qc: QuorumCertificate,
    pub last_vote: Option<VoteMessage>,
}
