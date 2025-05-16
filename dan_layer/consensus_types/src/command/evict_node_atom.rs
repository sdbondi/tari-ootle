//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt::{Display, Formatter};

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_template_lib::prelude::RistrettoPublicKeyBytes;

#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize)]
pub struct EvictNodeAtom {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    #[serde(with = "serde_with::hex")]
    pub public_key: RistrettoPublicKeyBytes,
}

impl Display for EvictNodeAtom {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.public_key)
    }
}
