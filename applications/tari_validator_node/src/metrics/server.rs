//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::net::SocketAddr;

use axum::Router;
use log::*;
use tari_shutdown::ShutdownSignal;
use tokio::{net::TcpListener, task::JoinHandle};

use super::handler::MetricsHandler;

const LOG_TARGET: &str = "tari::validator_node::metrics";

/// Spawn the metrics server.
pub async fn spawn_listener(
    address: SocketAddr,
    mut shutdown_signal: ShutdownSignal,
    registry: prometheus_client::registry::Registry,
) -> Result<JoinHandle<Result<(), anyhow::Error>>, anyhow::Error> {
    info!(target: LOG_TARGET, "🌐 Starting metrics server on {}", address);
    let router = Router::new().route("/_metrics", axum::routing::get(MetricsHandler::new(registry)));

    let listener = TcpListener::bind(address).await?;
    let server = axum::serve(listener, router).with_graceful_shutdown(async move {
        shutdown_signal.wait().await;
    });
    let addr = server.local_addr()?;
    info!(target: LOG_TARGET, "🌐 Metrics listening on {addr}");
    let handle = tokio::spawn(async move {
        server.await.map_err(anyhow::Error::from)?;
        info!(target: LOG_TARGET, "💤 server exited cleanly");
        Ok(())
    });

    Ok(handle)
}
