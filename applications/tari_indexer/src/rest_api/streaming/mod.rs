//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

pub mod encoding;
mod utxo_stream;

use axum::http::{HeaderMap, HeaderName, HeaderValue};
pub use utxo_stream::*;

const X_ACCEL_BUFFERING: HeaderName = HeaderName::from_static("x-accel-buffering");

/// Opts a response out of proxy response buffering.
///
/// nginx and the proxies that follow its conventions buffer an upstream response by default, so a
/// stream's frames are held back until the buffer fills or the response ends — on a long-lived
/// stream, indefinitely. Every streaming endpoint sets this so its frames reach the client as they
/// are produced rather than in bursts, or never.
pub fn disable_proxy_buffering(headers: &mut HeaderMap) {
    headers.insert(X_ACCEL_BUFFERING, HeaderValue::from_static("no"));
}
