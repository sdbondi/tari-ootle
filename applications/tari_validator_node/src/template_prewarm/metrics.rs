//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use prometheus_client::{
    metrics::{counter::Counter, gauge::Gauge},
    registry::Registry,
};

use crate::metrics::CollectorRegister;

/// Counters are shared by every clone, so a worker and the handle that queued its work report to the
/// same series.
#[derive(Debug, Clone)]
pub struct PrometheusPrewarmMetrics {
    enqueued: Counter,
    dropped: Counter,
    compiled: Counter,
    already_resident: Counter,
    failed: Counter,
    queue_depth: Gauge,
}

impl PrometheusPrewarmMetrics {
    pub fn new(registry: &mut Registry) -> Self {
        let registry = registry.sub_registry_with_prefix("template_prewarm");
        Self {
            enqueued: Counter::default().register_at(
                "enqueued",
                "Number of templates queued for background compilation",
                registry,
            ),
            dropped: Counter::default().register_at(
                "dropped",
                "Number of prewarm requests dropped because the queue was full",
                registry,
            ),
            compiled: Counter::default().register_at(
                "compiled",
                "Number of templates compiled by the prewarm pool",
                registry,
            ),
            already_resident: Counter::default().register_at(
                "already_resident",
                "Number of prewarm requests whose template was resident by the time a worker reached it, most often \
                 because execution compiled it first",
                registry,
            ),
            failed: Counter::default().register_at(
                "failed",
                "Number of prewarm requests whose template could not be loaded",
                registry,
            ),
            queue_depth: Gauge::default().register_at(
                "queue_depth",
                "Targets queued for background compilation, including the one each worker is compiling",
                registry,
            ),
        }
    }

    pub fn on_enqueued(&self, queue_depth: usize) {
        self.enqueued.inc();
        self.set_queue_depth(queue_depth);
    }

    pub fn on_finished(&self, queue_depth: usize) {
        self.set_queue_depth(queue_depth);
    }

    pub fn on_dropped(&self) {
        self.dropped.inc();
    }

    pub fn on_compiled(&self) {
        self.compiled.inc();
    }

    pub fn on_already_resident(&self) {
        self.already_resident.inc();
    }

    pub fn on_failed(&self) {
        self.failed.inc();
    }

    fn set_queue_depth(&self, depth: usize) {
        self.queue_depth.set(i64::try_from(depth).unwrap_or(i64::MAX));
    }
}
