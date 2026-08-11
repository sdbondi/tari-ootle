//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use borsh::BorshSerialize;
use minicbor::{CborLen, Decode, Encode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, Default, Serialize, Deserialize, BorshSerialize, Encode, Decode, CborLen)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct ShardGroupAccumulatedData {
    #[n(0)]
    pub total_exhaust_burn: u128,
}

impl From<ShardGroupAccumulatedData> for tari_sidechain::ShardGroupAccumulatedData {
    fn from(value: ShardGroupAccumulatedData) -> Self {
        Self {
            total_exhaust_burn: value.total_exhaust_burn,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `total_exhaust_burn` crosses the `u64::MAX` boundary where minicbor switches between a
    /// plain CBOR integer and an RFC 8949 bignum, and `CborLen` has to agree with the encoder on
    /// both sides of it.
    #[test]
    fn total_exhaust_burn_roundtrips_across_the_bignum_boundary() {
        for total_exhaust_burn in [0, 1, u128::from(u64::MAX), u128::from(u64::MAX) + 1, u128::MAX] {
            let data = ShardGroupAccumulatedData { total_exhaust_burn };
            let bytes = minicbor::to_vec(data).unwrap();
            assert_eq!(bytes.len(), minicbor::len(data));
            let decoded: ShardGroupAccumulatedData = minicbor::decode(&bytes).unwrap();
            assert_eq!(decoded.total_exhaust_burn, total_exhaust_burn);
        }
    }
}
