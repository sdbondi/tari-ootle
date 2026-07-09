// Copyright 2025 The Tari Project
// SPDX-License-Identifier: BSD-3-Clause

use std::{fs, path::Path};

use anyhow::anyhow;
use log::{info, warn};
use tari_crypto::ristretto::RistrettoSecretKey;
use tari_engine_types::crypto::ElgamalVerifiableBalanceBytes;
use tari_ootle_wallet_crypto::{GenerateValueLookup, SortedPrefixFileLookup};
use tari_ootle_wallet_sdk::apis::viewable_balance::ViewableBalanceApi;
use tari_ootle_walletd_client::types::StealthUtxosGetValueLookupInfoResponse;

const LOG_TARGET: &str = "tari::ootle::walletd::handlers::value_lookup";

/// Upper bound for the on-the-fly scan when no lookup file is configured and the caller gives no maximum.
const DEFAULT_NO_FILE_MAX_VALUE: u64 = 10_000_000_000;

/// Recovers the plaintext balances behind `proofs` by reverse-searching a value lookup.
///
/// With a lookup file configured, a [`SortedPrefixFileLookup`] is searched by O(log n) binary search per balance. The
/// whole file is searched cheaply, so `min_expected`/`max_expected` no longer bound the search — they are only
/// a coverage hint. A balance whose value is not in the file is reported as `None`.
///
/// Without a lookup file, a [`GenerateValueLookup`] recovers each balance by computing `v·G` over
/// `min_expected..=max_expected`, which is very slow.
///
/// This performs blocking CPU/IO work and must be called from a blocking context.
pub(crate) fn brute_force_viewable_balances(
    api: &ViewableBalanceApi,
    lookup_file: Option<&Path>,
    secret_view_key: &RistrettoSecretKey,
    proofs: &[ElgamalVerifiableBalanceBytes],
    min_expected: Option<u64>,
    max_expected: Option<u64>,
) -> anyhow::Result<Vec<Option<u64>>> {
    let Some(path) = lookup_file else {
        let value_range = min_expected.unwrap_or(0)..=max_expected.unwrap_or(DEFAULT_NO_FILE_MAX_VALUE);
        warn!(
            target: LOG_TARGET,
            "No value lookup table file configured. Recovering balances by on-the-fly generation over {}-{}; this \
             may be extremely slow.",
            value_range.start(),
            value_range.end(),
        );
        let lookup = GenerateValueLookup::new(value_range);
        return Ok(api.try_decrypt_commitment_balances(secret_view_key, proofs.iter(), &lookup)?);
    };

    let file =
        fs::File::open(path).map_err(|e| anyhow!("Unable to load value lookup file '{}': {e}", path.display()))?;
    // SAFETY: We assume the file will not be modified while mapped. Although not enforced (e.g. locks, permissions
    // and other platform specific mechanisms), this is a reasonable assumption for most scenarios.
    let lookup = unsafe { SortedPrefixFileLookup::load(&file) }?;
    info!(
        target: LOG_TARGET,
        "Using value lookup table '{}' ({}-{}) for reverse balance lookup",
        path.display(),
        lookup.range().start(),
        lookup.range().end(),
    );

    // A requested maximum above the file's coverage means high-value outputs simply cannot be found; surface it.
    if let Some(max) = max_expected &&
        max > *lookup.range().end()
    {
        warn!(
            target: LOG_TARGET,
            "Requested maximum value {max} exceeds the lookup file coverage {}-{}; values above {} cannot be found \
             and require a larger lookup file.",
            lookup.range().start(),
            lookup.range().end(),
            lookup.range().end(),
        );
    }

    let results = api.try_decrypt_commitment_balances(secret_view_key, proofs.iter(), &lookup)?;

    // A validated ciphertext decrypted with the correct view key yields v·G for a real v in [0, 2^64), so a
    // not-found result means the value is outside the file's coverage (a larger file is required) — or the view
    // key does not match the output. There is deliberately no on-the-fly fallback: computing v·G beyond the
    // file's range can take years.
    if results.iter().any(|r| r.is_none()) {
        warn!(
            target: LOG_TARGET,
            "Some balances could not be decrypted: the value is outside the lookup file range {}-{} (a larger \
             lookup file is required), or the provided view key does not match the output(s).",
            lookup.range().start(),
            lookup.range().end(),
        );
    }

    Ok(results)
}

/// Reports the configured value lookup table's format and coverage for diagnostics. When no file is
/// configured, `configured` is `false`; a configured-but-unreadable file surfaces as an error.
pub(crate) fn value_lookup_info(lookup_file: Option<&Path>) -> anyhow::Result<StealthUtxosGetValueLookupInfoResponse> {
    let Some(path) = lookup_file else {
        return Ok(StealthUtxosGetValueLookupInfoResponse {
            configured: false,
            path: None,
            format: None,
            min: None,
            max: None,
            prefix_len: None,
            value_len: None,
            num_records: None,
        });
    };

    let file =
        fs::File::open(path).map_err(|e| anyhow!("Unable to load value lookup file '{}': {e}", path.display()))?;
    // SAFETY: We assume the file will not be modified while mapped. Although not enforced (e.g. locks, permissions
    // and other platform specific mechanisms), this is a reasonable assumption for most scenarios.
    let lookup = unsafe { SortedPrefixFileLookup::load(&file) }?;
    let header = lookup.header();
    Ok(StealthUtxosGetValueLookupInfoResponse {
        configured: true,
        path: Some(path.display().to_string()),
        format: Some("sorted_prefix_v1".to_string()),
        min: Some(header.min),
        max: Some(header.max),
        prefix_len: Some(header.prefix_len),
        value_len: Some(header.value_len),
        num_records: Some(lookup.len() as u64),
    })
}
