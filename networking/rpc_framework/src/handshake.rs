//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{io, time::Duration};

use bytes::BytesMut;
use futures::{AsyncRead, AsyncWrite, SinkExt, StreamExt};
use prost::{DecodeError, Message};
use tokio::time;
use tracing::{Instrument, Level, debug, error, span, warn};

use crate::{error::HandshakeRejectReason, framing::CanonicalFraming, proto};

const LOG_TARGET: &str = "libp2p::rpc::handshake";

/// How long to spend reading a refusal a peer has already sent. The frame is buffered, so this is
/// generous for the case it exists for; the point of the bound is that a peer whose write failed for
/// some other reason and then says nothing cannot hold up connecting for the handshake timeout.
const REJECTION_READ_TIMEOUT: Duration = Duration::from_secs(1);

/// Supported RPC protocol versions.
/// Currently only v0 is supported
pub(super) const SUPPORTED_RPC_VERSIONS: &[u32] = &[0];

#[derive(Debug, thiserror::Error)]
pub enum RpcHandshakeError {
    #[error("Failed to decode message: {0}")]
    DecodeError(#[from] DecodeError),
    #[error("IO Error: {0}")]
    Io(#[from] io::Error),
    #[error("The client does not support any RPC protocol version supported by this node")]
    ClientNoSupportedVersion,
    #[error("Remote peer unexpectedly closed the RPC connection")]
    ServerClosedRequest,
    #[error("RPC handshake timed out")]
    TimedOut,
    #[error("RPC handshake was explicitly rejected: {0}")]
    Rejected(#[from] HandshakeRejectReason),
    #[error("The client connection is closed")]
    ClientClosed,
}

/// Handshake protocol
pub struct Handshake<'a, T> {
    framed: &'a mut CanonicalFraming<T>,
    timeout: Option<Duration>,
}

impl<'a, T> Handshake<'a, T>
where T: AsyncRead + AsyncWrite + Unpin
{
    /// Create a Handshake using the given framing and no timeout. To set a timeout, use `with_timeout`.
    pub fn new(framed: &'a mut CanonicalFraming<T>) -> Self {
        Self { framed, timeout: None }
    }

    /// Set the length of time that a client/server should wait for the other side to respond before timing out.
    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Server-side handshake protocol
    pub async fn perform_server_handshake(&mut self) -> Result<u32, RpcHandshakeError> {
        match self.recv_next_frame().await {
            Ok(Some(Ok(msg))) => {
                let msg = proto::RpcSession::decode(&mut msg.freeze())?;
                let version = SUPPORTED_RPC_VERSIONS
                    .iter()
                    .find(|v| msg.supported_versions.contains(v));
                if let Some(version) = version {
                    debug!(target: LOG_TARGET, "Server accepted version: {}", version);
                    // Acceptance is silent: a reply would cost a round trip on every session opened,
                    // and the first response says as much. Only a refusal is worth a frame.
                    return Ok(*version);
                }

                let span = span!(Level::INFO, "rpc::server::handshake::send_rejection");
                self.reject_with_reason(HandshakeRejectReason::UnsupportedVersion)
                    .instrument(span)
                    .await?;
                Err(RpcHandshakeError::ClientNoSupportedVersion)
            },
            Ok(Some(Err(err))) => {
                error!(target: LOG_TARGET, "Error during handshake: {}", err);
                Err(err.into())
            },
            Ok(None) => {
                error!(target: LOG_TARGET, "Error during handshake, client closed connection");
                Err(RpcHandshakeError::ClientClosed)
            },
            Err(_) => {
                error!(target: LOG_TARGET, "Error during handshake, timed out");
                Err(RpcHandshakeError::TimedOut)
            },
        }
    }

    pub async fn reject_with_reason(&mut self, reject_reason: HandshakeRejectReason) -> Result<(), RpcHandshakeError> {
        warn!(target: LOG_TARGET, "Rejecting handshake because {}", reject_reason);
        let reply = proto::RpcSessionReply {
            session_result: Some(proto::rpc_session_reply::SessionResult::Rejected(true)),
            reject_reason: reject_reason.as_i32(),
        };
        self.framed.send(reply.encode_to_vec().into()).await?;
        self.framed.close().await?;
        Ok(())
    }

    /// Client-side handshake protocol
    pub async fn perform_client_handshake(&mut self) -> Result<(), RpcHandshakeError> {
        let msg = proto::RpcSession {
            supported_versions: SUPPORTED_RPC_VERSIONS.to_vec(),
        };
        let payload = msg.encode_to_vec();
        debug!(target: LOG_TARGET, "Sending client handshake ({} bytes)", payload.len());
        // A server that refuses the session replies and closes without waiting to be spoken to, so
        // the write can fail against a peer that has already said why. Reading is only reached on
        // that failure: the accepted path never waits for a reply, and so never pays a round trip.
        let send_result = match self.framed.send(payload.into()).await {
            Ok(()) => self.framed.flush().await,
            Err(err) => Err(err),
        };
        if let Err(err) = send_result {
            warn!(
                target: LOG_TARGET,
                "IO error when sending new session handshake to peer: {}", err
            );
            return Err(self.read_rejection().await.unwrap_or_else(|| err.into()));
        }
        Ok(())
    }

    /// The refusal the peer has already sent, if it sent one. Whatever it wrote before closing is
    /// buffered, so this reads what is there under its own short bound rather than the handshake's.
    async fn read_rejection(&mut self) -> Option<RpcHandshakeError> {
        let frame = time::timeout(REJECTION_READ_TIMEOUT, self.framed.next())
            .await
            .ok()??
            .ok()?;
        decode_session_rejection(&frame).map(RpcHandshakeError::Rejected)
    }

    async fn recv_next_frame(&mut self) -> Result<Option<Result<BytesMut, io::Error>>, time::error::Elapsed> {
        match self.timeout {
            Some(timeout) => time::timeout(timeout, self.framed.next()).await,
            None => Ok(self.framed.next().await),
        }
    }
}

/// The reason a server refused a session, if `frame` is its refusal reply.
///
/// A refusal is a reply whose session result is `Rejected(true)`. A reply carrying no session
/// result at all is not one, and neither is any other frame that happens to decode as a reply.
pub(crate) fn decode_session_rejection(frame: &[u8]) -> Option<HandshakeRejectReason> {
    let reply = proto::RpcSessionReply::decode(frame).ok()?;
    match reply.session_result {
        Some(proto::rpc_session_reply::SessionResult::Rejected(true)) => Some(
            HandshakeRejectReason::from_i32(reply.reject_reason).unwrap_or(HandshakeRejectReason::Unknown(
                "server returned unrecognised rejection reason",
            )),
        ),
        _ => None,
    }
}
