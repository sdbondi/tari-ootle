//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_dan_common_types::ShardGroup;

use crate::ids::BlockId;

#[derive(Debug, Clone, Eq, PartialEq, Hash, Serialize, Deserialize, PartialOrd, Ord, BorshSerialize)]
#[cfg_attr(
    feature = "ts",
    derive(ts_rs::TS),
    ts(export, export_to = "../../bindings/src/types/")
)]
pub struct ForeignProposalAtom {
    #[cfg_attr(feature = "ts", ts(type = "string"))]
    pub block_id: BlockId,
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    pub shard_group: ShardGroup,
}
