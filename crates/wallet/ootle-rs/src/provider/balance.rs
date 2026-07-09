//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::collections::HashMap;

use ootle_byte_type::ConvertFromByteType;
use tari_crypto::ristretto::RistrettoSecretKey;
use tari_indexer_client::types::GetSubstatesRequest;
use tari_ootle_common_types::engine_types::{
    crypto::{ElgamalVerifiableBalance, ValueLookup},
    indexed_value::IndexedWellKnownTypes,
    substate::{Substate, SubstateId},
};
use tari_template_lib_types::{Amount, ComponentAddress, ResourceAddress, UtxoAddress, VaultId};

use crate::provider::{ProviderError, ProviderResult, indexer::IndexerProvider};

/// Balance information for a single vault
#[derive(Debug, Clone)]
pub struct VaultBalance {
    pub vault_id: VaultId,
    pub resource_address: ResourceAddress,
    pub balance: Amount,
    pub locked_balance: Amount,
}

impl<Wallet> IndexerProvider<Wallet> {
    /// Returns the balance for a specific resource in the given account.
    ///
    /// Fetches the account component substate, extracts all vault IDs from its state,
    /// then fetches each vault to find the one matching the requested resource address.
    /// Returns `Amount::zero()` if no vault exists for the given resource.
    pub async fn get_account_balance(
        &self,
        account: ComponentAddress,
        resource: ResourceAddress,
    ) -> ProviderResult<Amount> {
        let vaults = self.fetch_account_vaults(account).await?;
        let balance = vaults
            .into_iter()
            .find(|v| v.resource_address == resource)
            .map(|v| v.balance)
            .unwrap_or_else(Amount::zero);
        Ok(balance)
    }

    /// Returns balances for all resources held in the given account.
    ///
    /// Fetches the account component and all its vaults, returning a map
    /// from resource address to balance amount.
    pub async fn get_account_balances(
        &self,
        account: ComponentAddress,
    ) -> ProviderResult<HashMap<ResourceAddress, Amount>> {
        let vaults = self.fetch_account_vaults(account).await?;
        let balances = vaults.into_iter().map(|v| (v.resource_address, v.balance)).collect();
        Ok(balances)
    }

    /// Decrypts the value of one or more stealth UTXOs using the given ElGamal view secret key and a value
    /// lookup.
    ///
    /// Each UTXO must contain a viewable balance proof. Returns a map from UTXO address to the decrypted value;
    /// UTXOs without a viewable balance proof, or whose value is not covered by the lookup, are omitted.
    pub fn decrypt_stealth_utxo_values<L: ValueLookup>(
        &self,
        view_secret_key: &RistrettoSecretKey,
        utxo_substates: &HashMap<SubstateId, Substate>,
        lookup: &L,
    ) -> ProviderResult<HashMap<UtxoAddress, u64>> {
        let proofs = utxo_substates
            .iter()
            .filter_map(|(id, s)| {
                let addr = id.as_utxo_address()?;
                let proof = s
                    .substate_value()
                    .as_utxo()
                    .and_then(|u| u.output())
                    .and_then(|o| o.output.viewable_balance.as_ref())?;
                Some((addr, proof))
            })
            .collect::<Vec<_>>();

        if proofs.is_empty() {
            return Ok(HashMap::new());
        }

        let addresses = proofs.iter().map(|(a, _)| a.clone()).collect::<Vec<_>>();
        let elgamal_proofs = proofs
            .iter()
            .map(|(_, p)| ElgamalVerifiableBalance::convert_from_byte_type(p))
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| ProviderError::other(format!("Failed to decompress viewable balance proof: {e}")))?;

        let results = ElgamalVerifiableBalance::decrypt_many(view_secret_key, &elgamal_proofs, lookup)
            .map_err(|e| ProviderError::other(format!("Value lookup error: {e}")))?;

        let values = addresses
            .into_iter()
            .zip(results)
            .filter_map(|(addr, val)| val.map(|v| (addr, v)))
            .collect();

        Ok(values)
    }

    /// Fetches a UTXO from the network and decrypts its value using the given ElGamal view secret key.
    ///
    /// This is a convenience method that fetches a single UTXO substate and decrypts its viewable balance.
    /// Returns `None` if the UTXO does not contain a viewable balance proof or its value is not covered by the
    /// lookup.
    pub async fn get_utxo_value<L: ValueLookup>(
        &self,
        view_secret_key: &RistrettoSecretKey,
        utxo_address: UtxoAddress,
        lookup: &L,
    ) -> ProviderResult<Option<u64>> {
        let substate = self.fetch_substate(SubstateId::from(utxo_address)).await?;

        let proof = substate
            .substate_value()
            .as_utxo()
            .and_then(|u| u.output())
            .and_then(|o| o.output.viewable_balance.as_ref());

        let Some(proof) = proof else {
            return Ok(None);
        };

        let balance = ElgamalVerifiableBalance::convert_from_byte_type(proof)
            .map_err(|e| ProviderError::other(format!("Failed to decompress viewable balance proof: {e}")))?;

        balance
            .decrypt(view_secret_key, lookup)
            .map_err(|e| ProviderError::other(format!("Value lookup error: {e}")))
    }

    /// Fetches all vault balances for the given account component.
    async fn fetch_account_vaults(&self, account: ComponentAddress) -> ProviderResult<Vec<VaultBalance>> {
        let substate = self.fetch_substate(account).await?;
        let component = substate
            .substate_value()
            .component()
            .ok_or_else(|| ProviderError::other("Expected component substate for account"))?;

        let indexed = IndexedWellKnownTypes::from_value(component.state())
            .map_err(|e| ProviderError::other(format!("Failed to index component state: {e}")))?;

        let vault_ids = indexed.vault_ids();
        if vault_ids.is_empty() {
            return Ok(Vec::new());
        }

        let vault_substate_ids: Vec<SubstateId> = vault_ids.iter().copied().map(SubstateId::Vault).collect();
        let resp = self
            .client()
            .fetch_substates(GetSubstatesRequest {
                requests: vault_substate_ids
                    .try_into()
                    .map_err(|_| ProviderError::other("Too many vaults in account"))?,
                cached_only: false,
            })
            .await?;

        let mut balances = Vec::with_capacity(resp.substates.len());
        for (id, substate) in resp.substates {
            let substate: Substate = substate;
            let vault = substate.into_substate_value().into_vault().ok_or_else(|| {
                ProviderError::other(format!(
                    "Expected vault substate for {id}, but got a different substate type"
                ))
            })?;
            balances.push(VaultBalance {
                vault_id: id.as_vault_id().expect("SubstateId::Vault always has a vault id"),
                resource_address: *vault.resource_address(),
                balance: vault.balance(),
                locked_balance: vault.locked_balance(),
            });
        }

        Ok(balances)
    }
}
