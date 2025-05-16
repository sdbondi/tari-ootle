//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::io::Write;

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_template_lib::models::UnclaimedConfidentialOutputAddress;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, PartialOrd, Ord)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
pub struct MintConfidentialOutputAtom {
    pub commitment: UnclaimedConfidentialOutputAddress,
}

impl BorshSerialize for MintConfidentialOutputAtom {
    fn serialize<W: Write>(&self, writer: &mut W) -> std::io::Result<()> {
        BorshSerialize::serialize(&self.commitment.as_object_key().into_array(), writer)
    }
}
