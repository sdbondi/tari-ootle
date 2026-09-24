//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{fmt::Display, num::ParseIntError, str::FromStr};

use borsh::{BorshDeserialize, BorshSerialize};
use serde::{Deserialize, Serialize};
#[cfg(feature = "ts")]
use ts_rs::TS;

/// The version of a substate: the number of times it has been written since it was created at version 0.
///
/// Every encoding (serde, CBOR and Borsh) is that of the inner `u64`, so the version hashes, and addresses the JMT,
/// exactly as a bare integer would.
#[derive(
    Copy,
    Clone,
    Debug,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
    Deserialize,
    Serialize,
    BorshSerialize,
    BorshDeserialize,
    minicbor::Encode,
    minicbor::Decode,
    minicbor::CborLen,
)]
#[serde(transparent)]
#[cbor(transparent)]
#[cfg_attr(feature = "ts", derive(TS), ts(export))]
pub struct SubstateVersion(
    #[cfg_attr(feature = "ts", ts(type = "number"))]
    #[n(0)]
    u64,
);

impl SubstateVersion {
    pub const ZERO: Self = Self(0);

    pub const fn new(version: u64) -> Self {
        Self(version)
    }

    pub const fn as_u64(self) -> u64 {
        self.0
    }

    pub const fn is_zero(self) -> bool {
        self.0 == 0
    }

    pub const fn to_be_bytes(self) -> [u8; 8] {
        self.0.to_be_bytes()
    }

    pub const fn from_be_bytes(bytes: [u8; 8]) -> Self {
        Self(u64::from_be_bytes(bytes))
    }

    /// The version a write to this version creates.
    ///
    /// Saturates at `u64::MAX`, because a version can arrive from untrusted input and the increment must stay total.
    /// An honest substate never gets there, since a version counts the writes to one substate. Where reaching the
    /// ceiling is an error to report, use [`checked_next`](Self::checked_next).
    pub const fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }

    /// The version a write to this version creates, or `None` if this is `u64::MAX`.
    pub const fn checked_next(self) -> Option<Self> {
        match self.0.checked_add(1) {
            Some(v) => Some(Self(v)),
            None => None,
        }
    }

    /// The version this one replaced, or `None` for the version the substate was created at.
    pub fn previous(self) -> Option<Self> {
        self.0.checked_sub(1).map(Self)
    }
}

impl From<u64> for SubstateVersion {
    fn from(version: u64) -> Self {
        Self(version)
    }
}

impl From<SubstateVersion> for u64 {
    fn from(version: SubstateVersion) -> Self {
        version.0
    }
}

impl PartialEq<u64> for SubstateVersion {
    fn eq(&self, other: &u64) -> bool {
        self.0 == *other
    }
}

impl Display for SubstateVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl FromStr for SubstateVersion {
    type Err = ParseIntError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        s.parse().map(Self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_encoding_is_that_of_the_inner_u64() {
        let version = SubstateVersion::new(33_691);
        assert_eq!(borsh::to_vec(&version).unwrap(), borsh::to_vec(&33_691u64).unwrap());
        assert_eq!(minicbor::to_vec(version).unwrap(), minicbor::to_vec(33_691u64).unwrap());
        assert_eq!(serde_json::to_string(&version).unwrap(), "33691");
        assert_eq!(serde_json::from_str::<SubstateVersion>("33691").unwrap(), version);
    }

    #[test]
    fn previous_of_the_creation_version_is_none() {
        assert_eq!(SubstateVersion::ZERO.previous(), None);
        assert_eq!(SubstateVersion::new(5).previous(), Some(SubstateVersion::new(4)));
        assert_eq!(SubstateVersion::new(4).next(), SubstateVersion::new(5));
    }
}
