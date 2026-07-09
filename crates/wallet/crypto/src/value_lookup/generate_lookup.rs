//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{convert::Infallible, ops::RangeInclusive};

use ootle_byte_type::ToByteType;
use tari_crypto::{
    keys::PublicKey,
    ristretto::{RistrettoPublicKey, RistrettoSecretKey},
};
use tari_engine_types::crypto::ValueLookup;
use tari_template_lib_types::crypto::RistrettoPublicKeyBytes;

/// A [`ValueLookup`] that recovers `v` by computing `v·G` for each candidate in `range` on the fly.
///
/// This needs no precomputed file but is very slow — up to the whole `range` is scanned per lookup. Prefer
/// the precomputed `SortedPrefixFileLookup` whenever a lookup file is available.
#[derive(Clone)]
pub struct GenerateValueLookup {
    range: RangeInclusive<u64>,
}

impl GenerateValueLookup {
    pub fn new(range: RangeInclusive<u64>) -> Self {
        Self { range }
    }

    fn compute_point(value: u64) -> RistrettoPublicKeyBytes {
        RistrettoPublicKey::from_secret_key(&RistrettoSecretKey::from(value)).to_byte_type()
    }
}

impl ValueLookup for GenerateValueLookup {
    type Error = Infallible;

    fn lookup(&self, point: &RistrettoPublicKeyBytes) -> Result<Option<u64>, Self::Error> {
        for v in self.range.clone() {
            if Self::compute_point(v) == *point {
                return Ok(Some(v));
            }
        }
        Ok(None)
    }

    fn lookup_many(&self, points: &[RistrettoPublicKeyBytes]) -> Result<Vec<Option<u64>>, Self::Error> {
        let mut results = vec![None; points.len()];
        let mut remaining = points.len();
        for v in self.range.clone() {
            if remaining == 0 {
                break;
            }
            let candidate = Self::compute_point(v);
            for (i, target) in points.iter().enumerate() {
                if results[i].is_none() && candidate == *target {
                    results[i] = Some(v);
                    remaining -= 1;
                }
            }
        }
        Ok(results)
    }
}
