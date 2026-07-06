//   Copyright 2024 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    fmt::{Display, Formatter},
    num::NonZeroU64,
};

use borsh::BorshSerialize;
use minicbor::{CborLen, Decode, Encode};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, Encode, Decode, CborLen)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS), ts(export))]
pub struct LeaderFee {
    /// The fee payable to the leader of each involved shard group.
    #[n(0)]
    pub fee: u64,
    /// Unused by consensus logic: reserves CBOR index 1, which persisted pre-surcharge encodings populate with a
    /// per-transaction exhaust burn amount. The index must not be reused. Always `None` on newly constructed values,
    /// so new CBOR encodings omit it. The value must survive decode → hash/re-encode round trips so that
    /// pre-existing command hashes (and therefore block IDs) remain reproducible — see the manual `BorshSerialize`
    /// impl and the p2p conversions, which carry it as a plain `u64` with 0 == `None`.
    #[n(1)]
    #[serde(skip)]
    #[cfg_attr(feature = "ts", ts(skip))]
    unused_exhaust_burn: Option<u64>,
}

impl LeaderFee {
    pub fn new(fee: u64) -> Self {
        Self {
            fee,
            unused_exhaust_burn: None,
        }
    }

    /// Constructs a `LeaderFee` from decoded wire data. A zero `unused_exhaust_burn` maps to `None`: the two are
    /// hash-equivalent (both borsh-serialize as `0u64`), and newly constructed fees are always `None`, so equality
    /// comparisons between decoded and locally constructed values remain consistent.
    pub fn load(fee: u64, unused_exhaust_burn: u64) -> Self {
        Self {
            fee,
            unused_exhaust_burn: (unused_exhaust_burn > 0).then_some(unused_exhaust_burn),
        }
    }

    pub fn fee(&self) -> u64 {
        self.fee
    }

    pub fn unused_exhaust_burn(&self) -> u64 {
        self.unused_exhaust_burn.unwrap_or(0)
    }
}

/// The borsh encoding feeds command hashing and must be byte-identical to the pre-surcharge layout, which encodes
/// the retired exhaust burn as a plain `u64` after the fee. `None` encodes as `0u64` (never as an `Option` tag), so
/// pre-existing command hashes are reproducible from decoded values and new values hash the reserved index as 0.
impl BorshSerialize for LeaderFee {
    fn serialize<W: std::io::Write>(&self, writer: &mut W) -> std::io::Result<()> {
        BorshSerialize::serialize(&self.fee, writer)?;
        BorshSerialize::serialize(&self.unused_exhaust_burn(), writer)
    }
}

impl Display for LeaderFee {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        write!(f, "Leader fee: {}", self.fee)
    }
}

/// The fee payable to the leader of each involved shard group is `transaction_fee` divided evenly across
/// `num_involved_shards`. Any indivisible remainder is not paid to leaders — see `calculate_exhaust_burn`, which
/// folds it into the burn total instead.
pub fn calculate_leader_fee(transaction_fee: u64, num_involved_shards: NonZeroU64) -> LeaderFee {
    LeaderFee::new(transaction_fee / num_involved_shards)
}

/// The amount burned for the whole transaction across all involved shard groups. Each shard group accumulates only
/// its portion of this into its block header burn total — see `Evidence::exhaust_burn_portion`.
///
/// This is the exhaust burn surcharge (`transaction_fee * rate_bps / 10_000`, floor division) plus any remainder
/// left over from dividing `transaction_fee` evenly across `num_involved_shards` in `calculate_leader_fee` — folding
/// this dust into the burn keeps `fee * num_involved_shards + exhaust_burn == transaction_fee + surcharge` exact
/// without reviving asymmetric per-leader rounding.
pub fn calculate_exhaust_burn(transaction_fee: u64, num_involved_shards: NonZeroU64, rate_bps: u16) -> u64 {
    let surcharge = (u128::from(transaction_fee) * u128::from(rate_bps) / 10_000) as u64;
    let leader_remainder = transaction_fee % num_involved_shards;
    surcharge + leader_remainder
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn it_decodes_encodings_that_populate_the_reserved_index() {
        #[derive(Encode)]
        struct WithReservedIndex {
            #[n(0)]
            fee: u64,
            #[n(1)]
            exhaust_burn: u64,
        }

        let bytes = minicbor::to_vec(WithReservedIndex {
            fee: 50,
            exhaust_burn: 5,
        })
        .unwrap();
        let decoded = minicbor::decode::<LeaderFee>(&bytes).unwrap();
        assert_eq!(decoded.fee, 50);
        assert_eq!(decoded.unused_exhaust_burn, Some(5));
    }

    #[test]
    fn it_reproduces_the_borsh_encoding_of_values_that_populate_the_reserved_index() {
        #[derive(BorshSerialize)]
        struct WithReservedIndex {
            fee: u64,
            exhaust_burn: u64,
        }

        // Command hashes cover the borsh encoding, so a value decoded with the reserved index populated must
        // reproduce the exact bytes it hashed with when it was created.
        let expected = borsh::to_vec(&WithReservedIndex {
            fee: 50,
            exhaust_burn: 5,
        })
        .unwrap();
        assert_eq!(borsh::to_vec(&LeaderFee::load(50, 5)).unwrap(), expected);

        let expected = borsh::to_vec(&WithReservedIndex {
            fee: 50,
            exhaust_burn: 0,
        })
        .unwrap();
        assert_eq!(borsh::to_vec(&LeaderFee::new(50)).unwrap(), expected);
        assert_eq!(borsh::to_vec(&LeaderFee::load(50, 0)).unwrap(), expected);
    }

    #[test]
    fn it_round_trips_the_reserved_index_through_the_cbor_encoding() {
        let bytes = minicbor::to_vec(LeaderFee::load(50, 5)).unwrap();
        let decoded = minicbor::decode::<LeaderFee>(&bytes).unwrap();
        assert_eq!(decoded, LeaderFee::load(50, 5));
        assert_eq!(decoded.unused_exhaust_burn(), 5);
    }

    #[test]
    fn it_omits_the_reserved_index_when_encoding() {
        let leader_fee = calculate_leader_fee(100, NonZeroU64::new(2).unwrap());
        let bytes = minicbor::to_vec(&leader_fee).unwrap();
        let mut decoder = minicbor::Decoder::new(&bytes);
        assert_eq!(decoder.array().unwrap(), Some(1));

        let decoded = minicbor::decode::<LeaderFee>(&bytes).unwrap();
        assert_eq!(decoded, leader_fee);
    }

    #[test]
    fn it_calculates_the_correct_leader_fee_and_burn() {
        let test_cases = [
            // (transaction_fee, num_involved_shards, rate_bps, expected_leader_fee, expected_burn)
            // 10% (1000bps) surcharge rate
            (100, 1, 1000, 100, 10),
            (100, 2, 1000, 50, 10),
            (100, 3, 1000, 33, 11),
            (100, 4, 1000, 25, 10),
            (100, 5, 1000, 20, 10),
            (100, 6, 1000, 16, 14),
            (100, 7, 1000, 14, 12),
            (100, 8, 1000, 12, 14),
            (100, 9, 1000, 11, 11),
            (100, 10, 1000, 10, 10),
            // 5% (500bps) surcharge rate, matches current production rate
            (100, 1, 500, 100, 5),
            (100, 2, 500, 50, 5),
            (100, 3, 500, 33, 6),
            (100, 4, 500, 25, 5),
            (100, 5, 500, 20, 5),
            (100, 6, 500, 16, 9),
            (100, 7, 500, 14, 7),
            (100, 8, 500, 12, 9),
            (100, 9, 500, 11, 6),
            (100, 10, 500, 10, 5),
            // 5% (500bps) surcharge rate
            (55, 3, 500, 18, 3),
            (55, 4, 500, 13, 5),
            (55, 5, 500, 11, 2),
            (55, 6, 500, 9, 3),
            (55, 7, 500, 7, 8),
            (55, 8, 500, 6, 9),
            (55, 9, 500, 6, 3),
            (55, 10, 500, 5, 7),
            // exact indivisible remainder case: no leader-fee remainder, burn is pure surcharge
            (101, 2, 500, 50, 6),
            // zero rate: no surcharge, only the leader-fee division remainder is burned
            (101, 2, 0, 50, 1),
        ];

        for (transaction_fee, num_involved_shards, rate_bps, expected_leader_fee, expected_burn) in test_cases {
            let num_involved_shards = NonZeroU64::new(num_involved_shards).unwrap();
            let leader_fee = calculate_leader_fee(transaction_fee as u64, num_involved_shards);
            let burn = calculate_exhaust_burn(transaction_fee as u64, num_involved_shards, rate_bps as u16);
            let surcharge = (transaction_fee as u128 * rate_bps as u128 / 10_000) as u64;
            assert_eq!(
                leader_fee.fee * num_involved_shards.get() + burn,
                transaction_fee as u64 + surcharge,
                "In/deflation! transaction_fee: {transaction_fee}, num_involved_shards: {num_involved_shards}, \
                 rate_bps: {rate_bps}",
            );
            assert_eq!(
                leader_fee.fee(),
                expected_leader_fee as u64,
                "Failed for transaction_fee: {}, num_involved_shards: {}, rate_bps: {}",
                transaction_fee,
                num_involved_shards,
                rate_bps
            );
            assert_eq!(
                burn, expected_burn as u64,
                "Failed for transaction_fee: {}, num_involved_shards: {}, rate_bps: {}",
                transaction_fee, num_involved_shards, rate_bps
            );
        }
    }
}
