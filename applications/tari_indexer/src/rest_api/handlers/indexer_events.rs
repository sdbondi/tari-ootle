//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use axum::{
    Extension,
    response::{IntoResponse, Response, Sse, sse},
};
use log::info;
use tari_indexer_client::event::IndexerEvent;
use tokio_stream::StreamExt;

use crate::rest_api::{context::HandlerContext, handlers::HandlerResult, streaming::disable_proxy_buffering};

const LOG_TARGET: &str = "tari::indexer::rest_api::handlers::events";

#[utoipa::path(get, path = "/events", description = "SSE events")]
pub async fn sse_events(Extension(context): Extension<HandlerContext>) -> HandlerResult<Response> {
    info!(target: LOG_TARGET, "Client connected to SSE event stream");
    let event_stream = tokio_stream::wrappers::BroadcastStream::new(context.subscribe_events())
        .take_while(|res| res.is_ok())
        .map(|res| res.expect("take_while should prevent errors here"))
        .map(|event| encode_event(&event));

    let mut response = Sse::new(event_stream).keep_alive(sse::KeepAlive::new()).into_response();
    disable_proxy_buffering(response.headers_mut());
    Ok(response)
}

fn encode_event(event: &IndexerEvent) -> Result<sse::Event, axum::Error> {
    let encoded = sse::Event::default().event(event.as_event_name());
    match event {
        IndexerEvent::NewEpoch(event) => encoded.json_data(event),
        IndexerEvent::TransactionFinalized(event) => encoded.json_data(event),
    }
}
