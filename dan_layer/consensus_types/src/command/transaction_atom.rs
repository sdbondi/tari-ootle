//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt::{Display, Formatter};

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_transaction::TransactionId;

use crate::command::{Decision, Evidence};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, BorshSerialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
pub struct TransactionAtom {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub id: TransactionId,
    pub decision: Decision,
    pub evidence: Evidence,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub transaction_fee: u64,
    pub leader_fee: Option<LeaderFee>,
}

impl TransactionAtom {
    pub fn id(&self) -> &TransactionId {
        &self.id
    }
}

impl Display for TransactionAtom {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "TransactionAtom({}, {}, {}, ",
            self.id, self.decision, self.transaction_fee,
        )?;
        match self.leader_fee {
            Some(ref leader_fee) => write!(f, "{}", leader_fee)?,
            None => write!(f, "--")?,
        }
        write!(f, ")")
    }
}
