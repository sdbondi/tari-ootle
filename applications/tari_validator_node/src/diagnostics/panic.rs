//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::sync::{
    Mutex,
    PoisonError,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use tari_ootle_common_types::diag_event;
use tari_ootle_storage::DiagnosticEventStore;

type Recorder = Box<dyn Fn(&str, &str) + Send + Sync>;

/// The installed recorder, tagged with the id of the guard that owns it.
static PANIC_RECORDER: Mutex<Option<(u64, Recorder)>> = Mutex::new(None);
static NEXT_RECORDER_ID: AtomicU64 = AtomicU64::new(0);
static RECORDING: AtomicBool = AtomicBool::new(false);

/// Lets the process-wide panic hook write a `node.panic` event.
///
/// The write goes straight to the store rather than through [`DiagnosticsHandle`], because by the
/// time a panic reaches the hook the writer task may already be gone.
///
/// The recorder owns `store` until the returned guard is dropped, so the guard must not outlive the
/// node: the store stays open, and its database locked, for as long as the recorder holds it. A
/// later install replaces the recorder, and dropping an earlier guard then leaves it in place.
///
/// [`DiagnosticsHandle`]: super::DiagnosticsHandle
#[must_use = "dropping the guard uninstalls the recorder"]
pub fn install_panic_recorder<TStore>(store: TStore) -> PanicRecorderGuard
where TStore: DiagnosticEventStore + Send + Sync + 'static {
    let id = NEXT_RECORDER_ID.fetch_add(1, Ordering::Relaxed);
    let recorder: Recorder = Box::new(move |location, message| {
        let event = diag_event!(error, "node.panic", "Panicked at {location}: {message}",
            location => location,
            message => message,
            thread => std::thread::current().name().unwrap_or("<unnamed>")
        );
        let _ignore = store.diagnostic_events_append(&[event]);
    });
    *PANIC_RECORDER.lock().unwrap_or_else(PoisonError::into_inner) = Some((id, recorder));
    PanicRecorderGuard { id }
}

/// Uninstalls the panic recorder, releasing its store, when dropped.
#[derive(Debug)]
pub struct PanicRecorderGuard {
    id: u64,
}

impl Drop for PanicRecorderGuard {
    fn drop(&mut self) {
        let removed = {
            let mut slot = PANIC_RECORDER.lock().unwrap_or_else(PoisonError::into_inner);
            if slot.as_ref().is_some_and(|(id, _)| *id == self.id) {
                slot.take()
            } else {
                None
            }
        };
        // Dropped outside the lock: this is what closes the store.
        drop(removed);
    }
}

/// Records a panic, if a recorder has been installed.
///
/// Only the first panic is recorded: a panic raised from inside the store while recording would
/// re-enter here and could deadlock on the same lock it panicked holding.
pub fn record_panic(location: &str, message: &str) {
    let Ok(slot) = PANIC_RECORDER.try_lock() else {
        return;
    };
    let Some((_, recorder)) = slot.as_ref() else {
        return;
    };
    if RECORDING.swap(true, Ordering::SeqCst) {
        return;
    }
    recorder(location, message);
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use tari_ootle_common_types::diagnostics::{DiagnosticEvent, DiagnosticEventFilter, DiagnosticEventRecord};
    use tari_ootle_storage::{DiagnosticEventPage, DiagnosticPruneStats, DiagnosticRetention, StorageError};

    use super::*;

    #[derive(Clone, Default)]
    struct NullStore(Arc<()>);

    impl DiagnosticEventStore for NullStore {
        fn diagnostic_events_append(&self, _: &[DiagnosticEvent]) -> Result<Option<u64>, StorageError> {
            Ok(None)
        }

        fn diagnostic_events_query(&self, _: &DiagnosticEventPage) -> Result<Vec<DiagnosticEventRecord>, StorageError> {
            Ok(vec![])
        }

        fn diagnostic_events_clear(&self, _: &DiagnosticEventFilter) -> Result<usize, StorageError> {
            Ok(0)
        }

        fn diagnostic_events_prune(&self, _: DiagnosticRetention) -> Result<DiagnosticPruneStats, StorageError> {
            Ok(DiagnosticPruneStats::default())
        }

        fn diagnostic_events_bounds(&self) -> Result<Option<(u64, u64)>, StorageError> {
            Ok(None)
        }
    }

    // Both cases share the process-wide slot, so they run in one test.
    #[test]
    fn guard_releases_the_store_it_installed() {
        let first = NullStore::default();
        let first_guard = install_panic_recorder(first.clone());
        assert_eq!(Arc::strong_count(&first.0), 2);
        drop(first_guard);
        assert_eq!(Arc::strong_count(&first.0), 1);

        let older = NullStore::default();
        let newer = NullStore::default();
        let older_guard = install_panic_recorder(older.clone());
        let newer_guard = install_panic_recorder(newer.clone());
        assert_eq!(Arc::strong_count(&older.0), 1);
        drop(older_guard);
        assert_eq!(Arc::strong_count(&newer.0), 2);
        drop(newer_guard);
        assert_eq!(Arc::strong_count(&newer.0), 1);
    }
}
