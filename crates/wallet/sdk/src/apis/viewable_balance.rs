//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use ootle_byte_type::ConvertFromByteType;
use tari_crypto::ristretto::RistrettoSecretKey;
use tari_engine_types::crypto::{ElgamalVerifiableBalance, ElgamalVerifiableBalanceBytes, ValueLookup};
use tari_ootle_wallet_crypto::WalletCryptoError;

#[derive(Debug, Clone)]
pub struct ViewableBalanceApi;

impl ViewableBalanceApi {
    /// Decrypts the values behind the given viewable-balance proofs using the provided value lookup.
    pub fn try_decrypt_commitment_balances<'a, L, TProofsIter>(
        &self,
        secret_view_key: &RistrettoSecretKey,
        proofs: TProofsIter,
        lookup: &L,
    ) -> Result<Vec<Option<u64>>, ViewableBalanceApiError>
    where
        L: ValueLookup,
        TProofsIter: Iterator<Item = &'a ElgamalVerifiableBalanceBytes>,
    {
        let decompressed = proofs
            .map(ElgamalVerifiableBalance::convert_from_byte_type)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| WalletCryptoError::InvalidArgument {
                name: "proofs",
                details: "Malformed viewable balance when decompressing ElgamalVerifiableBalance for decryption"
                    .to_string(),
            })?;

        let results = ElgamalVerifiableBalance::decrypt_many(secret_view_key, &decompressed, lookup)
            .map_err(|e| ViewableBalanceApiError::ValueLookupError { details: e.to_string() })?;

        Ok(results)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ViewableBalanceApiError {
    #[error(transparent)]
    WalletCryptoError(#[from] WalletCryptoError),
    #[error("Value lookup error: {details}")]
    ValueLookupError { details: String },
}
