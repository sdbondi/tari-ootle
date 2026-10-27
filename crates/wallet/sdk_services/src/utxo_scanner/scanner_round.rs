//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    mem,
};

use futures::StreamExt;
use log::{debug, info, trace, warn};
use ootle_byte_type::FromByteType;
use tari_engine_types::substate::SubstateId;
use tari_ootle_common_types::{
    NumPreshards,
    StateVersion,
    SubstateVersion,
    VersionedSubstateIdRef,
    optional::Optional,
    shard::Shard,
};
use tari_ootle_transaction::Network;
use tari_ootle_wallet_sdk::{
    NetworkInterfaceError,
    WalletSdk,
    WalletSdkSpec,
    apis::{substate::SubstateApiError, validator_set::ValidatorSetApiError},
    models::{
        AccountWithAddress,
        CheckpointStateVersion,
        ClaimedStateVersion,
        OutputStatus,
        StartOfShard,
        StealthOutputModel,
        UtxoSpent,
        UtxoSpentEvent,
        UtxoUnspent,
        WalletEvent,
        WalletSecretKey,
        WalletUtxoUpdate,
    },
    network::{UtxoUpdateStream, WalletNetworkInterface},
    storage::{ReadableWalletStore, WalletStorageError, WalletStoreReader, WalletStoreWriter, WriteableWalletStore},
};
use tari_template_lib_types::{
    ComponentAddress,
    ResourceAddress,
    UtxoAddress,
    crypto::{RistrettoPublicKeyBytes, UtxoTag},
};

use crate::{notify::Notify, utxo_scanner::StealthScannerApiError};

const LOG_TARGET: &str = "tari::ootle::wallet_services::scanner_round";
// TODO: either fetch num preshards from the network or we should hardcode it to a single value for all apps
const NUM_PRESHARDS: NumPreshards = NumPreshards::P256;

/// A claim expires once the wallet verifies a checkpoint at least this many epochs after the latest one it had verified
/// when the claim began. The network may have ended one more epoch than the wallet had verified by then, so the
/// checkpoint after that is the first that certainly ended after the claim began, and an honest claim cannot be past
/// it.
const CLAIM_EXPIRY_EPOCHS: u64 = 2;

pub struct UtxoScannerRound<'a, TSpec: WalletSdkSpec> {
    network: Network,
    account: &'a AccountWithAddress,
    view_key: &'a WalletSecretKey,
    resource_address: &'a ResourceAddress,

    sdk: &'a WalletSdk<TSpec>,
    stats: UtxoScanRoundStats,

    /// The cursor each shard's next batch is requested from. It follows the indexer's answers within the round, and is
    /// stored only as far as `stored_cursor_bound` allows, with any progress past it stored as a claim.
    cursors: HashMap<Shard, StateVersion>,
    stored_cursor_bound: StoredCursorBound,
    /// The live claim of each shard.
    claims: HashMap<Shard, ClaimedStateVersion>,
    /// The shards this round resumed from a claim. Their stored cursors stay where the claim began, so that the scan
    /// after the claim expires asks again for every update the claim may have skipped.
    resumed_from_claim: HashSet<Shard>,
    shard_state_versions_to_set: HashMap<Shard, StateVersion>,
    /// The shards being rescanned for their unspent outputs because the indexer may lack spends below their cursors.
    resyncs: HashMap<Shard, Resync>,
    /// The shards this round has resynced. A round resyncs a shard at most once, so that an indexer that keeps asking
    /// cannot keep it from ending.
    resynced: HashSet<Shard>,
    utxos_to_recover: Vec<(ComponentAddress, UtxoUnspent)>,
    utxos_to_spend: Vec<UtxoSpent>,
    /// The spends of held outputs that a resync did not return, which the indexer has already shown gone.
    reconciled_spends: Vec<UtxoSpent>,
    notify: &'a Notify<WalletEvent>,
}

impl<'a, TSpec> UtxoScannerRound<'a, TSpec>
where TSpec: WalletSdkSpec
{
    pub fn new(
        network: Network,
        sdk: &'a WalletSdk<TSpec>,
        account: &'a AccountWithAddress,
        view_key: &'a WalletSecretKey,
        resource_address: &'a ResourceAddress,
        notify: &'a Notify<WalletEvent>,
    ) -> Self {
        Self {
            network,
            sdk,
            account,
            view_key,
            resource_address,
            cursors: HashMap::new(),
            stored_cursor_bound: StoredCursorBound::Unbounded,
            claims: HashMap::new(),
            resumed_from_claim: HashSet::new(),
            shard_state_versions_to_set: HashMap::new(),
            resyncs: HashMap::new(),
            resynced: HashSet::new(),
            utxos_to_recover: Vec::new(),
            utxos_to_spend: Vec::new(),
            reconciled_spends: Vec::new(),
            notify,
            stats: UtxoScanRoundStats::default(),
        }
    }

    pub async fn scan_for_utxo_updates(&mut self) -> Result<(), StealthScannerApiError> {
        self.stored_cursor_bound = self.load_stored_cursor_bound().await?;
        self.load_cursors()?;
        loop {
            if !self.process_next_batch().await? {
                break;
            }
        }

        Ok(())
    }

    pub fn into_stats(self) -> UtxoScanRoundStats {
        self.stats
    }

    /// Under [`IndexerTrust::Verify`](tari_ootle_wallet_sdk::IndexerTrust::Verify) the stored cursors are bounded by
    /// the shard state versions of the latest verified end-of-epoch checkpoints. Without them the stored cursors stay
    /// where they are, and this round stores its progress only as claims.
    async fn load_stored_cursor_bound(&self) -> Result<StoredCursorBound, StealthScannerApiError> {
        if !self.sdk.sdk_config().indexer_trust.is_verify() {
            return Ok(StoredCursorBound::Unbounded);
        }
        match self.sdk.validator_set_api().verified_shard_state_versions().await {
            Ok(versions) => Ok(StoredCursorBound::Verified(versions)),
            Err(ValidatorSetApiError::StoreError(err)) => Err(err.into()),
            Err(err) => {
                warn!(
                    target: LOG_TARGET,
                    "⚠️ No verified shard state versions to bound the stealth scan cursors by: {err}. The scan stores its progress only as claims."
                );
                Ok(StoredCursorBound::Verified(HashMap::new()))
            },
        }
    }

    /// Loads the stored cursors, lowering any that are past the bound to it, and resumes each shard from its claim
    /// until the claim expires.
    fn load_cursors(&mut self) -> Result<(), StealthScannerApiError> {
        let account = self.account.component_address();
        let (stored, claims) = self.sdk.store().with_read_tx(|tx| {
            let stored = tx.shard_state_version_get(account, self.resource_address)?;
            let claims = if self.stored_cursor_bound.is_verified() {
                tx.shard_state_version_claims_get(account, self.resource_address)?
            } else {
                HashMap::new()
            };
            Ok::<_, WalletStorageError>((stored, claims))
        })?;

        let mut lowered = Vec::new();
        let mut expired = Vec::new();
        let mut disproved = 0usize;
        for shard in NUM_PRESHARDS.all_shards_iter() {
            let stored = stored.get(&shard).copied().unwrap_or_else(StateVersion::zero);
            let mut cursor = match self.stored_cursor_bound.ceiling(shard) {
                Some(ceiling) if stored > ceiling => {
                    lowered.push((shard, ceiling));
                    ceiling
                },
                _ => stored,
            };
            if let Some(claim) = claims.get(&shard) {
                if self.stored_cursor_bound.expires(shard, claim) || claim.state_version <= cursor {
                    if self.stored_cursor_bound.disproves(shard, claim) {
                        disproved += 1;
                    }
                    expired.push(shard);
                } else {
                    cursor = claim.state_version;
                    self.claims.insert(shard, *claim);
                    self.resumed_from_claim.insert(shard);
                }
            }
            self.cursors.insert(shard, cursor);
        }

        if disproved > 0 {
            warn!(
                target: LOG_TARGET,
                "⚠️ {disproved} stealth scan cursor(s) of account {account} (resource {}) that an indexer claimed are past the state version a later verified checkpoint commits, so that indexer reported updates the network never committed.",
                self.resource_address,
            );
        }
        if !expired.is_empty() {
            debug!(
                target: LOG_TARGET,
                "{} claimed stealth scan cursor(s) of account {account} (resource {}) expired. Rescanning those shards from the verified cursor.",
                expired.len(),
                self.resource_address,
            );
            self.sdk
                .store()
                .with_write_tx(|tx| tx.shard_state_version_claims_remove(account, self.resource_address, expired))?;
        }

        if !lowered.is_empty() {
            warn!(
                target: LOG_TARGET,
                "⚠️ {} stored stealth scan cursor(s) of account {} (resource {}) were past the verified shard state version. Rescanning those shards from it.",
                lowered.len(),
                self.account.component_address(),
                self.resource_address,
            );
            self.sdk.store().with_write_tx(|tx| {
                tx.shard_state_version_set_many(self.account.component_address(), self.resource_address, lowered)
            })?;
        }
        Ok(())
    }

    async fn process_next_batch(&mut self) -> Result<bool, StealthScannerApiError> {
        info!(
            target: LOG_TARGET,
            "🔍️ Scanning for stealth outputs (account {}, resource {}, num shards {}, resyncing {})",
            self.account.component_address(),
            self.resource_address,
            self.cursors.len(),
            self.resyncs.len(),
        );

        let mut has_more = false;
        // NOTE that this will request shards in a random order (HashMap). This is good to avoid always starting with
        // the same shard.
        let cursors = self
            .cursors
            .iter()
            .filter(|(shard, _)| !self.resyncs.contains_key(shard))
            .map(|(shard, cursor)| (*shard, *cursor))
            .collect::<Vec<_>>();
        if !cursors.is_empty() {
            let live_output_count = self
                .sdk
                .stealth_outputs_api()
                .count_unspent_or_locked_outputs_for_account(self.account.component_address(), self.resource_address)?;
            // Optimisation: with no unspent or locked outputs there is no spend left for the wallet to learn of, so
            // only unspent updates are needed. The indexer moves the cursor past the spent updates this skips, so a
            // later pass from that cursor does not deliver them either.
            let stream = self.open_stream(cursors, live_output_count == 0).await?;
            has_more |= self.process_stream(stream, ScanMode::Incremental).await?;
        }

        // A shard the pass above flagged is resynced in the same batch.
        if !self.resyncs.is_empty() {
            let cursors = self
                .resyncs
                .iter()
                .map(|(shard, resync)| (*shard, resync.cursor))
                .collect();
            let stream = self.open_stream(cursors, true).await?;
            has_more |= self.process_stream(stream, ScanMode::Resync).await?;
        }

        Ok(has_more)
    }

    async fn open_stream(
        &self,
        cursors: Vec<(Shard, StateVersion)>,
        unspent_only: bool,
    ) -> Result<UtxoUpdateStream<NetworkInterfaceError<TSpec>>, StealthScannerApiError> {
        self.sdk
            .get_network_interface()
            .stream_stealth_utxo_updates(
                self.account.birthday_epoch(),
                *self.resource_address,
                cursors,
                unspent_only,
            )
            .await
            .map_err(|e| StealthScannerApiError::NetworkInterfaceError(e.into()))
    }

    async fn process_stream(
        &mut self,
        mut stream: UtxoUpdateStream<NetworkInterfaceError<TSpec>>,
        mode: ScanMode,
    ) -> Result<bool, StealthScannerApiError> {
        let mut num_received = 0usize;
        let mut sos: Option<StartOfShard> = None;
        // Whether the stream is in a shard flagged for a resync, whose frames are skipped up to its EndOfShard.
        let mut skipping = false;
        let mut has_more = false;

        while let Some(result) = stream.next().await {
            let payload = result.map_err(|e| StealthScannerApiError::NetworkInterfaceError(e.into()))?;
            if let Some(recv_sos) = payload.sos {
                debug!(target: LOG_TARGET, "Received StartOfShard for {}", recv_sos.shard);
                if let Some(unterminated) = sos.take() {
                    warn!(
                        target: LOG_TARGET,
                        "⚠️ Shard {} ended without an EndOfShard. Its scan cursor is left in place.",
                        unterminated.shard
                    );
                }
                skipping = mode == ScanMode::Incremental && recv_sos.resync_required;
                if skipping {
                    self.begin_resync(recv_sos.shard)?;
                } else {
                    sos = Some(recv_sos);
                }
            }
            if skipping {
                skipping = payload.eos.is_none();
                continue;
            }

            if let Some(update) = payload.update {
                num_received += 1;
                self.process_update(update, sos.as_ref())?;
            }

            if payload.eos.is_some() {
                // The shard's cursor advances only once every update up to its max state version has been received.
                let Some(sos) = sos.take() else {
                    warn!(target: LOG_TARGET, "⚠️ Received an EndOfShard without a StartOfShard. Ignoring it.");
                    continue;
                };
                debug!(
                    target: LOG_TARGET,
                    "🔍️ Scanned shard {} for account {} up to state version {}",
                    sos.shard,
                    self.account.component_address(),
                    sos.max_state_version
                );
                match mode {
                    ScanMode::Incremental => {
                        for versions in [&mut self.cursors, &mut self.shard_state_versions_to_set] {
                            versions
                                .entry(sos.shard)
                                .and_modify(|v| {
                                    if sos.max_state_version > *v {
                                        *v = sos.max_state_version
                                    }
                                })
                                .or_insert(sos.max_state_version);
                        }
                    },
                    ScanMode::Resync => self.advance_resync(&sos).await?,
                }
                has_more |= sos.has_more;

                self.stats.num_received += num_received;
                num_received = 0;
                self.commit_progress().await?;
            }
        }

        if let Some(unterminated) = sos {
            warn!(
                target: LOG_TARGET,
                "⚠️ Shard {} ended without an EndOfShard. Its scan cursor is left in place.",
                unterminated.shard
            );
        }

        self.commit_progress().await?;
        self.stats.num_received += num_received;

        if !has_more {
            info!(
                target: LOG_TARGET,
                "🔍️ Scan complete for account {}: No more stealth outputs found",
                self.account.component_address()
            );
        }

        Ok(has_more)
    }

    /// Queues `update` of the shard that `sos` began.
    fn process_update(
        &mut self,
        update: WalletUtxoUpdate,
        sos: Option<&StartOfShard>,
    ) -> Result<(), StealthScannerApiError> {
        match update {
            WalletUtxoUpdate::Unspent(unspent) => {
                if self.check_if_tag_matches(&unspent)? {
                    debug!(
                        target: LOG_TARGET,
                        "🏷️ Stealth output tag {} matches. Queueing for recovery.",
                        unspent.tag
                    );
                    if let Some(resync) = sos.and_then(|sos| self.resyncs.get_mut(&sos.shard)) {
                        resync.returned.insert((unspent.tag, unspent.public_nonce));
                    }
                    self.utxos_to_recover.push((*self.account.component_address(), unspent));
                }
            },
            WalletUtxoUpdate::Spent(spent) => {
                self.utxos_to_spend.push(spent);
            },
            WalletUtxoUpdate::Burnt(burnt) => {
                // NOTE: we treat burnt outputs the same as spent for now. In future, we may want to track them
                // separately
                self.utxos_to_spend.push(UtxoSpent {
                    id: burnt.id,
                    version: burnt.version,
                });
            },
        }
        Ok(())
    }

    async fn commit_progress(&mut self) -> Result<bool, StealthScannerApiError> {
        if self.utxos_to_recover.is_empty() &&
            self.utxos_to_spend.is_empty() &&
            self.reconciled_spends.is_empty() &&
            self.shard_state_versions_to_set.is_empty()
        {
            return Ok(false);
        }

        if self.sdk.sdk_config().indexer_trust.is_verify() {
            self.retain_proven_spends().await?;
        }
        self.utxos_to_spend.append(&mut self.reconciled_spends);

        let mut num_spent = 0;
        let num_recovered = self.utxos_to_recover.len();
        self.stats.num_potential_recoveries += num_recovered;
        self.sdk.store().with_write_tx(|tx| {
            // Mark UTXOs as spent (if they exist)
            for spent in self.utxos_to_spend.drain(..) {
                if Self::spend(tx, self.resource_address, &spent)? {
                    self.notify.notify(UtxoSpentEvent {
                        account_address: *self.account.component_address(),
                        address: UtxoAddress::new(*self.resource_address, spent.id),
                    });
                    num_spent += 1;
                }
            }

            if !self.utxos_to_recover.is_empty() {
                // Queue up tag matching UTXOs for processing
                tx.utxo_process_queue_extend(self.resource_address, self.utxos_to_recover.drain(..))?;
            }

            let bound = &self.stored_cursor_bound;
            let mut cursors = Vec::new();
            let mut claims = Vec::new();
            for (shard, version) in self.shard_state_versions_to_set.drain() {
                if !self.resumed_from_claim.contains(&shard) &&
                    let Some(cursor) = bound.bound(shard, version)
                {
                    cursors.push((shard, cursor));
                }
                let claim = match self.claims.get(&shard) {
                    // A claim keeps where it began as it advances, so that it expires on schedule and is checked
                    // against what the indexer first reported.
                    Some(live) if version > live.state_version => Some(ClaimedStateVersion {
                        state_version: version,
                        ..*live
                    }),
                    Some(_) => None,
                    None => bound.claim(shard, version),
                };
                if let Some(claim) = claim {
                    self.claims.insert(shard, claim);
                    claims.push((shard, claim));
                }
            }
            if !cursors.is_empty() {
                tx.shard_state_version_set_many(self.account.component_address(), self.resource_address, cursors)?;
            }
            if !claims.is_empty() {
                tx.shard_state_version_claims_set_many(
                    self.account.component_address(),
                    self.resource_address,
                    claims,
                )?;
            }
            Ok::<_, StealthScannerApiError>(())
        })?;

        self.stats.num_spent += num_spent;
        Ok(true)
    }

    /// Drops each reported spend of an output the wallet holds as not spent, unless the indexer proves that the output
    /// has gone down at the reported version, with a down proof or, for the version the wallet holds proven, an
    /// exclusion (see [`SubstatesApi::verify_down`]). A spend is dropped only when the answer refuses it for good: the
    /// indexer serves no proof, a proof that does not verify, or one the wallet can never trust. The output then stays
    /// as it is and the spend is not asked for again. Any other failure fails the round before it stores its progress,
    /// so the next scan asks for the spend again.
    ///
    /// [`SubstatesApi::verify_down`]: tari_ootle_wallet_sdk::apis::substate::SubstatesApi::verify_down
    ///
    /// A spend of an output the wallet already holds as spent needs no proof, as the wallet settled that spend from its
    /// own verified transaction.
    async fn retain_proven_spends(&mut self) -> Result<(), StealthScannerApiError> {
        let substate_api = self.sdk.substate_api();
        let mut retained = Vec::with_capacity(self.utxos_to_spend.len());
        for spent in mem::take(&mut self.utxos_to_spend) {
            let held = self
                .sdk
                .store()
                .with_read_tx(|tx| {
                    tx.stealth_outputs_get_by_commitment(self.resource_address, &spent.id.into_commitment_bytes())
                })
                .optional()?;
            let Some(held) = held.filter(|output| !matches!(output.status, OutputStatus::Spent)) else {
                retained.push(spent);
                continue;
            };
            if Self::is_superseded(&held, &spent) {
                continue;
            }

            let address = UtxoAddress::new(*self.resource_address, spent.id);
            match substate_api
                .verify_down(&address.clone().into(), spent.version, held.proven_up)
                .await
            {
                Ok(()) => retained.push(spent),
                Err(err) if is_refusal(&err) => {
                    warn!(
                        target: LOG_TARGET,
                        "⚠️ The indexer reported stealth output {} spent at v{} without proving it ({}). Keeping the output.",
                        address,
                        spent.version,
                        err
                    );
                },
                Err(err) => return Err(err.into()),
            }
        }
        self.utxos_to_spend = retained;
        Ok(())
    }

    /// Starts rescanning `shard` for its unspent outputs from the start, unless this round has resynced it already. The
    /// shard's claim is dropped, and its stored cursor stays where it is until the resync completes, so a scan that
    /// fails before then is asked to resync again.
    fn begin_resync(&mut self, shard: Shard) -> Result<(), StealthScannerApiError> {
        if self.resynced.contains(&shard) {
            warn!(
                target: LOG_TARGET,
                "⚠️ The indexer asked again to resync shard {shard}, which this scan has resynced. Its scan cursor is left in place until the next scan.",
            );
            return Ok(());
        }
        info!(
            target: LOG_TARGET,
            "🔄️ The indexer may lack spends of shard {shard} below the scan cursor of account {} (resource {}). Rescanning the shard for unspent outputs.",
            self.account.component_address(),
            self.resource_address,
        );
        self.resumed_from_claim.remove(&shard);
        if self.claims.remove(&shard).is_some() {
            self.sdk.store().with_write_tx(|tx| {
                tx.shard_state_version_claims_remove(self.account.component_address(), self.resource_address, [shard])
            })?;
        }
        self.resyncs.insert(shard, Resync {
            cursor: StateVersion::zero(),
            returned: HashSet::new(),
        });
        Ok(())
    }

    /// Moves the resync of the shard that `sos` began to where the pass ended, and completes it once the indexer has
    /// no more to give: the outputs the wallet holds that the resync did not return are reconciled, and the shard's
    /// cursor moves to where the resync ended.
    async fn advance_resync(&mut self, sos: &StartOfShard) -> Result<(), StealthScannerApiError> {
        let Entry::Occupied(mut entry) = self.resyncs.entry(sos.shard) else {
            warn!(
                target: LOG_TARGET,
                "⚠️ The indexer streamed shard {}, which is not being resynced. Ignoring it.",
                sos.shard
            );
            return Ok(());
        };
        let resync = entry.get_mut();
        resync.cursor = resync.cursor.max(sos.max_state_version);
        if sos.has_more {
            return Ok(());
        }
        let resync = entry.remove();
        self.reconcile(sos.shard, &resync.returned).await?;
        self.cursors.insert(sos.shard, resync.cursor);
        self.shard_state_versions_to_set.insert(sos.shard, resync.cursor);
        self.resynced.insert(sos.shard);
        self.stats.num_resynced_shards += 1;
        Ok(())
    }

    /// Queues the spend of each output of `shard` the wallet holds as live and on chain that a resync of the shard did
    /// not return, once the indexer shows that it is gone. Under
    /// [`IndexerTrust::Verify`](tari_ootle_wallet_sdk::IndexerTrust::Verify) the indexer must prove the version the
    /// wallet holds proven up gone (see [`SubstatesApi::verify_down`]); otherwise it need only not serve the output.
    ///
    /// The indexer delivers an output that a freeze or unfreeze took to a later version in the resync, which matches
    /// the held output by tag and nonce, so such an output is not checked.
    ///
    /// [`SubstatesApi::verify_down`]: tari_ootle_wallet_sdk::apis::substate::SubstatesApi::verify_down
    async fn reconcile(
        &mut self,
        shard: Shard,
        returned: &HashSet<(UtxoTag, RistrettoPublicKeyBytes)>,
    ) -> Result<(), StealthScannerApiError> {
        let account = self.account.component_address();
        let missing = self
            .sdk
            .store()
            .with_read_tx(|tx| {
                let mut held = Vec::new();
                for status in [OutputStatus::Unspent, OutputStatus::LockedForSpend] {
                    held.extend(tx.stealth_outputs_get_many(self.resource_address, Some(account), Some(status))?);
                }
                Ok::<_, WalletStorageError>(held)
            })?
            .into_iter()
            .filter(|output| {
                output.is_on_chain &&
                    shard_of(output) == shard &&
                    !returned.contains(&(output.tag_byte, output.sender_public_nonce))
            })
            .collect::<Vec<_>>();
        if missing.is_empty() {
            return Ok(());
        }
        debug!(
            target: LOG_TARGET,
            "Resync of shard {shard} did not return {} held stealth output(s). Checking whether they are gone.",
            missing.len()
        );

        let substate_api = self.sdk.substate_api();
        if !self.sdk.sdk_config().indexer_trust.is_verify() {
            let ids = missing
                .iter()
                .map(|output| SubstateId::Utxo(output.to_utxo_address()))
                .collect();
            let live = substate_api.get_substates_from_network(ids).await?;
            for output in missing {
                if !live.contains_key(&SubstateId::Utxo(output.to_utxo_address())) {
                    // The indexer does not say which version went down. The latest the wallet saw up is the floor.
                    let version = output
                        .proven_up
                        .map_or(SubstateVersion::ZERO, |proven_up| proven_up.version);
                    self.reconciled_spends.push(UtxoSpent {
                        id: output.commitment.into(),
                        version,
                    });
                }
            }
            return Ok(());
        }

        for output in missing {
            let address = output.to_utxo_address();
            let Some(proven_up) = output.proven_up else {
                warn!(
                    target: LOG_TARGET,
                    "⚠️ Stealth output {address} was not returned by the resync of shard {shard}, and the wallet holds no proof of a version of it to check against. Keeping the output.",
                );
                continue;
            };
            match substate_api
                .verify_down(&address.clone().into(), proven_up.version, Some(proven_up))
                .await
            {
                Ok(()) => self.reconciled_spends.push(UtxoSpent {
                    id: output.commitment.into(),
                    version: proven_up.version,
                }),
                Err(SubstateApiError::SubstateNotDown { version, .. }) => {
                    debug!(
                        target: LOG_TARGET,
                        "Stealth output {address} was not returned by the resync of shard {shard}, but is live at v{version}. Keeping the output.",
                    );
                },
                Err(err) if is_refusal(&err) => {
                    warn!(
                        target: LOG_TARGET,
                        "⚠️ Stealth output {address} was not returned by the resync of shard {shard}, but the indexer did not prove it gone ({err}). Keeping the output.",
                    );
                },
                Err(err) => return Err(err.into()),
            }
        }
        Ok(())
    }

    /// Whether the wallet holds `held` proven up at a later version than `spent` takes down. A version is taken down by
    /// the change that brings up the next, so such a report is of a re-versioning (a freeze or an unfreeze), not of a
    /// spend of the output. A scan from an earlier cursor reports it again after the wallet holds the later version.
    fn is_superseded(held: &StealthOutputModel, spent: &UtxoSpent) -> bool {
        let is_superseded = held
            .proven_up
            .is_some_and(|proven_up| spent.version < proven_up.version);
        if is_superseded {
            debug!(
                target: LOG_TARGET,
                "Stealth output {} went down at v{}, and is proven up at a later version. Ignoring the spend.",
                spent.id,
                spent.version
            );
        }
        is_superseded
    }

    fn check_if_tag_matches(&self, unspent: &UtxoUnspent) -> Result<bool, StealthScannerApiError> {
        let Ok(public_nonce) = unspent.public_nonce.try_from_byte_type().inspect_err(|e| {
            warn!(
                target: LOG_TARGET,
                "⚠️ Received a malformed public nonce while syncing output: {}. Ignoring output.",
                e
            )
        }) else {
            return Ok(false);
        };

        let tag = self.sdk.stealth_crypto_api().derive_stealth_output_tag(
            self.network,
            &self.view_key.secret,
            &public_nonce,
            self.resource_address,
        );

        if tag != unspent.tag {
            trace!(
                target: LOG_TARGET,
                "Stealth output tag {} does not match (likely not our output)", unspent.tag
            );
            return Ok(false);
        }

        debug!(
            target: LOG_TARGET,
            "Stealth output tag {} matches. Will request full output.", unspent.tag
        );

        Ok(true)
    }

    fn spend(
        tx: &mut <TSpec::Store as WriteableWalletStore>::WriteTransaction<'_>,
        resource_address: &ResourceAddress,
        spent: &UtxoSpent,
    ) -> Result<bool, WalletStorageError> {
        let Some(output) = tx
            .stealth_outputs_get_by_commitment(resource_address, &spent.id.into_commitment_bytes())
            .optional()?
        else {
            debug!(
                target: LOG_TARGET,
                "Spent UTXO {} not found in wallet store (likely not our output)", spent.id
            );
            return Ok(false);
        };
        if output.status == OutputStatus::Spent {
            debug!(target: LOG_TARGET, "Stealth output {} is already spent", spent.id);
            return Ok(false);
        }
        if Self::is_superseded(&output, spent) {
            return Ok(false);
        }
        tx.stealth_outputs_mark_as_spent(resource_address, &spent.id, spent.version)?;
        info!(target: LOG_TARGET, "Marked stealth output {} as spent", spent.id);
        Ok(true)
    }
}

/// Whether an answer to [`SubstatesApi::verify_down`] refuses the proof for good, so asking again shows nothing more.
///
/// [`SubstatesApi::verify_down`]: tari_ootle_wallet_sdk::apis::substate::SubstatesApi::verify_down
fn is_refusal(err: &SubstateApiError) -> bool {
    matches!(
        err,
        SubstateApiError::SubstateDoesNotExist { .. } |
            SubstateApiError::SubstateNotDown { .. } |
            SubstateApiError::SubstateUnproven { .. } |
            SubstateApiError::SubstateUnverified { .. } |
            SubstateApiError::SubstateProvenBeforeAnchor { .. }
    )
}

fn shard_of(output: &StealthOutputModel) -> Shard {
    let id = SubstateId::Utxo(output.to_utxo_address());
    // A substate's shard is the same at every version.
    VersionedSubstateIdRef::new(&id, SubstateVersion::ZERO).to_shard(NUM_PRESHARDS)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ScanMode {
    /// Each shard is read from its cursor.
    Incremental,
    /// Each shard being resynced is read for its unspent outputs from where its resync reached.
    Resync,
}

/// A shard's rescan for its unspent outputs, from the start.
struct Resync {
    cursor: StateVersion,
    /// The tag and nonce of each of the wallet's outputs the rescan returned.
    returned: HashSet<(UtxoTag, RistrettoPublicKeyBytes)>,
}

/// How far a round may move the cursors it stores.
enum StoredCursorBound {
    /// Every cursor is stored as the indexer reports it.
    Unbounded,
    /// Each shard's cursor is stored no further than its state version in a verified end-of-epoch checkpoint, which
    /// the network committed. A shard without one keeps the cursor already stored. What the indexer reports past that
    /// is stored as a claim.
    Verified(HashMap<Shard, CheckpointStateVersion>),
}

impl StoredCursorBound {
    fn is_verified(&self) -> bool {
        matches!(self, Self::Verified(_))
    }

    fn ceiling(&self, shard: Shard) -> Option<StateVersion> {
        match self {
            Self::Unbounded => None,
            Self::Verified(versions) => versions.get(&shard).map(|checkpoint| checkpoint.state_version),
        }
    }

    /// The cursor to store for `shard` once the indexer reports it read to `version`, if any.
    fn bound(&self, shard: Shard, version: StateVersion) -> Option<StateVersion> {
        match self {
            Self::Unbounded => Some(version),
            Self::Verified(_) => self.ceiling(shard).map(|ceiling| version.min(ceiling)),
        }
    }

    /// The claim to begin for `shard` once the indexer reports it read to `version`, if `version` is past what a
    /// verified checkpoint commits for the shard.
    fn claim(&self, shard: Shard, version: StateVersion) -> Option<ClaimedStateVersion> {
        let Self::Verified(versions) = self else {
            return None;
        };
        let checkpoint = versions.get(&shard);
        if checkpoint.is_some_and(|checkpoint| version <= checkpoint.state_version) {
            return None;
        }
        Some(ClaimedStateVersion {
            state_version: version,
            first_state_version: version,
            verified_epoch: checkpoint.map(|checkpoint| checkpoint.epoch),
        })
    }

    /// The verified checkpoint of `shard` that ended after `claim` began, if any. For a claim that began without a
    /// verified checkpoint, that is the first one.
    fn checkpoint_after(&self, shard: Shard, claim: &ClaimedStateVersion) -> Option<&CheckpointStateVersion> {
        let Self::Verified(versions) = self else {
            return None;
        };
        versions.get(&shard).filter(|checkpoint| {
            claim
                .verified_epoch
                .is_none_or(|epoch| checkpoint.epoch.as_u64() >= epoch.as_u64().saturating_add(CLAIM_EXPIRY_EPOCHS))
        })
    }

    /// Whether `claim` has expired, and its shard is to be scanned again from the stored cursor.
    fn expires(&self, shard: Shard, claim: &ClaimedStateVersion) -> bool {
        self.checkpoint_after(shard, claim).is_some()
    }

    /// Whether a verified checkpoint that certainly ended after `claim` began commits a lower state version than the
    /// claim began at, which proves the indexer reported updates the network never committed.
    fn disproves(&self, shard: Shard, claim: &ClaimedStateVersion) -> bool {
        claim.verified_epoch.is_some() &&
            self.checkpoint_after(shard, claim)
                .is_some_and(|checkpoint| claim.first_state_version > checkpoint.state_version)
    }
}

#[derive(Debug, Clone, Default)]
pub struct UtxoScanRoundStats {
    /// Number of UTXO updates received from the network
    pub num_received: usize,
    /// Number of UTXOs that matched the tag and were queued for recovery
    pub num_potential_recoveries: usize,
    /// Number of UTXOs that were marked as spent
    pub num_spent: usize,
    /// Number of shards rescanned for their unspent outputs because the indexer may have lacked spends below their
    /// cursors
    pub num_resynced_shards: usize,
}

#[cfg(test)]
mod tests {
    use tari_ootle_common_types::Epoch;

    use super::*;

    const SHARD: Shard = Shard::from_u32(1);

    fn bound_at(epoch: u64, state_version: u64) -> StoredCursorBound {
        StoredCursorBound::Verified(HashMap::from([(SHARD, CheckpointStateVersion {
            epoch: Epoch(epoch),
            state_version: StateVersion::from(state_version),
        })]))
    }

    fn claim(first_state_version: u64, state_version: u64, verified_epoch: Option<u64>) -> ClaimedStateVersion {
        ClaimedStateVersion {
            state_version: StateVersion::from(state_version),
            first_state_version: StateVersion::from(first_state_version),
            verified_epoch: verified_epoch.map(Epoch),
        }
    }

    #[test]
    fn an_honest_claim_that_advanced_past_the_expiring_checkpoint_is_not_disproved() {
        let bound = bound_at(4, 100);
        let claim = claim(60, 130, Some(2));

        assert!(bound.expires(SHARD, &claim));
        assert!(!bound.disproves(SHARD, &claim));
    }

    #[test]
    fn a_claim_that_began_past_the_expiring_checkpoint_is_disproved() {
        let bound = bound_at(4, 100);

        assert!(bound.disproves(SHARD, &claim(101, 101, Some(2))));
    }

    #[test]
    fn a_claim_is_not_disproved_by_a_checkpoint_that_may_have_ended_before_it_began() {
        let bound = bound_at(3, 100);
        let claim = claim(101, 101, Some(2));

        assert!(!bound.expires(SHARD, &claim));
        assert!(!bound.disproves(SHARD, &claim));
    }

    #[test]
    fn a_claim_begun_without_a_verified_checkpoint_expires_without_being_disproved() {
        let bound = bound_at(4, 100);
        let claim = claim(101, 101, None);

        assert!(bound.expires(SHARD, &claim));
        assert!(!bound.disproves(SHARD, &claim));
    }
}
