//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt::{Display, Formatter};

use borsh::BorshSerialize;
use serde::{Deserialize, Serialize};
use tari_common_types::types::{FixedHash, FixedHashSizeError};
use tari_dan_common_types::hashing;
use tari_engine_types::serde_with;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, BorshSerialize)]
#[serde(transparent)]
pub struct BlockId(#[serde(with = "serde_with::hex")] FixedHash);

impl BlockId {
    pub const fn zero() -> Self {
        Self(FixedHash::zero())
    }

    pub fn from_parent_and_header_hash(parent_id: &BlockId, header_hash: &FixedHash) -> Self {
        // The zero block is a special case. It has no parent and its ID is always zero.
        if *header_hash == FixedHash::zero() && parent_id.is_zero() {
            return BlockId::zero();
        }

        hashing::block_hasher()
            .chain(parent_id)
            .chain(header_hash)
            .finalize_into_array()
            .into()
    }

    pub fn new<T: Into<FixedHash>>(hash: T) -> Self {
        Self(hash.into())
    }

    pub const fn as_hash(&self) -> &FixedHash {
        &self.0
    }

    pub fn as_bytes(&self) -> &[u8] {
        self.0.as_slice()
    }

    pub fn into_array(self) -> [u8; 32] {
        self.0.into_array()
    }

    pub fn is_zero(&self) -> bool {
        self.0.iter().all(|b| *b == 0)
    }

    pub const fn byte_size() -> usize {
        FixedHash::byte_size()
    }
}

impl AsRef<[u8]> for BlockId {
    fn as_ref(&self) -> &[u8] {
        self.0.as_slice()
    }
}

impl From<FixedHash> for BlockId {
    fn from(value: FixedHash) -> Self {
        Self(value)
    }
}

impl From<[u8; 32]> for BlockId {
    fn from(value: [u8; 32]) -> Self {
        Self(value.into())
    }
}

impl TryFrom<Vec<u8>> for BlockId {
    type Error = FixedHashSizeError;

    fn try_from(value: Vec<u8>) -> Result<Self, Self::Error> {
        Self::try_from(value.as_slice())
    }
}

impl TryFrom<&[u8]> for BlockId {
    type Error = FixedHashSizeError;

    fn try_from(value: &[u8]) -> Result<Self, Self::Error> {
        FixedHash::try_from(value).map(Self)
    }
}

impl Display for BlockId {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl AsRef<BlockId> for BlockId {
    fn as_ref(&self) -> &BlockId {
        self
    }
}
