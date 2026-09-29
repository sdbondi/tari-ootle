//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::HashMap,
    fmt::Display,
    future::Future,
    hash::Hash,
    sync::{Arc, Mutex},
    time::Duration,
};

use log::*;
use tokio::{
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};

const LOG_TARGET: &str = "tari::ootle::consensus::bounded_spawn";

/// Error emitted from [`try_spawn`](self::BoundedSpawn::try_spawn) when there are no tasks available
#[derive(Debug)]
pub struct TrySpawnError;

/// A task executor bounded by a semaphore.
///
/// Use the asynchronous spawn method to spawn a task. If a given number of tasks are already spawned and have not
/// completed, the spawn function will block (asynchronously) until a previously spawned task completes.
#[derive(Debug)]
pub struct BoundedSpawn {
    // inner: runtime::Handle,
    semaphore: Arc<Semaphore>,
}

impl BoundedSpawn {
    pub fn new(num_permits: usize) -> Self {
        Self {
            semaphore: Arc::new(Semaphore::new(num_permits)),
        }
    }

    #[allow(dead_code)]
    pub fn allow_maximum() -> Self {
        Self::new(Self::max_theoretical_tasks())
    }

    #[allow(dead_code)]
    pub const fn max_theoretical_tasks() -> usize {
        // Maximum from here: https://github.com/tokio-rs/tokio/blob/ce9eabfdd12a14efb74f5e6d507f2acbe7a814c9/tokio/src/sync/batch_semaphore.rs#L101
        // NOTE: usize::MAX >> 3 does not work. The reason is not clear, however 1152921504606846975 tasks seems
        // sufficient
        usize::MAX >> 4
    }

    #[allow(dead_code)]
    pub fn can_spawn(&self) -> bool {
        self.num_available() > 0
    }

    /// Returns the remaining number of tasks that can be spawned on this executor without waiting.
    #[allow(dead_code)]
    pub fn num_available(&self) -> usize {
        self.semaphore.available_permits()
    }

    pub fn try_spawn<F>(&self, future: F) -> Result<JoinHandle<F::Output>, TrySpawnError>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        let permit = self.semaphore.clone().try_acquire_owned().map_err(|_| TrySpawnError)?;
        let handle = self.do_spawn(permit, future);
        Ok(handle)
    }

    /// Spawn a future onto the Tokio runtime asynchronously blocking if there are too many
    /// spawned tasks.
    ///
    /// This spawns the given future onto the runtime's executor, usually a
    /// thread pool. The thread pool is then responsible for polling the future
    /// until it completes.
    ///
    /// If the number of pending tasks exceeds the num_permits value given to `BoundedExecutor::new`
    /// the future returned from spawn will block until a permit is released.
    ///
    /// See [module level][mod] documentation for more details.
    ///
    /// [mod]: index.html
    ///
    /// # Examples
    ///
    /// ```
    /// use tokio::runtime::Handle;
    /// use tari_comms::bounded_executor::BoundedExecutor;
    ///
    /// # fn dox() {
    /// // Create the runtime
    /// let executor = BoundedExecutor::new(1);
    ///
    /// // Spawn a future onto the runtime
    /// // NOTE: BoundedExecutor::spawn is an async function and therefore, must be polled/awaited for the task to be spawned
    /// let task1 = executor.spawn(async {
    ///     println!("now running on a worker thread");
    /// });
    /// // This will spawn after task1
    /// let task2 = executor.spawn(async {
    ///     println!("will always run after the first task");
    /// });
    ///
    /// Handle::current().block_on(task1);
    /// Handle::current().block_on(task2);
    /// # }
    /// ```
    ///
    /// # Panics
    ///
    /// This function panics if the spawn fails. Failure occurs if the executor
    /// is currently at capacity and is unable to spawn a new future.
    #[allow(dead_code)]
    pub async fn spawn<F>(&self, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        // SAFETY: acquire_owned only fails if the semaphore is closed (i.e self.semaphore.close() is called) - this
        // never happens in this implementation
        let permit = self.semaphore.clone().acquire_owned().await.expect("semaphore closed");
        self.do_spawn(permit, future)
    }

    fn do_spawn<F>(&self, permit: OwnedSemaphorePermit, future: F) -> JoinHandle<F::Output>
    where
        F: Future + Send + 'static,
        F::Output: Send + 'static,
    {
        tokio::spawn(async move {
            // Task is finished, release the permit
            let ret = future.await;
            drop(permit);
            ret
        })
    }
}

/// A [`BoundedSpawn`] that also caps how many of its permits one key may hold at once, and bounds how long a task may
/// hold one.
///
/// A responder serving peers needs both bounds. Without the per-key cap, one peer's requests take the whole pool and
/// every other peer is refused. Without the deadline, a task awaiting a peer that has stopped reading holds its permit
/// for as long as that peer stays connected, which reaches the same state one request at a time.
#[derive(Debug, Clone)]
pub struct PerKeyBoundedSpawn<K> {
    inner: Arc<BoundedSpawn>,
    in_flight: Arc<Mutex<HashMap<K, usize>>>,
    max_per_key: usize,
    task_timeout: Duration,
}

impl<K: Eq + Hash + Clone + Display + Send + 'static> PerKeyBoundedSpawn<K> {
    pub fn new(num_permits: usize, max_per_key: usize, task_timeout: Duration) -> Self {
        assert!(
            max_per_key > 0,
            "max_per_key must be non-zero or nothing can be spawned"
        );
        Self {
            inner: Arc::new(BoundedSpawn::new(num_permits)),
            in_flight: Arc::new(Mutex::new(HashMap::new())),
            max_per_key,
            task_timeout,
        }
    }

    /// Spawns `future` against `key`'s share of the pool, abandoning it if it outlives the timeout.
    ///
    /// Returns `Err` if the pool is full or `key` already holds its share, in which case nothing is spawned.
    pub fn try_spawn<F>(&self, key: K, future: F) -> Result<JoinHandle<()>, TrySpawnError>
    where F: Future<Output = ()> + Send + 'static {
        let guard = self.reserve(key)?;
        let task_timeout = self.task_timeout;
        self.inner.try_spawn(async move {
            if tokio::time::timeout(task_timeout, future).await.is_err() {
                warn!(
                    target: LOG_TARGET,
                    "⚠️ Task for {} did not complete within {:.0?}, abandoning it to free its permit",
                    guard.key.as_ref().expect("held until drop"),
                    task_timeout,
                );
            }
            drop(guard);
        })
    }

    fn reserve(&self, key: K) -> Result<KeyGuard<K>, TrySpawnError> {
        let mut in_flight = self.lock_in_flight();
        let count = in_flight.entry(key.clone()).or_insert(0);
        if *count >= self.max_per_key {
            // Leaves a zero entry only if this key had none, which the next reserve or release removes
            return Err(TrySpawnError);
        }
        *count += 1;
        Ok(KeyGuard {
            key: Some(key),
            in_flight: self.in_flight.clone(),
        })
    }

    fn lock_in_flight(&self) -> std::sync::MutexGuard<'_, HashMap<K, usize>> {
        // A panic while counting leaves the count usable: an over-count only refuses a request, and the guard's drop
        // corrects it
        self.in_flight.lock().unwrap_or_else(|err| err.into_inner())
    }

    /// The number of tasks `key` currently holds.
    #[cfg(test)]
    pub fn in_flight_for(&self, key: &K) -> usize {
        self.lock_in_flight().get(key).copied().unwrap_or(0)
    }
}

/// Releases a key's share when its task ends, however it ends.
struct KeyGuard<K: Eq + Hash> {
    key: Option<K>,
    in_flight: Arc<Mutex<HashMap<K, usize>>>,
}

impl<K: Eq + Hash> Drop for KeyGuard<K> {
    fn drop(&mut self) {
        let Some(key) = self.key.take() else {
            return;
        };
        let mut in_flight = self.in_flight.lock().unwrap_or_else(|err| err.into_inner());
        if let Some(count) = in_flight.get_mut(&key) {
            *count = count.saturating_sub(1);
            if *count == 0 {
                in_flight.remove(&key);
            }
        }
    }
}

#[cfg(test)]
mod test {
    use std::{
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
        time::Duration,
    };

    use tokio::time::sleep;

    use super::*;

    /// One key must not be able to take the whole pool.
    #[tokio::test]
    async fn per_key_cap_refuses_a_greedy_key() {
        let spawn = PerKeyBoundedSpawn::new(4, 2, Duration::from_secs(30));
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let block = Arc::new(tokio::sync::Notify::new());

        for _ in 0..2 {
            let block = block.clone();
            spawn
                .try_spawn("greedy", async move { block.notified().await })
                .unwrap();
        }
        assert_eq!(spawn.in_flight_for(&"greedy"), 2);

        // The pool still has permits, but this key has used its share
        spawn.try_spawn("greedy", async {}).unwrap_err();
        // Another key is unaffected
        spawn
            .try_spawn("other", async move {
                let _ = rx.await;
            })
            .unwrap();
        assert_eq!(spawn.in_flight_for(&"other"), 1);

        block.notify_waiters();
        let _ = tx.send(());
    }

    /// A task that outlives the deadline is abandoned so its share returns to the pool.
    #[tokio::test(start_paused = true)]
    async fn a_task_past_its_deadline_releases_its_share() {
        let spawn = PerKeyBoundedSpawn::new(1, 1, Duration::from_millis(50));
        spawn
            .try_spawn("slow", async {
                sleep(Duration::from_secs(60)).await;
            })
            .unwrap();
        assert_eq!(spawn.in_flight_for(&"slow"), 1);

        sleep(Duration::from_millis(100)).await;
        tokio::task::yield_now().await;

        assert_eq!(
            spawn.in_flight_for(&"slow"),
            0,
            "the deadline did not release the share"
        );
        spawn.try_spawn("slow", async {}).unwrap();
    }

    #[tokio::test]
    async fn spawn() {
        let flag = Arc::new(AtomicBool::new(false));
        let flag_cloned = flag.clone();
        let executor = BoundedSpawn::new(1);

        // Spawn 1
        let task1_fut = executor
            .spawn(async move {
                sleep(Duration::from_millis(1)).await;
                flag_cloned.store(true, Ordering::SeqCst);
            })
            .await;

        // Spawn 2
        let task2_fut = executor
            .spawn(async move {
                // This will panic if this task is spawned before task1 completes (e.g if num_permitted > 1)
                assert!(flag.load(Ordering::SeqCst));
            })
            .await;

        task2_fut.await.unwrap();
        task1_fut.await.unwrap();
    }
}
