//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use futures::SinkExt;
use prost::Message;
use tokio::task;
use tokio_util::compat::TokioAsyncReadCompatExt;

use crate::{
    Handshake,
    error::HandshakeRejectReason,
    framing,
    handshake::{RpcHandshakeError, SUPPORTED_RPC_VERSIONS},
    proto,
};

const FRAME_SIZE: usize = 1024;

#[tokio::test]
async fn it_performs_the_handshake() {
    let (client, server) = tokio::io::duplex(FRAME_SIZE);

    let handshake_result = task::spawn(async move {
        let mut server_framed = framing::canonical(server.compat(), FRAME_SIZE);
        let mut handshake_server = Handshake::new(&mut server_framed);
        handshake_server.perform_server_handshake().await
    });

    let mut client_framed = framing::canonical(client.compat(), FRAME_SIZE);
    let mut handshake_client = Handshake::new(&mut client_framed);

    handshake_client.perform_client_handshake().await.unwrap();
    let v = handshake_result.await.unwrap().unwrap();
    assert!(SUPPORTED_RPC_VERSIONS.contains(&v));
}

#[tokio::test]
async fn the_client_does_not_wait_for_the_server_to_accept() {
    let (client, server) = tokio::io::duplex(FRAME_SIZE);
    // Nothing ever reads or answers the client's side.
    let _server = server;

    let mut client_framed = framing::canonical(client.compat(), FRAME_SIZE);
    let mut handshake_client = Handshake::new(&mut client_framed);

    // Completes on the send alone. A reply would cost a round trip on every session opened.
    handshake_client.perform_client_handshake().await.unwrap();
}

#[tokio::test]
async fn a_frame_that_is_not_a_refusal_is_not_read_as_one() {
    let (client, server) = tokio::io::duplex(FRAME_SIZE);

    // A reply-shaped frame carrying no session result. Reporting this as a refusal would put an
    // invented reason in front of whatever really went wrong.
    let mut server_framed = framing::canonical(server.compat(), FRAME_SIZE);
    server_framed
        .send(
            proto::RpcSessionReply {
                session_result: None,
                reject_reason: 0,
            }
            .encode_to_vec()
            .into(),
        )
        .await
        .unwrap();
    server_framed.close().await.unwrap();
    drop(server_framed);

    let mut client_framed = framing::canonical(client.compat(), FRAME_SIZE);
    let mut handshake_client = Handshake::new(&mut client_framed);

    let err = handshake_client.perform_client_handshake().await.unwrap_err();
    assert!(
        matches!(err, RpcHandshakeError::Io(_)),
        "expected the write's own error, got {err:?}"
    );
}

#[tokio::test]
async fn a_rejection_is_reported_when_the_peer_has_already_closed() {
    let (client, server) = tokio::io::duplex(FRAME_SIZE);

    let mut server_framed = framing::canonical(server.compat(), FRAME_SIZE);
    let mut handshake_server = Handshake::new(&mut server_framed);
    handshake_server
        .reject_with_reason(HandshakeRejectReason::NoSessionsAvailable)
        .await
        .unwrap();
    drop(server_framed);

    let mut client_framed = framing::canonical(client.compat(), FRAME_SIZE);
    let mut handshake_client = Handshake::new(&mut client_framed);

    // The send fails against the closed peer, so the reason it already sent is read rather than
    // reported as a bare IO error.
    let err = handshake_client.perform_client_handshake().await.unwrap_err();
    assert!(
        matches!(
            err,
            RpcHandshakeError::Rejected(HandshakeRejectReason::NoSessionsAvailable)
        ),
        "unexpected error {err:?}"
    );
}
