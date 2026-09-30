//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

mod support;

use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use futures::StreamExt;
use ootle_byte_type::ToByteType;
use tari_crypto::{keys::PublicKey, ristretto::RistrettoPublicKey};
use tari_engine_types::{
    Epoch,
    Utxo,
    substate::{Substate, SubstateId},
};
use tari_indexer_client::types::WatchedSubstateItem;
use tari_ootle_common_types::{
    StateVersion,
    SubstateVersion,
    optional::IsNotFoundError,
    response_status::{ResponseErrorStatus, TransactionStatusResponseError},
    shard::Shard,
};
use tari_ootle_transaction::{Transaction, TransactionEnvelope, TransactionId};
use tari_ootle_wallet_sdk::{
    apis::config::ConfigKey,
    models::{KeyBranch, KeyId, StartOfShard, UtxoUnspent, UtxoUpdatePayload, WalletUtxoUpdate},
    network::{
        SubstateQueryResult,
        TransactionFinalizedStream,
        TransactionQueryResult,
        UtxoUpdateStream,
        WalletNetworkInterface,
    },
    storage::TagAndPublicNoncePair,
};
use tari_ootle_wallet_sdk_services::{
    account_monitor::AccountMonitor,
    account_recovery::AccountRecoveryService,
    notify::Notify,
    utxo_scanner::StealthUtxoScannerWorker,
};
use tari_shutdown::Shutdown;
use tari_template_abi::TemplateDef;
use tari_template_lib::types::{
    ComponentAddress,
    ResourceAddress,
    TemplateAddress,
    UtxoId,
    constants::STEALTH_TARI_RESOURCE_ADDRESS,
    crypto::UtxoTag,
};

use crate::support::{TestWithNetwork, random_keypair};

#[derive(Debug, thiserror::Error)]
enum FakeError {
    #[error("not found")]
    NotFound,
}

impl IsNotFoundError for FakeError {
    fn is_not_found_error(&self) -> bool {
        true
    }
}

impl TransactionStatusResponseError for FakeError {
    fn get_status(&self) -> ResponseErrorStatus {
        ResponseErrorStatus::NotFound {
            message: self.to_string(),
        }
    }

    fn get_error_message(&self) -> String {
        self.to_string()
    }
}

/// A network with no accounts on chain and at most one stealth UTXO, in shard 0.
#[derive(Debug, Clone, Default)]
struct OneUtxoNetwork {
    utxo: Arc<Mutex<Option<UtxoUnspent>>>,
}

impl OneUtxoNetwork {
    fn set_utxo(&self, utxo: UtxoUnspent) {
        *self.utxo.lock().unwrap() = Some(utxo);
    }
}

impl WalletNetworkInterface for OneUtxoNetwork {
    type Error = FakeError;

    async fn query_substate(
        &self,
        _address: &SubstateId,
        _version: Option<SubstateVersion>,
        _local_search_only: bool,
    ) -> Result<SubstateQueryResult, Self::Error> {
        Err(FakeError::NotFound)
    }

    async fn get_substates(&self, _: Vec<SubstateId>) -> Result<HashMap<SubstateId, Substate>, Self::Error> {
        panic!("OneUtxoNetwork::get_substates called")
    }

    async fn submit_transaction(&self, _: Transaction) -> Result<TransactionId, Self::Error> {
        panic!("OneUtxoNetwork::submit_transaction called")
    }

    async fn submit_transaction_envelope(&self, _: TransactionEnvelope) -> Result<TransactionId, Self::Error> {
        panic!("OneUtxoNetwork::submit_transaction_envelope called")
    }

    async fn submit_dry_run_transaction(&self, _: Transaction) -> Result<TransactionQueryResult, Self::Error> {
        panic!("OneUtxoNetwork::submit_dry_run_transaction called")
    }

    async fn query_transaction_result(&self, _: TransactionId) -> Result<TransactionQueryResult, Self::Error> {
        panic!("OneUtxoNetwork::query_transaction_result called")
    }

    async fn subscribe_transaction_finalized(&self) -> Result<TransactionFinalizedStream<Self::Error>, Self::Error> {
        Ok(futures::stream::pending().boxed())
    }

    async fn fetch_template_definition(&self, _: TemplateAddress) -> Result<TemplateDef, Self::Error> {
        panic!("OneUtxoNetwork::fetch_template_definition called")
    }

    async fn stream_stealth_utxo_updates(
        &self,
        _from_epoch: Epoch,
        _resource_address: ResourceAddress,
        shard_state_versions: Vec<(Shard, StateVersion)>,
        _unspent_only: bool,
    ) -> Result<UtxoUpdateStream<Self::Error>, Self::Error> {
        let shard = Shard::from(0u32);
        let already_synced = shard_state_versions
            .iter()
            .any(|(s, v)| *s == shard && *v >= StateVersion::from(1));
        let Some(utxo) = self.utxo.lock().unwrap().clone().filter(|_| !already_synced) else {
            return Ok(futures::stream::empty().boxed());
        };
        let payload = UtxoUpdatePayload {
            sos: Some(StartOfShard {
                shard,
                max_state_version: StateVersion::from(1),
                has_more: false,
            }),
            update: Some(WalletUtxoUpdate::Unspent(utxo)),
            eos: None,
        };
        Ok(futures::stream::iter([Ok(payload)]).boxed())
    }

    async fn list_watched_substates(
        &self,
        _: Option<TemplateAddress>,
        _: Option<u64>,
        _: Option<u64>,
    ) -> Result<Vec<WatchedSubstateItem>, Self::Error> {
        panic!("OneUtxoNetwork::list_watched_substates called")
    }

    async fn get_unspent_utxos(
        &self,
        _: ResourceAddress,
        _: Vec<TagAndPublicNoncePair>,
    ) -> Result<Vec<(UtxoId, Utxo)>, Self::Error> {
        panic!("OneUtxoNetwork::get_unspent_utxos called")
    }

    async fn get_current_epoch(&self) -> Result<Epoch, Self::Error> {
        panic!("OneUtxoNetwork::get_current_epoch called")
    }

    async fn wait_until_ready(&self) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Builds a UTXO that the view key at `key_index` recognises as its own.
fn utxo_for_key_index(test: &TestWithNetwork<impl WalletNetworkInterface>, key_index: u64) -> UtxoUnspent {
    let sdk = test.sdk();
    let view_key = sdk
        .key_manager_api()
        .get_key(KeyId::derived(KeyBranch::ViewOnlyKey, key_index))
        .unwrap();
    let (_, public_nonce) = random_keypair();
    let tag: UtxoTag = sdk.stealth_crypto_api().derive_stealth_output_tag(
        sdk.network(),
        &view_key.secret,
        &public_nonce,
        &STEALTH_TARI_RESOURCE_ADDRESS,
    );
    UtxoUnspent {
        tag,
        public_nonce: public_nonce.to_byte_type(),
    }
}

fn account_address_for_key_index(
    test: &TestWithNetwork<impl WalletNetworkInterface>,
    key_index: u64,
) -> ComponentAddress {
    let key = test.sdk().key_manager_api().derive_account_key(key_index).unwrap();
    let public_key = RistrettoPublicKey::from_secret_key(&key.key).to_byte_type();
    test.sdk()
        .accounts_api()
        .derive_account_address_from_public_key(&public_key)
}

#[tokio::test(flavor = "multi_thread")]
async fn recovery_keeps_stealth_only_accounts_and_removes_unused_ones() {
    const ABANDON_AFTER_NOT_FOUND: usize = 3;

    let network = OneUtxoNetwork::default();
    let test = TestWithNetwork::with_network(network.clone());
    network.set_utxo(utxo_for_key_index(&test, 1));
    let sdk = test.sdk().clone();

    let shutdown = Shutdown::new();
    let notify = Notify::new(100);
    let (_scanner_join, utxo_scanner_handle) = StealthUtxoScannerWorker::new(sdk.clone(), notify.clone()).spawn();
    let (account_monitor, account_monitor_handle) =
        AccountMonitor::new(notify, sdk.clone(), utxo_scanner_handle.clone(), shutdown.to_signal());
    tokio::spawn(account_monitor.run());

    let seed_birthday = sdk.key_manager_api().get_cipher_seed_birthday_epoch().unwrap();
    let recovery = AccountRecoveryService::new(
        sdk.clone(),
        account_monitor_handle,
        utxo_scanner_handle,
        ABANDON_AFTER_NOT_FOUND,
        seed_birthday,
    );
    tokio::time::timeout(Duration::from_secs(30), recovery.scan())
        .await
        .expect("recovery did not finish");

    let accounts_api = sdk.accounts_api();
    // Key 0 is unused, key 1 holds a UTXO, and keys 2..=4 are the unused keys that end the scan.
    assert!(
        !accounts_api
            .exists_by_address(&account_address_for_key_index(&test, 0))
            .unwrap()
    );
    let stealth_account = account_address_for_key_index(&test, 1);
    assert!(accounts_api.exists_by_address(&stealth_account).unwrap());
    for index in 2..=4 {
        assert!(
            !accounts_api
                .exists_by_address(&account_address_for_key_index(&test, index))
                .unwrap(),
            "unused account at key index {index} was not removed"
        );
    }
    assert!(!sdk.config_api().get::<bool>(ConfigKey::RecoveryNeeded).unwrap());
    assert_eq!(
        sdk.config_api()
            .get::<u64>(ConfigKey::RecoveryMaxProbedKeyIndex)
            .unwrap(),
        4
    );

    // The next account reuses a removed key index, so it scans from the seed birthday.
    let address = sdk.key_manager_api().next_account_address().unwrap();
    assert_eq!(address.owner_key_id.derived_index(), Some(2));
    let created = accounts_api.create_account(Some("new"), false, address).unwrap();
    assert_eq!(created.account.birthday_epoch, seed_birthday);

    shutdown.trigger();
}

#[tokio::test(flavor = "multi_thread")]
async fn utxo_scanner_answers_every_waiting_scan_beyond_its_concurrency_limit() {
    const NUM_ACCOUNTS: u64 = 25;

    let test = TestWithNetwork::with_network(OneUtxoNetwork::default());
    let sdk = test.sdk().clone();
    let (_scanner_join, utxo_scanner_handle) = StealthUtxoScannerWorker::new(sdk.clone(), Notify::new(100)).spawn();

    let mut accounts = Vec::new();
    for index in 1..=NUM_ACCOUNTS {
        let address = sdk.key_manager_api().derive_account_address(index).unwrap();
        let account = sdk
            .accounts_api()
            .create_account(Some(&format!("account-{index}")), false, address)
            .unwrap();
        accounts.push(*account.account.component_address());
    }

    let scans = accounts.iter().flat_map(|account| {
        // A waiting scan for an account whose scan is already running must still be answered.
        [
            utxo_scanner_handle.scan(*account, STEALTH_TARI_RESOURCE_ADDRESS),
            utxo_scanner_handle.scan(*account, STEALTH_TARI_RESOURCE_ADDRESS),
        ]
    });
    let results = tokio::time::timeout(Duration::from_secs(30), futures::future::join_all(scans))
        .await
        .expect("a waiting scan was never answered");
    for result in results {
        result.unwrap();
    }
}
