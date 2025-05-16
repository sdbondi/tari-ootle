//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};

use crate::consensus_models::certificates::v1::TimeoutCertificateV1;

#[derive(Debug, Clone, Deserialize, Serialize, BorshSerialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
pub enum TimeoutCertificate {
    V1(TimeoutCertificateV1),
}
