//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt::Display;

use tari_bor::{Deserialize, Serialize};

// TODO: use this new-type where appropriate in the codebase
#[derive(
    Debug,
    Clone,
    Copy,
    PartialEq,
    Eq,
    Hash,
    Ord,
    PartialOrd,
    Deserialize,
    Serialize,
    minicbor::Encode,
    minicbor::Decode,
    minicbor::CborLen,
)]
#[cbor(transparent)]
#[serde(transparent)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct StateVersion(
    #[n(0)]
    #[serde(deserialize_with = "ootle_serde::str_number::deserialize")]
    #[cfg_attr(feature = "ts", ts(type = "number | bigint | string"))]
    u64,
);

impl StateVersion {
    pub const fn new(version: u64) -> Self {
        Self(version)
    }

    pub const fn zero() -> Self {
        Self(0)
    }

    pub const fn as_u64(&self) -> u64 {
        self.0
    }
}

impl Display for StateVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<u64> for StateVersion {
    fn from(value: u64) -> Self {
        Self(value)
    }
}

/// The most encoded substate bytes a leader puts up in one shard in a single block, and so the most
/// one honest block adds to a shard's state version.
pub const MAX_BLOCK_SHARD_OUTPUT_BYTES: usize = 64 * 1024 * 1024;

/// The most encoded substate bytes a block may put up in one shard and still be voted for.
///
/// State sync streams a shard one state version at a time and buffers each version whole, so a
/// consumer's per-version buffer must hold this with room for the metadata that accompanies
/// each update. CONSENSUS RULE: must be uniform network-wide, and at least
/// [`MAX_BLOCK_SHARD_OUTPUT_BYTES`] so honest blocks are never rejected.
pub const MAX_BLOCK_VALIDATION_SHARD_OUTPUT_BYTES: usize = 96 * 1024 * 1024;
