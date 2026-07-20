//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashMap,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use libp2p::{PeerId, gossipsub};
use tokio::sync::mpsc;

use crate::message::MessageSpec;

pub const TOPIC_DELIMITER: &str = "-";

#[derive(Debug, thiserror::Error)]
pub enum GossipSendError {
    #[error("Invalid token topic: {0}")]
    InvalidToken(String),
    #[error("Inbound gossip channel closed")]
    InboundGossipChannelClosed,
    #[error(
        "Inbound gossip queue for topic {topic} is full ({queued_messages} messages, {queued_bytes} bytes queued); \
         dropped a {len} byte message"
    )]
    QueueFull {
        topic: String,
        queued_messages: usize,
        queued_bytes: usize,
        len: usize,
    },
}

/// An inbound gossipsub message, carrying the identifiers needed to report its validation verdict.
///
/// gossipsub runs in `validate_messages` mode, so a message is held and propagated to nobody until a
/// verdict is reported for it. A consumer MUST call
/// [`crate::NetworkingService::report_gossip_validation`] for every message it receives, or that
/// topic stops propagating across the network.
#[derive(Debug)]
pub struct GossipMessage {
    /// The peer that authored the message.
    pub source: PeerId,
    /// The peer we received it from, which may differ from `source`.
    pub propagation_source: PeerId,
    pub message_id: gossipsub::MessageId,
    pub message: gossipsub::Message,
    /// Holds this message's reservation against its topic queue's byte budget for as long as it is
    /// queued, releasing it when the message is dropped by the consumer. `None` until the message
    /// is admitted to a queue.
    _queue_permit: Option<QueuePermit>,
}

impl GossipMessage {
    pub fn new(
        source: PeerId,
        propagation_source: PeerId,
        message_id: gossipsub::MessageId,
        message: gossipsub::Message,
    ) -> Self {
        Self {
            source,
            propagation_source,
            message_id,
            message,
            _queue_permit: None,
        }
    }

    /// The pair identifying this message to [`crate::NetworkingService::report_gossip_validation`].
    pub fn validation_key(&self) -> (gossipsub::MessageId, PeerId) {
        (self.message_id.clone(), self.propagation_source)
    }

    fn with_queue_permit(mut self, permit: QueuePermit) -> Self {
        self._queue_permit = Some(permit);
        self
    }
}

/// Sender half of a bounded inbound gossip queue for one topic.
///
/// Bounds the queue by total size as well as message count. A count alone cannot do the job: every
/// topic admits messages up to the swarm's `gossip_sub_max_message_size`, so a count low enough to
/// bound worst-case memory is far too low for ordinary traffic, while a count high enough for
/// ordinary traffic admits that many maximum-size messages. Sizing by bytes is simultaneously
/// generous for real traffic (small messages, so many of them fit) and tight against a flood of
/// maximum-size ones.
///
/// Admission never blocks the networking worker: one worker task serves every topic, so waiting for
/// capacity on one topic would stall the others, consensus included. A message that does not fit is
/// rejected, and the worker reports `Ignore` for it — a full queue is this node's condition, not
/// misbehaviour by the peer that sent it, so it must not count against that peer's score.
#[derive(Debug, Clone)]
pub struct GossipQueueSender {
    tx: mpsc::Sender<GossipMessage>,
    queued_bytes: Arc<AtomicUsize>,
    max_queued_bytes: usize,
}

/// Creates a bounded inbound gossip queue for a single topic.
pub fn gossip_queue(
    max_queued_messages: usize,
    max_queued_bytes: usize,
) -> (GossipQueueSender, mpsc::Receiver<GossipMessage>) {
    let (tx, rx) = mpsc::channel(max_queued_messages);
    (
        GossipQueueSender {
            tx,
            queued_bytes: Arc::new(AtomicUsize::new(0)),
            max_queued_bytes,
        },
        rx,
    )
}

impl GossipQueueSender {
    /// Bytes currently held by queued messages.
    pub fn queued_bytes(&self) -> usize {
        self.queued_bytes.load(Ordering::Relaxed)
    }

    /// Messages currently queued.
    pub fn queued_messages(&self) -> usize {
        self.tx.max_capacity() - self.tx.capacity()
    }

    fn try_send(&self, msg: GossipMessage) -> Result<(), GossipSendError> {
        let len = msg.message.data.len();
        let full = |msg: &GossipMessage| GossipSendError::QueueFull {
            topic: msg.message.topic.to_string(),
            queued_messages: self.queued_messages(),
            queued_bytes: self.queued_bytes(),
            len,
        };

        let Some(permit) = self.reserve(len) else {
            return Err(full(&msg));
        };

        match self.tx.try_send(msg.with_queue_permit(permit)) {
            Ok(()) => Ok(()),
            // The rejected message carries its permit, so the reservation is released as it drops.
            Err(mpsc::error::TrySendError::Full(msg)) => Err(full(&msg)),
            Err(mpsc::error::TrySendError::Closed(_)) => Err(GossipSendError::InboundGossipChannelClosed),
        }
    }

    /// Reserves `len` bytes of this queue's budget, or `None` when that would exceed it. The
    /// reservation is released when the returned permit drops, i.e. once the consumer is done with
    /// the message it travels with.
    fn reserve(&self, len: usize) -> Option<QueuePermit> {
        let previous = self.queued_bytes.fetch_add(len, Ordering::AcqRel);
        if previous.saturating_add(len) > self.max_queued_bytes {
            self.queued_bytes.fetch_sub(len, Ordering::AcqRel);
            return None;
        }
        Some(QueuePermit {
            queued_bytes: self.queued_bytes.clone(),
            len,
        })
    }
}

/// Releases a queued message's share of its topic queue's byte budget when dropped.
#[derive(Debug)]
struct QueuePermit {
    queued_bytes: Arc<AtomicUsize>,
    len: usize,
}

impl Drop for QueuePermit {
    fn drop(&mut self) {
        self.queued_bytes.fetch_sub(self.len, Ordering::AcqRel);
    }
}

pub enum MessagingMode<TMsg: MessageSpec> {
    Enabled {
        tx_messages: mpsc::UnboundedSender<(PeerId, TMsg::Message)>,
        tx_gossip_messages_by_topic: HashMap<String, GossipQueueSender>,
    },
    Disabled,
}

impl<TMsg: MessageSpec> MessagingMode<TMsg> {
    pub fn is_enabled(&self) -> bool {
        matches!(self, MessagingMode::Enabled { .. })
    }
}

impl<TMsg: MessageSpec> MessagingMode<TMsg> {
    pub fn send_message(
        &self,
        peer_id: PeerId,
        msg: TMsg::Message,
    ) -> Result<(), mpsc::error::SendError<(PeerId, TMsg::Message)>> {
        if let MessagingMode::Enabled { tx_messages, .. } = self {
            tx_messages.send((peer_id, msg))?;
        }
        Ok(())
    }

    pub fn send_gossip_message(&self, msg: GossipMessage) -> Result<(), GossipSendError> {
        if let MessagingMode::Enabled {
            tx_gossip_messages_by_topic,
            ..
        } = self
        {
            // Topics may be a bare prefix (single global topic, e.g. "consensus") or a prefixed topic
            // (e.g. "transactions-0-15"). Route on the prefix before the first delimiter, falling back to the
            // whole topic when there is no delimiter.
            let queue = {
                let topic = msg.message.topic.as_str();
                let prefix = topic.split_once(TOPIC_DELIMITER).map_or(topic, |(prefix, _)| prefix);
                tx_gossip_messages_by_topic
                    .get(prefix)
                    .ok_or_else(|| GossipSendError::InvalidToken(topic.to_string()))?
            };
            queue.try_send(msg)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn message(len: usize) -> GossipMessage {
        GossipMessage::new(
            PeerId::random(),
            PeerId::random(),
            gossipsub::MessageId::new(b"id"),
            gossipsub::Message {
                source: None,
                data: vec![0u8; len],
                sequence_number: None,
                topic: gossipsub::IdentTopic::new("test").hash(),
            },
        )
    }

    #[test]
    fn admits_messages_until_the_byte_budget_is_reached() {
        let (tx, _rx) = gossip_queue(16, 100);

        tx.try_send(message(60)).unwrap();
        assert_eq!(tx.queued_bytes(), 60);

        let err = tx.try_send(message(60)).unwrap_err();
        assert!(matches!(err, GossipSendError::QueueFull { .. }));
        assert_eq!(tx.queued_bytes(), 60, "a rejected message must not hold budget");

        tx.try_send(message(40)).unwrap();
        assert_eq!(tx.queued_bytes(), 100, "a message that exactly fits is admitted");
    }

    #[test]
    fn budget_is_released_once_a_message_is_consumed() {
        let (tx, mut rx) = gossip_queue(16, 100);
        tx.try_send(message(100)).unwrap();

        let err = tx.try_send(message(1)).unwrap_err();
        assert!(matches!(err, GossipSendError::QueueFull { .. }));

        drop(rx.try_recv().unwrap());
        assert_eq!(tx.queued_bytes(), 0);
        tx.try_send(message(100)).unwrap();
    }

    #[test]
    fn message_count_is_bounded_independently_of_size() {
        // A flood of tiny messages carries per-message overhead the byte budget does not see.
        let (tx, _rx) = gossip_queue(2, 1024 * 1024);
        tx.try_send(message(1)).unwrap();
        tx.try_send(message(1)).unwrap();

        let err = tx.try_send(message(1)).unwrap_err();
        assert!(matches!(err, GossipSendError::QueueFull { .. }));
        assert_eq!(tx.queued_bytes(), 2, "the rejected message released its reservation");
    }
}
