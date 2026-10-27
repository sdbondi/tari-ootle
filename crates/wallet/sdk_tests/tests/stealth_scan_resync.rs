//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! A wallet whose indexer may lack the spends of a shard below the scan cursor rescans the shard for its unspent
//! outputs, and marks spent each output it holds that the rescan does not return, once the indexer shows it gone.

mod support;

use tari_engine_types::{
    Utxo,
    substate::{SubstateId, SubstateValue},
};
use tari_ootle_common_types::{
    Epoch,
    NumPreshards,
    StateVersion,
    SubstateVersion,
    VersionedSubstateIdRef,
    shard::Shard,
};
use tari_ootle_light_client::test_fixtures::{ValidatorSetFixture, substate_down_proof, substate_proof};
use tari_ootle_wallet_sdk::{
    IndexerTrust,
    models::{
        AccountWithAddress,
        EndOfShard,
        OutputStatus,
        ProvenUp,
        StartOfShard,
        StealthOutputModel,
        UtxoUnspent,
        UtxoUpdatePayload,
        WalletUtxoUpdate,
    },
};
use tari_ootle_wallet_sdk_services::{
    notify::Notify,
    utxo_scanner::{StealthScannerApiError, StealthUtxoScannerWorker, UtxoScanRoundStats},
};
use tari_template_lib::types::{UtxoAddress, UtxoId, constants::STEALTH_TARI_RESOURCE_ADDRESS};

use crate::support::{ProvingNetwork, Test, TestWithNetwork};

const VALUE: u64 = 1_234;
const VERSION: SubstateVersion = SubstateVersion::ZERO;
/// The indexer's tombstone horizon of every shard, and where a rescan of a shard ends.
const HORIZON: StateVersion = StateVersion::new(50);

type Wallet = TestWithNetwork<ProvingNetwork>;

/// A wallet holding the test account, anchored on the validator set of epoch 2 and walked to that of epoch 5.
async fn wallet(indexer_trust: IndexerTrust) -> (Vec<ValidatorSetFixture>, ProvingNetwork, Wallet, AccountWithAddress) {
    let sets = (1..=5).map(ValidatorSetFixture::new).collect::<Vec<_>>();
    let network = ProvingNetwork::new(&sets, 0);
    let test = TestWithNetwork::with_network_trusting(network.clone(), indexer_trust);
    test.sdk().validator_set_api().sync().await.unwrap();
    network.set_latest(3);
    let trusted = test.sdk().validator_set_api().sync().await.unwrap();
    assert_eq!(trusted.epoch(), Epoch(5));
    let account = test
        .sdk()
        .accounts_api()
        .get_account_by_address(&Test::test_account_address())
        .unwrap();
    (sets, network, test, account)
}

fn substate_id(id: UtxoId) -> SubstateId {
    SubstateId::Utxo(UtxoAddress::new(STEALTH_TARI_RESOURCE_ADDRESS, id))
}

fn shard_of(id: UtxoId) -> Shard {
    VersionedSubstateIdRef::new(&substate_id(id), VERSION).to_shard(NumPreshards::P256)
}

fn unspent(utxo: &Utxo) -> UtxoUnspent {
    let output = utxo.output().unwrap();
    UtxoUnspent {
        tag: output.tag,
        public_nonce: output.output.public_nonce,
    }
}

/// The wallet holds `utxo` unspent, proven up at [`VERSION`] in `proven_up_epoch`.
fn hold(test: &Wallet, account: &AccountWithAddress, id: UtxoId, utxo: &Utxo, proven_up_epoch: Option<u64>) {
    let output = utxo.output().unwrap();
    test.sdk()
        .stealth_outputs_api()
        .add_output(&StealthOutputModel {
            owner_account: *account.account.component_address(),
            resource_address: STEALTH_TARI_RESOURCE_ADDRESS,
            commitment: id.into_commitment_bytes(),
            value: VALUE,
            sender_public_nonce: output.output.public_nonce,
            view_only_key_id: account.view_only_key_id(),
            owner_key_id: account.owner_key_id(),
            encrypted_data: output.output.encrypted_data.clone(),
            tag_byte: output.tag,
            memo: None,
            auth: output.auth.clone(),
            minimum_value_promise: 0,
            status: OutputStatus::Unspent,
            is_burnt: false,
            is_frozen: false,
            is_on_chain: true,
            is_condition_spendable: true,
            lock_id: None,
            proven_up: proven_up_epoch.map(|epoch| ProvenUp {
                version: VERSION,
                epoch: Epoch(epoch),
            }),
            spent_version: None,
        })
        .unwrap();
}

/// An indexer whose tombstone horizon is [`HORIZON`] in every shard. A request for spends from below it is asked to
/// resync, and a request for unspent outputs from below it returns those of `live` in the shard and ends at it.
fn index_below_horizon(network: &ProvingNetwork, live: Vec<(UtxoId, UtxoUnspent)>) {
    network.answer_utxo_streams(move |cursors, unspent_only| {
        let mut payloads = Vec::new();
        for (shard, cursor) in cursors {
            if *cursor >= HORIZON {
                continue;
            }
            let sos = StartOfShard {
                shard: *shard,
                max_state_version: HORIZON,
                has_more: false,
                resync_required: !unspent_only,
            };
            let mut updates = if unspent_only {
                live.iter()
                    .filter(|(id, _)| shard_of(*id) == *shard)
                    .map(|(_, unspent)| WalletUtxoUpdate::Unspent(unspent.clone()))
                    .collect()
            } else {
                Vec::new()
            };
            payloads.push(UtxoUpdatePayload {
                sos: Some(sos),
                update: updates.pop(),
                eos: None,
            });
            payloads.extend(updates.into_iter().map(|update| UtxoUpdatePayload {
                sos: None,
                update: Some(update),
                eos: None,
            }));
            payloads.push(UtxoUpdatePayload {
                sos: None,
                update: None,
                eos: Some(EndOfShard {
                    max_state_version: HORIZON,
                }),
            });
        }
        payloads
    });
}

/// The indexer serves `id` down at [`VERSION`] with only its exclusion, anchored in the latest trusted epoch, 5.
fn serve_excluded(sets: &[ValidatorSetFixture], network: &ProvingNetwork, id: UtxoId) {
    let substate_id = substate_id(id);
    let mut proof = substate_down_proof(&sets[4], &sets[4], &substate_id, VERSION);
    proof.down_proof = None;
    network.serve_down_substate(substate_id, VERSION, Some(proof));
}

/// The indexer serves `id` live at [`VERSION`], proven by the committee of the latest trusted validator set.
fn serve_live(sets: &[ValidatorSetFixture], network: &ProvingNetwork, id: UtxoId, utxo: &Utxo) {
    let value = SubstateValue::Utxo(utxo.clone());
    let proof = substate_proof(&sets[4], &substate_id(id), VERSION, &value);
    network.serve_substate(substate_id(id), VERSION, value, Some(proof));
}

async fn try_scan(test: &Wallet) -> Result<UtxoScanRoundStats, StealthScannerApiError> {
    let (_join, scanner) = StealthUtxoScannerWorker::new(test.sdk().clone(), Notify::new(100)).spawn();
    scanner
        .scan(Test::test_account_address(), STEALTH_TARI_RESOURCE_ADDRESS)
        .await
}

async fn scan(test: &Wallet) -> UtxoScanRoundStats {
    try_scan(test).await.unwrap()
}

fn status(test: &Wallet, id: UtxoId) -> OutputStatus {
    test.sdk()
        .stealth_outputs_api()
        .utxos_get_many(&STEALTH_TARI_RESOURCE_ADDRESS, None, None)
        .unwrap()
        .into_iter()
        .find(|output| output.commitment == id.into_commitment_bytes())
        .expect("the output is stored")
        .status
}

/// Scans again, and asserts that the scan resumed every shard from at least [`HORIZON`] and resynced none.
async fn assert_resumed_past_horizon_after_scan(test: &Wallet, network: &ProvingNetwork) {
    let stats = scan(test).await;
    assert_eq!(stats.num_resynced_shards, 0);
    let requests = network.utxo_stream_requests();
    let last = requests.last().unwrap();
    assert_eq!(last.len(), NumPreshards::P256.as_u32() as usize);
    assert!(last.iter().all(|(_, cursor)| *cursor >= HORIZON), "{last:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_output_a_resync_does_not_return_is_marked_spent_once_proven_gone() {
    let (sets, network, test, account) = wallet(IndexerTrust::Verify).await;
    let (returned_id, returned) = test.owned_utxo(&account, VALUE);
    let (gone_id, gone) = test.owned_utxo(&account, VALUE);
    let (live_id, live) = test.owned_utxo(&account, VALUE);
    for (id, utxo) in [(returned_id, &returned), (gone_id, &gone), (live_id, &live)] {
        hold(&test, &account, id, utxo, Some(3));
    }
    serve_excluded(&sets, &network, gone_id);
    // Live, but committed past where the rescan read to.
    serve_live(&sets, &network, live_id, &live);
    index_below_horizon(&network, vec![(returned_id, unspent(&returned))]);

    let stats = scan(&test).await;
    assert_eq!(stats.num_resynced_shards, NumPreshards::P256.as_u32() as usize);
    assert_eq!(stats.num_spent, 1);
    assert_eq!(status(&test, gone_id), OutputStatus::Spent);
    assert_eq!(status(&test, live_id), OutputStatus::Unspent);
    assert_eq!(status(&test, returned_id), OutputStatus::Unspent);
    assert_eq!(
        network.substate_queries(),
        2,
        "only the outputs the resync did not return are checked"
    );

    let stats = scan(&test).await;
    assert_eq!(stats.num_resynced_shards, 0);
    assert_resumed_past_horizon_after_scan(&test, &network).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_held_output_never_proven_up_is_kept_when_a_resync_does_not_return_it() {
    let (sets, network, test, account) = wallet(IndexerTrust::Verify).await;
    let (id, utxo) = test.owned_utxo(&account, VALUE);
    hold(&test, &account, id, &utxo, None);
    serve_excluded(&sets, &network, id);
    index_below_horizon(&network, vec![]);

    let stats = scan(&test).await;
    assert_eq!(stats.num_resynced_shards, NumPreshards::P256.as_u32() as usize);
    assert_eq!(stats.num_spent, 0);
    assert_eq!(status(&test, id), OutputStatus::Unspent);
    assert_eq!(network.substate_queries(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_resync_whose_check_could_not_be_made_is_asked_for_again() {
    let (sets, network, test, account) = wallet(IndexerTrust::Verify).await;
    let (id, utxo) = test.owned_utxo(&account, VALUE);
    hold(&test, &account, id, &utxo, Some(3));
    serve_excluded(&sets, &network, id);
    index_below_horizon(&network, vec![]);
    network.fail_substate_queries(1);

    let err = try_scan(&test).await.unwrap_err();
    assert!(matches!(err, StealthScannerApiError::SubstateApiError(_)), "{err}");
    assert_eq!(status(&test, id), OutputStatus::Unspent);
    let shard = shard_of(id);
    let requests = network.utxo_stream_requests();
    assert!(
        requests.last().unwrap().contains(&(shard, StateVersion::zero())),
        "the shard's resync was under way"
    );

    let stats = scan(&test).await;
    assert!(stats.num_resynced_shards >= 1);
    assert_eq!(stats.num_spent, 1);
    assert_eq!(status(&test, id), OutputStatus::Spent);
    assert_resumed_past_horizon_after_scan(&test, &network).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_trusting_wallet_marks_spent_a_held_output_the_indexer_no_longer_serves() {
    let (sets, network, test, account) = wallet(IndexerTrust::Trust).await;
    let (gone_id, gone) = test.owned_utxo(&account, VALUE);
    let (live_id, live) = test.owned_utxo(&account, VALUE);
    hold(&test, &account, gone_id, &gone, None);
    hold(&test, &account, live_id, &live, None);
    network.serve_down_substate(substate_id(gone_id), VERSION, None);
    serve_live(&sets, &network, live_id, &live);
    index_below_horizon(&network, vec![]);

    let stats = scan(&test).await;
    assert_eq!(stats.num_spent, 1);
    assert_eq!(status(&test, gone_id), OutputStatus::Spent);
    assert_eq!(status(&test, live_id), OutputStatus::Unspent);
    assert_resumed_past_horizon_after_scan(&test, &network).await;
}

/// A wallet holding nothing asks only for unspent outputs, which the indexer never asks to resync.
#[tokio::test(flavor = "multi_thread")]
async fn a_wallet_holding_nothing_is_not_asked_to_resync() {
    let (_, network, test, _) = wallet(IndexerTrust::Verify).await;
    index_below_horizon(&network, vec![]);

    let stats = scan(&test).await;
    assert_eq!(stats.num_resynced_shards, 0);
    assert_resumed_past_horizon_after_scan(&test, &network).await;
}
