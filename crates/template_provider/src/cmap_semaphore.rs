//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    fmt::{Debug, Formatter},
    hash::Hash,
    sync::{Arc, Mutex, MutexGuard, PoisonError},
};

#[derive(Clone)]
pub struct ConcurrentMapSemaphore<K: Hash + Eq> {
    map: Arc<dashmap::DashMap<K, Arc<Mutex<()>>>>,
    global: Arc<std_semaphore::Semaphore>,
}

impl<K: Hash + Eq + Clone> ConcurrentMapSemaphore<K> {
    pub fn new(max_global_access: isize) -> Self {
        Self {
            map: Arc::new(dashmap::DashMap::new()),
            global: Arc::new(std_semaphore::Semaphore::new(max_global_access)),
        }
    }

    pub fn acquire(&self, key: K) -> ConcurrentMapSemaphoreGuard<'_, K> {
        let global_access = self.global.access();
        let map_mutex = self
            .map
            .entry(key.clone())
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone();
        ConcurrentMapSemaphoreGuard {
            _global_access: global_access,
            map: self.map.clone(),
            map_mutex,
            key,
        }
    }
}

impl<K: Hash + Eq> Debug for ConcurrentMapSemaphore<K> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CMapSemaphore")
            .field("map", &self.map.len())
            .field("global", &"...")
            .finish()
    }
}

pub struct ConcurrentMapSemaphoreGuard<'a, K: Hash + Eq> {
    /// The RAII handle to the global semaphore, which must be held for the duration of this guard
    _global_access: std_semaphore::SemaphoreGuard<'a>,
    map: Arc<dashmap::DashMap<K, Arc<Mutex<()>>>>,
    map_mutex: Arc<Mutex<()>>,
    key: K,
}

impl<K: Hash + Eq> ConcurrentMapSemaphoreGuard<'_, K> {
    pub fn access(&self) -> MutexGuard<'_, ()> {
        // The mutex guards `()`, so a panic under it leaves nothing half-written and the next
        // caller can take it. Propagating the poison instead would turn one panicking load into a
        // panic for every later caller of that key.
        self.map_mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl<K: Hash + Eq> Drop for ConcurrentMapSemaphoreGuard<'_, K> {
    fn drop(&mut self) {
        // The entry must outlive every guard that took a reference to it, so that a thread arriving
        // later contends on the same mutex as the waiters already queued on it. Two references are
        // the map's own and this guard's; `remove_if` holds the shard lock across the count and the
        // removal, which is the same lock `acquire` takes to create an entry.
        self.map.remove_if(&self.key, |_, mutex| Arc::strong_count(mutex) == 2);
    }
}

#[cfg(test)]
mod tests {
    use std::{
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc,
        },
        thread,
        time::Duration,
    };

    use super::*;

    /// Tracks how many threads were inside the critical section at once.
    #[derive(Default)]
    struct Occupancy {
        current: AtomicUsize,
        max: AtomicUsize,
    }

    impl Occupancy {
        fn enter(&self) {
            let n = self.current.fetch_add(1, Ordering::SeqCst) + 1;
            self.max.fetch_max(n, Ordering::SeqCst);
        }

        fn leave(&self) {
            self.current.fetch_sub(1, Ordering::SeqCst);
        }
    }

    #[test]
    fn an_arrival_contends_with_a_waiter_that_took_the_mutex_before_it() {
        let sem = ConcurrentMapSemaphore::new(10);
        let occupancy = Arc::new(Occupancy::default());

        let (first_holds_tx, first_holds_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel::<()>();
        let (waiter_acquired_tx, waiter_acquired_rx) = mpsc::channel();
        let (waiter_in_section_tx, waiter_in_section_rx) = mpsc::channel();
        let (release_waiter_tx, release_waiter_rx) = mpsc::channel::<()>();

        let first = thread::spawn({
            let sem = sem.clone();
            move || {
                let guard = sem.acquire(1);
                let _access = guard.access();
                first_holds_tx.send(()).unwrap();
                release_first_rx.recv().unwrap();
            }
        });

        first_holds_rx.recv().unwrap();

        // Takes a reference to the mutex the first thread holds, then blocks on it.
        let waiter = thread::spawn({
            let sem = sem.clone();
            let occupancy = occupancy.clone();
            move || {
                let guard = sem.acquire(1);
                waiter_acquired_tx.send(()).unwrap();
                let _access = guard.access();
                occupancy.enter();
                waiter_in_section_tx.send(()).unwrap();
                release_waiter_rx.recv().unwrap();
                occupancy.leave();
            }
        });

        waiter_acquired_rx.recv().unwrap();
        thread::sleep(Duration::from_millis(50));
        release_first_tx.send(()).unwrap();
        first.join().unwrap();
        waiter_in_section_rx.recv().unwrap();

        // Arrives once the first thread has released its guard, while the waiter is still inside.
        let arrival = thread::spawn({
            let sem = sem.clone();
            let occupancy = occupancy.clone();
            move || {
                let guard = sem.acquire(1);
                let _access = guard.access();
                occupancy.enter();
                occupancy.leave();
            }
        });

        thread::sleep(Duration::from_millis(50));
        release_waiter_tx.send(()).unwrap();
        waiter.join().unwrap();
        arrival.join().unwrap();

        assert_eq!(
            occupancy.max.load(Ordering::SeqCst),
            1,
            "two threads held the same key at once",
        );
        assert_eq!(sem.map.len(), 0, "the last guard leaves no entry behind");
    }
}
