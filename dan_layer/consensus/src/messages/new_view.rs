//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use serde::Serialize;
use tari_dan_common_types::NodeHeight;
use tari_dan_storage::consensus_models::{QuorumCertificate, ValidatorSignature};

use super::VoteMessage;

#[derive(Debug, Clone, Serialize)]
pub struct NewViewMessage {
    pub high_qc: QuorumCertificate,
    pub new_height: NodeHeight,
    /// Signature that signs the dummy block, 2f + 1 of these can be collected to create a new QC that justifies the
    /// view change
    pub dummy_signature: ValidatorSignature,
    pub last_vote: Option<VoteMessage>,
}
