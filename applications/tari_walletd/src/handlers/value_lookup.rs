// Copyright 2025 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{fs, ops::RangeInclusive, path::Path};

use anyhow::anyhow;
use log::{info, warn};
use ootle_byte_type::ConvertFromByteType;
use tari_crypto::ristretto::RistrettoSecretKey;
use tari_engine_types::crypto::{ElgamalVerifiableBalance, ElgamalVerifiableBalanceBytes};
use tari_ootle_wallet_crypto::{GenerateValueLookup, SortedValueLookup};
use tari_ootle_wallet_sdk::apis::viewable_balance::ViewableBalanceApi;

const LOG_TARGET: &str = "tari::ootle::walletd::handlers::value_lookup";

/// Recovers the plaintext balances behind `proofs` by reverse-searching the value lookup table.
///
/// With a lookup file configured, the sorted prefix-index table is used (O(log n) binary search per balance).
/// Without one, balances are recovered by on-the-fly point generation, which is very slow.
///
/// This performs blocking CPU/IO work and must be called from a blocking context.
pub(crate) fn brute_force_viewable_balances(
    api: &ViewableBalanceApi,
    lookup_file: Option<&Path>,
    secret_view_key: &RistrettoSecretKey,
    proofs: &[ElgamalVerifiableBalanceBytes],
    value_range: RangeInclusive<u64>,
) -> anyhow::Result<Vec<Option<u64>>> {
    let Some(path) = lookup_file else {
        warn!(
            target: LOG_TARGET,
            "No value lookup table file configured. Recovering balances by on-the-fly generation; this may be very \
             slow."
        );
        return Ok(api.try_brute_force_commitment_balances(
            secret_view_key,
            proofs.iter(),
            value_range,
            &mut GenerateValueLookup,
        )?);
    };

    let file =
        fs::File::open(path).map_err(|e| anyhow!("Unable to load value lookup file '{}': {e}", path.display()))?;
    // SAFETY: We assume the file will not be modified while mapped. Although not enforced (e.g. locks, permissions
    // and other platform specific mechanisms), this is a reasonable assumption for most scenarios.
    let lookup = unsafe { SortedValueLookup::load(&file) }?;
    info!(
        target: LOG_TARGET,
        "Using value lookup table '{}' ({}-{}) for reverse balance lookup",
        path.display(),
        lookup.range().start(),
        lookup.range().end(),
    );

    reverse_lookup_sorted(secret_view_key, proofs, value_range, &lookup)
}

fn reverse_lookup_sorted(
    secret_view_key: &RistrettoSecretKey,
    proofs: &[ElgamalVerifiableBalanceBytes],
    value_range: RangeInclusive<u64>,
    lookup: &SortedValueLookup,
) -> anyhow::Result<Vec<Option<u64>>> {
    let balances = proofs
        .iter()
        .map(ElgamalVerifiableBalance::convert_from_byte_type)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| anyhow!("Malformed viewable balance while decompressing for reverse lookup"))?;

    let targets = balances
        .iter()
        .map(|b| b.value_lookup_target(secret_view_key))
        .collect::<Vec<_>>();

    let mut results = lookup.find_values(&targets);

    // Preserve the requested-range contract: a value present in the table but outside the requested range is
    // treated as not found, matching the forward scan, which never inspects values outside the range.
    for result in &mut results {
        if let Some(v) = *result &&
            !value_range.contains(&v)
        {
            *result = None;
        }
    }

    // Values inside the requested range but outside the table's coverage need an on-the-fly computed scan.
    let tails = tail_ranges(&value_range, lookup.range());
    if !tails.is_empty() {
        let unresolved = results
            .iter()
            .enumerate()
            .filter(|(_, r)| r.is_none())
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        if !unresolved.is_empty() {
            warn!(
                target: LOG_TARGET,
                "{} balance(s) fall outside the lookup table range {}-{}; falling back to on-the-fly scan over the \
                 uncovered range. This may be very slow.",
                unresolved.len(),
                lookup.range().start(),
                lookup.range().end(),
            );
            let unresolved_balances = unresolved.iter().map(|&i| &balances[i]).collect::<Vec<_>>();
            for tail in tails {
                let tail_results = ElgamalVerifiableBalance::batched_brute_force(
                    secret_view_key,
                    tail,
                    &mut GenerateValueLookup,
                    unresolved_balances.iter().copied(),
                )?;
                for (k, &i) in unresolved.iter().enumerate() {
                    if results[i].is_none() {
                        results[i] = tail_results[k];
                    }
                }
            }
        }
    }

    Ok(results)
}

/// The portions of `range` that fall outside the inclusive table coverage `table`.
fn tail_ranges(range: &RangeInclusive<u64>, table: RangeInclusive<u64>) -> Vec<RangeInclusive<u64>> {
    let (range_start, range_end) = (*range.start(), *range.end());
    let (table_start, table_end) = (*table.start(), *table.end());
    let mut tails = Vec::new();
    if range_start < table_start {
        let hi = range_end.min(table_start - 1);
        if range_start <= hi {
            tails.push(range_start..=hi);
        }
    }
    if range_end > table_end {
        let lo = range_start.max(table_end + 1);
        if lo <= range_end {
            tails.push(lo..=range_end);
        }
    }
    tails
}
