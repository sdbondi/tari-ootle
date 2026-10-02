//   Copyright 2024 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::future::Future;

use libp2p::PeerId;
use tari_consensus::{messages::HotstuffMessage, traits::InboundMessagingError};
use tari_epoch_manager::{EpochManagerReader, service::EpochManagerHandle};
use tari_networking::InboundMessage;
use tari_ootle_common_types::Epoch;
use tari_ootle_p2p::{PeerAddress, proto};
use tokio::sync::mpsc;

use crate::p2p::logging::MessageLogger;

/// Answers whether a peer is a validator registered for an epoch.
pub trait ValidatorRegistry {
    fn is_registered(&self, epoch: Epoch, address: &PeerAddress) -> impl Future<Output = bool> + Send;
}

impl ValidatorRegistry for EpochManagerHandle<PeerAddress> {
    async fn is_registered(&self, epoch: Epoch, address: &PeerAddress) -> bool {
        self.get_committee_info_by_validator_address(epoch, address)
            .await
            .is_ok()
    }
}

pub struct ConsensusInboundMessaging<TMsgLogger, TRegistry = EpochManagerHandle<PeerAddress>> {
    local_address: PeerAddress,
    rx_inbound_msg: mpsc::Receiver<InboundMessage<proto::consensus::HotStuffMessage>>,
    rx_gossip: mpsc::Receiver<(PeerId, HotstuffMessage)>,
    rx_loopback: mpsc::UnboundedReceiver<HotstuffMessage>,
    msg_logger: TMsgLogger,
    validators: TRegistry,
}

impl<TMsgLogger: MessageLogger, TRegistry: ValidatorRegistry> ConsensusInboundMessaging<TMsgLogger, TRegistry> {
    pub fn new(
        local_address: PeerAddress,
        rx_inbound_msg: mpsc::Receiver<InboundMessage<proto::consensus::HotStuffMessage>>,
        rx_gossip: mpsc::Receiver<(PeerId, HotstuffMessage)>,
        rx_loopback: mpsc::UnboundedReceiver<HotstuffMessage>,
        msg_logger: TMsgLogger,
        validators: TRegistry,
    ) -> Self {
        Self {
            local_address,
            rx_inbound_msg,
            rx_gossip,
            rx_loopback,
            msg_logger,
            validators,
        }
    }

    fn handle_message(
        &self,
        from: PeerId,
        msg: proto::consensus::HotStuffMessage,
    ) -> Option<Result<(PeerAddress, HotstuffMessage), InboundMessagingError>> {
        match HotstuffMessage::try_from(msg) {
            Ok(msg) => {
                self.msg_logger
                    .log_inbound_message(&from.to_string(), msg.as_type_str(), "", &msg);
                Some(Ok((from.into(), msg)))
            },
            Err(err) => Some(Err(InboundMessagingError::InvalidMessage {
                reason: format!("from peer {from}: {err}"),
            })),
        }
    }
}

impl<TMsgLogger, TRegistry> tari_consensus::traits::InboundMessaging
    for ConsensusInboundMessaging<TMsgLogger, TRegistry>
where
    TMsgLogger: MessageLogger + Send,
    TRegistry: ValidatorRegistry + Send + Sync,
{
    type Addr = PeerAddress;

    async fn next_message(&mut self) -> Option<Result<(Self::Addr, HotstuffMessage), InboundMessagingError>> {
        tokio::select! {
            // BIASED: messaging priority is loopback, then other
            biased;
            maybe_msg = self.rx_loopback.recv() => maybe_msg.map(|msg| {
                self.msg_logger.log_inbound_message(
                   &self.local_address.to_string(),
                   msg.as_type_str(),
                   "",
                   &msg,
                );
                Ok((self.local_address, msg))
            }),
            maybe_msg = self.rx_inbound_msg.recv() => {
                let inbound = maybe_msg?;
                if let Err(err) = check_sender(&self.validators, inbound.peer_id, &inbound.message).await {
                    return Some(Err(err));
                }
                self.handle_message(inbound.peer_id, inbound.message)
            },
            maybe_msg = self.rx_gossip.recv() => {
                let (from, msg) = maybe_msg?;
                self.msg_logger
                    .log_inbound_message(&from.to_string(), msg.as_type_str(), "", &msg);
                Some(Ok((from.into(), msg)))
            },
        }
    }
}

/// Refuses a direct message whose kind only a validator sends, from a peer that is not one, before its payload is
/// decoded. A transactions response is only ever requested from a validator, and its transactions are costly to
/// decode.
async fn check_sender<TRegistry: ValidatorRegistry>(
    validators: &TRegistry,
    from: PeerId,
    msg: &proto::consensus::HotStuffMessage,
) -> Result<(), InboundMessagingError> {
    if let Some(proto::consensus::hot_stuff_message::Message::RequestedTransaction(response)) = &msg.message &&
        !validators.is_registered(Epoch(response.epoch), &from.into()).await
    {
        return Err(InboundMessagingError::InvalidMessage {
            reason: format!(
                "peer {from} sent transactions for epoch {} but is not a validator registered for it",
                response.epoch
            ),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use tari_ootle_p2p::proto::{
        consensus::{HotStuffMessage, MissingTransactionsResponse, hot_stuff_message::Message},
        transaction::Transaction,
    };

    use super::*;

    struct Registered(HashSet<PeerAddress>);

    impl ValidatorRegistry for Registered {
        async fn is_registered(&self, _epoch: Epoch, address: &PeerAddress) -> bool {
            self.0.contains(address)
        }
    }

    fn transactions_response() -> HotStuffMessage {
        HotStuffMessage {
            message: Some(Message::RequestedTransaction(MissingTransactionsResponse {
                request_id: 1,
                epoch: 1,
                block_id: vec![1; 32],
                transactions: vec![Transaction {
                    bor_encoded: vec![0xff; 16],
                }],
            })),
        }
    }

    #[tokio::test]
    async fn a_transactions_response_from_an_unregistered_peer_is_refused() {
        let validator = PeerId::random();
        let registry = Registered(HashSet::from([validator.into()]));

        let err = check_sender(&registry, PeerId::random(), &transactions_response())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("not a validator"), "{err}");
    }

    #[tokio::test]
    async fn a_transactions_response_from_a_registered_validator_is_admitted() {
        let validator = PeerId::random();
        let registry = Registered(HashSet::from([validator.into()]));

        check_sender(&registry, validator, &transactions_response())
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn other_messages_are_admitted_from_any_peer() {
        let registry = Registered(HashSet::new());
        let request = HotStuffMessage {
            message: Some(Message::RequestMissingTransactions(Default::default())),
        };

        check_sender(&registry, PeerId::random(), &request).await.unwrap();
    }
}
