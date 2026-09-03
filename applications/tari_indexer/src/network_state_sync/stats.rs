//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    fmt::Display,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

/// Counters shared by every shard-group stream, reported and reset together.
#[derive(Debug, Clone, Default)]
pub struct SyncStats {
    inner: Arc<Counters>,
}

#[derive(Debug, Default)]
struct Counters {
    checkpoints: AtomicUsize,
    state_updates: AtomicUsize,
    events: AtomicUsize,
}

impl SyncStats {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn increment_checkpoints(&self) {
        self.inner.checkpoints.fetch_add(1, Ordering::Relaxed);
    }

    pub fn increase_state_updates(&self, by: usize) {
        self.inner.state_updates.fetch_add(by, Ordering::Relaxed);
    }

    pub fn increase_events(&self, by: usize) {
        self.inner.events.fetch_add(by, Ordering::Relaxed);
    }

    pub fn log_stats(&self) {
        log::info!("{}", self);
    }

    pub fn reset(&self) {
        self.inner.checkpoints.store(0, Ordering::Relaxed);
        self.inner.state_updates.store(0, Ordering::Relaxed);
        self.inner.events.store(0, Ordering::Relaxed);
    }
}

impl Display for SyncStats {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "Sync Stats: {{ checkpoints: {}, state_updates: {}, events: {} }}",
            self.inner.checkpoints.load(Ordering::Relaxed),
            self.inner.state_updates.load(Ordering::Relaxed),
            self.inner.events.load(Ordering::Relaxed)
        )
    }
}
