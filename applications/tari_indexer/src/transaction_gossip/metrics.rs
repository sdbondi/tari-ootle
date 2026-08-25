//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::fmt;

use libp2p::gossipsub::MessageAcceptance;
use prometheus_client::{
    collector::Collector,
    encoding::{DescriptorEncoder, EncodeMetric},
    metrics::{
        counter::{ConstCounter, Counter},
        gauge::ConstGauge,
    },
    registry::Registry,
};
use tari_networking::GossipQueueSender;

use crate::metrics::CollectorRegister;

/// Counters for the transaction gossip service.
///
/// The verdict split is the diagnostic that matters: accepted, rejected and ignored together say
/// whether the indexer is a healthy mesh participant or is refusing everything it is sent, which a
/// received count alone cannot distinguish from a quiet network.
#[derive(Debug, Clone)]
pub struct TransactionGossipMetrics {
    messages_received: Counter,
    messages_accepted: Counter,
    messages_rejected: Counter,
    messages_ignored: Counter,
    transactions_stored: Counter,
    transactions_dropped: Counter,
}

impl TransactionGossipMetrics {
    pub fn register(registry: &mut Registry) -> Self {
        let registry = registry.sub_registry_with_prefix("transaction_gossip");
        Self {
            messages_received: Counter::default().register_at(
                "messages_received",
                "Messages taken from the transaction gossip queue",
                registry,
            ),
            messages_accepted: Counter::default().register_at(
                "messages_accepted",
                "Messages validated and propagated onward",
                registry,
            ),
            messages_rejected: Counter::default().register_at(
                "messages_rejected",
                "Messages withheld from the mesh and counted against the peer that sent them",
                registry,
            ),
            messages_ignored: Counter::default().register_at(
                "messages_ignored",
                "Messages withheld from the mesh without penalising the peer that sent them",
                registry,
            ),
            transactions_stored: Counter::default().register_at(
                "transactions_stored",
                "Gossiped transactions written to the store, excluding those already stored",
                registry,
            ),
            transactions_dropped: Counter::default().register_at(
                "transactions_dropped",
                "Validated transactions discarded because their batch failed to write",
                registry,
            ),
        }
    }

    pub fn on_message_received(&self) {
        self.messages_received.inc();
    }

    pub fn on_verdict(&self, acceptance: MessageAcceptance) {
        match acceptance {
            MessageAcceptance::Accept => &self.messages_accepted,
            MessageAcceptance::Reject => &self.messages_rejected,
            MessageAcceptance::Ignore => &self.messages_ignored,
        }
        .inc();
    }

    pub fn on_transactions_stored(&self, num_stored: u64) {
        self.transactions_stored.inc_by(num_stored);
    }

    pub fn on_batch_dropped(&self, num_transactions: u64) {
        self.transactions_dropped.inc_by(num_transactions);
    }
}

/// Reports the depth and budget of the inbound transaction gossip queue, read at scrape time so the
/// numbers are always exactly in sync with the queue itself.
///
/// Depth against budget is the number to watch when sizing `max_transaction_gossip_queue_bytes`: a
/// queue that never rises above a small fraction of its budget under load is over-provisioned, while
/// a non-zero drop count means gossip is being discarded and the budget — or the write rate behind
/// it — needs attention.
pub struct TransactionGossipQueueCollector {
    queue: GossipQueueSender,
}

impl TransactionGossipQueueCollector {
    pub fn new(queue: GossipQueueSender) -> Self {
        Self { queue }
    }

    /// Registers under the `transaction_gossip_queue` sub-registry, so the metric names on the wire
    /// are `transaction_gossip_queue_queued_bytes` and so on.
    pub fn register(self, registry: &mut Registry) {
        let registry = registry.sub_registry_with_prefix("transaction_gossip_queue");
        registry.register_collector(Box::new(self));
    }
}

impl Collector for TransactionGossipQueueCollector {
    fn encode(&self, mut encoder: DescriptorEncoder) -> Result<(), fmt::Error> {
        // Units are carried in the metric names rather than passed to the encoder, which would
        // append them a second time; counter names omit `_total`, which prometheus-client appends
        // itself. Both match the validator node's `InboundQueueCollector`, whose readings these
        // mirror — the same alert ported between the two must match on both.
        let gauges: [(&str, &str, u64); 3] = [
            (
                "queued_bytes",
                "Bytes currently held by messages awaiting processing",
                self.queue.queued_bytes() as u64,
            ),
            (
                "max_queued_bytes",
                "Byte budget this queue admits up to before dropping",
                self.queue.max_queued_bytes() as u64,
            ),
            (
                "queued_messages",
                "Messages currently awaiting processing",
                self.queue.queued_messages() as u64,
            ),
        ];

        for (name, help, value) in gauges {
            let gauge = ConstGauge::<u64>::new(value);
            let encoder = encoder.encode_descriptor(name, help, None, gauge.metric_type())?;
            gauge.encode(encoder)?;
        }

        // Monotonic totals: a gauge here would make `rate()` and `increase()` read nothing and hide
        // restarts, which for a drop count is the whole signal.
        let counters: [(&str, &str, u64); 2] = [
            (
                "dropped_messages",
                "Messages discarded because the queue was full",
                self.queue.dropped_messages(),
            ),
            (
                "dropped_bytes",
                "Bytes discarded because the queue was full",
                self.queue.dropped_bytes(),
            ),
        ];

        for (name, help, value) in counters {
            let counter = ConstCounter::<u64>::new(value);
            let encoder = encoder.encode_descriptor(name, help, None, counter.metric_type())?;
            counter.encode(encoder)?;
        }

        Ok(())
    }
}

impl fmt::Debug for TransactionGossipQueueCollector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransactionGossipQueueCollector")
            .finish_non_exhaustive()
    }
}
