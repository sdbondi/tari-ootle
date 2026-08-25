//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{mem, time::Duration};

use libp2p::gossipsub::MessageAcceptance;
use log::*;
use tari_epoch_manager::{EpochManagerReader, service::EpochManagerHandle};
use tari_indexer_client::types::TransactionSource;
use tari_networking::{GossipMessage, NetworkingHandle, NetworkingService};
use tari_ootle_common_types::Epoch;
use tari_ootle_p2p::{
    GossipValidation,
    PeerAddress,
    TariMessage,
    TariMessagingSpec,
    TransactionGossipCodec,
    transaction_topic,
};
use tari_ootle_storage::StorageError;
use tari_ootle_transaction::Transaction;
use tari_ootle_transaction_validation::{TransactionValidationError, Validator};
use tari_shutdown::ShutdownSignal;
use tokio::{sync::mpsc, task, time};

use crate::store::{IndexerStore, IndexerStoreWriteTransaction};
#[cfg(feature = "metrics")]
use crate::transaction_gossip::metrics::TransactionGossipMetrics;

const LOG_TARGET: &str = "tari::indexer::transaction_gossip";

/// Transactions buffered before a flush is forced. Batching amortises SQLite's database-wide write
/// lock over a whole burst rather than taking it once per gossiped transaction, which at network
/// transaction rates would contend with state sync and the pruner for it continuously.
const BATCH_SIZE: usize = 100;

/// Longest a buffered transaction waits for the batch to fill before it is written anyway.
const FLUSH_INTERVAL: Duration = Duration::from_millis(500);

/// Stores transactions observed on the network-wide transaction gossip topic.
///
/// The indexer participates in the mesh as a full peer: it validates what it receives and reports a
/// verdict that propagates the transaction onward. Being in the mesh without forwarding would
/// occupy slots in its peers' meshes and black-hole propagation through them.
pub struct TransactionGossipService<TStore, TValidator> {
    store: TStore,
    epoch_manager: EpochManagerHandle<PeerAddress>,
    validator: TValidator,
    max_validity_epochs: u64,
    networking: NetworkingHandle<TariMessagingSpec>,
    rx_gossip: mpsc::Receiver<GossipMessage>,
    codec: TransactionGossipCodec,
    pending: Vec<Transaction>,
    #[cfg(feature = "metrics")]
    metrics: TransactionGossipMetrics,
}

impl<TStore, TValidator> TransactionGossipService<TStore, TValidator>
where
    TStore: IndexerStore + Clone,
    TValidator: Validator<Transaction, Context = Epoch, Error = TransactionValidationError> + Send + Sync + 'static,
{
    pub fn new(
        store: TStore,
        epoch_manager: EpochManagerHandle<PeerAddress>,
        validator: TValidator,
        max_validity_epochs: u64,
        networking: NetworkingHandle<TariMessagingSpec>,
        rx_gossip: mpsc::Receiver<GossipMessage>,
        #[cfg(feature = "metrics")] metrics: TransactionGossipMetrics,
    ) -> Self {
        Self {
            store,
            epoch_manager,
            validator,
            max_validity_epochs,
            networking,
            rx_gossip,
            codec: TransactionGossipCodec::new(),
            pending: Vec::with_capacity(BATCH_SIZE),
            #[cfg(feature = "metrics")]
            metrics,
        }
    }

    pub fn spawn(mut self, mut shutdown: ShutdownSignal) -> task::JoinHandle<()> {
        task::spawn(async move {
            if let Err(err) = self.run(&mut shutdown).await {
                error!(target: LOG_TARGET, "💥 Transaction gossip service exited: {err}");
            }
        })
    }

    async fn run(&mut self, shutdown: &mut ShutdownSignal) -> Result<(), anyhow::Error> {
        // Validation is epoch-dependent, and the epoch manager reports zero until its initial scan
        // completes. Subscribing before then would measure every transaction against an epoch the
        // network has long passed, refusing valid transactions and withholding them from the mesh.
        tokio::select! {
            _ = shutdown.wait() => return Ok(()),
            result = self.epoch_manager.wait_for_initial_scanning_to_complete() => result?,
        }

        self.networking.subscribe_topic(transaction_topic()).await?;
        info!(target: LOG_TARGET, "📥 Subscribed to transaction gossip on '{}'", transaction_topic());

        let mut flush_interval = time::interval(FLUSH_INTERVAL);
        flush_interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
        // The first tick of an interval resolves immediately. Taking it here means the first
        // buffered transaction gets a full flush window rather than being written on its own.
        flush_interval.tick().await;

        loop {
            tokio::select! {
                _ = shutdown.wait() => {
                    info!(target: LOG_TARGET, "📥 Transaction gossip service was shutdown.");
                    self.flush().await;
                    break;
                },
                maybe_msg = self.rx_gossip.recv() => {
                    let Some(msg) = maybe_msg else {
                        info!(target: LOG_TARGET, "📥 Transaction gossip channel closed.");
                        self.flush().await;
                        break;
                    };
                    self.handle_message(msg).await;
                    if self.pending.len() >= BATCH_SIZE {
                        self.flush().await;
                        flush_interval.reset();
                    }
                },
                _ = flush_interval.tick(), if !self.pending.is_empty() => {
                    self.flush().await;
                    flush_interval.reset();
                },
            }
        }

        Ok(())
    }

    async fn handle_message(&mut self, msg: GossipMessage) {
        let validation = GossipValidation::new(msg.validation_key());
        #[cfg(feature = "metrics")]
        self.metrics.on_message_received();

        let from = msg.source;
        let transaction = match self.codec.decode(msg.message).await {
            Ok((_, TariMessage::NewTransaction(msg))) => msg.transaction,
            Err(err) => {
                debug!(target: LOG_TARGET, "Undecodable gossip message from {from}: {err}");
                // Undecodable: withhold from the mesh and count it against the peer that sent it.
                self.report(validation, MessageAcceptance::Reject).await;
                return;
            },
        };

        let current_epoch = self.epoch_manager.get_current_epoch();
        let result = self.validator.validate(&current_epoch, &transaction);

        // Reported before the transaction reaches the batch, never after: gossipsub holds a message
        // in its validation cache for only a few heartbeats, so waiting out a flush interval would
        // stall propagation through this node. A write failure is this node's problem and must not
        // enter into the verdict at all.
        let acceptance = match &result {
            Ok(_) => MessageAcceptance::Accept,
            // Only a failure the sender is responsible for counts against their peer score. A
            // transaction refused because of this indexer's own lagging epoch view is withheld
            // without penalty.
            Err(err) if err.is_sender_fault() => MessageAcceptance::Reject,
            Err(_) => MessageAcceptance::Ignore,
        };
        self.report(validation, acceptance).await;

        if let Err(err) = result {
            debug!(
                target: LOG_TARGET,
                "Rejecting gossiped transaction {}: {err}",
                transaction.calculate_id(),
            );
            return;
        }

        self.pending.push(transaction);
    }

    /// Writes the buffered transactions in a single store write transaction. Duplicates — a
    /// transaction already submitted here, or one gossiped to us twice — are ignored by the store.
    async fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }

        let batch = mem::take(&mut self.pending);
        let num_transactions = batch.len();
        let retention_ceiling = self.retention_ceiling();
        // The write runs on a blocking thread that outlives this future, so the closure has to own
        // the batch rather than borrow it. Handing the buffer back on the way out keeps its
        // allocation across flushes.
        let result: Result<_, StorageError> = self
            .store
            .with_write_tx(move |tx| {
                let num_inserted =
                    tx.insert_batch_transactions(batch.iter(), TransactionSource::Gossip, retention_ceiling)?;
                Ok((num_inserted, batch))
            })
            .await;

        match result {
            Ok((num_inserted, mut batch)) => {
                batch.clear();
                self.pending = batch;
                info!(
                    target: LOG_TARGET,
                    "📥 Stored {num_inserted} of {num_transactions} gossiped transaction(s)",
                );
                #[cfg(feature = "metrics")]
                self.metrics.on_transactions_stored(num_inserted as u64);
            },
            Err(err) => {
                warn!(
                    target: LOG_TARGET,
                    "⚠️ Failed to store {num_transactions} gossiped transaction(s): {err}",
                );
                #[cfg(feature = "metrics")]
                self.metrics.on_batch_dropped(num_transactions as u64);
                self.pending = Vec::with_capacity(BATCH_SIZE);
            },
        }
    }

    /// The furthest out a retention key can honestly sit: the last epoch a transaction admitted now
    /// could still be sequenced in.
    fn retention_ceiling(&self) -> Epoch {
        Epoch(
            self.epoch_manager
                .get_current_epoch()
                .as_u64()
                .saturating_add(self.max_validity_epochs),
        )
    }

    /// gossipsub withholds a message from the rest of the mesh until its verdict is reported, so
    /// every message taken from the queue must be reported exactly once.
    async fn report(&mut self, validation: GossipValidation, acceptance: MessageAcceptance) {
        #[cfg(feature = "metrics")]
        self.metrics.on_verdict(acceptance);

        let (message_id, propagation_source) = validation.into_key();
        if let Err(err) = self
            .networking
            .report_gossip_validation(message_id, propagation_source, acceptance)
            .await
        {
            warn!(target: LOG_TARGET, "Failed to report gossip validation result: {err}");
        }
    }
}
