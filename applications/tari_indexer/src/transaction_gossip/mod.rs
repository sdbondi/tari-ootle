//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

mod service;
pub use service::TransactionGossipService;

#[cfg(feature = "metrics")]
mod metrics;
#[cfg(feature = "metrics")]
pub use metrics::{TransactionGossipMetrics, TransactionGossipQueueCollector};
