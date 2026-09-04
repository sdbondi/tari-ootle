//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{collections::HashSet, sync::Arc, time::Duration};

use tari_template_lib_types::TemplateAddress;

use crate::network_state_sync::EventFilter;

#[derive(Debug, Clone)]
pub struct NetworkWideStateSyncConfig {
    /// How long a shard group waits before reopening its stream after a failure or a final marker,
    /// and how often sync statistics are reported.
    pub work_interval: Duration,
    /// The deadline asked of a validator for a state sync stream. See
    /// `IndexerConfig::state_sync_stream_deadline`.
    pub stream_deadline: Duration,
    /// The keepalive interval asked of a validator for a state sync stream. See
    /// `IndexerConfig::state_sync_keepalive_interval`.
    pub keepalive_interval: Duration,
    pub event_filters: Arc<[EventFilter]>,
    pub watched_templates: Arc<HashSet<TemplateAddress>>,
}

impl Default for NetworkWideStateSyncConfig {
    fn default() -> Self {
        Self {
            work_interval: Duration::from_secs(30),
            stream_deadline: Duration::from_secs(600),
            keepalive_interval: Duration::from_secs(10),
            event_filters: Arc::new([]),
            watched_templates: Arc::new(HashSet::new()),
        }
    }
}
