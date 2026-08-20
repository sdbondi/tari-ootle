//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use alloc::vec::Vec;
use core::fmt::{self, Display};

/// A fixed-size byte wrapper standing in for the newtype hash/address types downstream crates pass
/// to these adapters. It exercises the `TryFrom<&[u8]> + AsRef<[u8]>` path, which is distinct from
/// the `[u8; N]` and `Vec<u8>` paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bytes32([u8; 32]);

impl Bytes32 {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }
}

impl AsRef<[u8]> for Bytes32 {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl TryFrom<&[u8]> for Bytes32 {
    type Error = WrongLength;

    fn try_from(bytes: &[u8]) -> Result<Self, Self::Error> {
        <[u8; 32]>::try_from(bytes)
            .map(Self)
            .map_err(|_| WrongLength(bytes.len()))
    }
}

impl TryFrom<Vec<u8>> for Bytes32 {
    type Error = WrongLength;

    fn try_from(bytes: Vec<u8>) -> Result<Self, Self::Error> {
        Self::try_from(bytes.as_slice())
    }
}

#[derive(Debug)]
pub struct WrongLength(usize);

impl Display for WrongLength {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "expected 32 bytes, got {}", self.0)
    }
}
