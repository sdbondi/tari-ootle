//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Background compilation of templates a node is about to need.
//!
//! Compiling a WASM template is unpriced work on the critical path: a validator that reaches a
//! transaction calling a template this process has not seen pays its Cranelift compile inside
//! consensus execution, and the leader pays it inside the proposal path. This subsystem moves that
//! compile earlier, to the moment the node learns it will need the template.
//!
//! Three properties hold it in place:
//!
//! - The cache is a memo, never an authority. A prewarm that has not finished, or never ran, is a cache miss, and
//!   `get_template` compiles inline exactly as it does today. Nothing here can change the outcome of an execution, only
//!   when it happens.
//! - Only validated transactions are prewarmed. Compiling for anything that reached the node unvalidated is free CPU
//!   for whoever can gossip.
//! - A wait is a delay, never a cancellation. [`PrewarmWait::wait`] gives up on its timeout while the compile it was
//!   waiting on keeps running, so the executor that follows joins that work rather than starting its own.
//!
//! Work goes through the *shared* [`MemoryCacheTemplateProvider`], not a private copy: its
//! per-address semaphore is then what coalesces a prewarm with an execution that wants the same
//! template, so the two never compile it twice and no single-flight machinery is needed here.
//!
//! This is validator-only. The indexer has no mempool and stores no substates, so neither trigger
//! exists there; its dry-run executor compiles on demand under the bounded cache.
//!
//! [`MemoryCacheTemplateProvider`]: tari_ootle_template_provider::MemoryCacheTemplateProvider

use std::{
    collections::{HashMap, hash_map::Entry},
    fmt,
    iter,
    sync::{
        Arc,
        Mutex,
        MutexGuard,
        PoisonError,
        mpsc::{self, Receiver, SyncSender},
    },
    thread,
    time::Duration,
};

use log::*;
use tari_engine_types::{component::Component, substate::SubstateId};
use tari_ootle_common_types::services::template_provider::TemplateProvider;
use tari_ootle_storage::{StateStore, StateStoreReadTransaction};
use tari_ootle_template_provider::ResidentTemplateProvider;
use tari_ootle_transaction::Transaction;
use tari_template_builtin::is_builtin_template_address;
use tari_template_lib::types::{ComponentAddress, TemplateAddress};
use tokio::sync::oneshot;

#[cfg(feature = "metrics")]
use crate::template_prewarm::metrics::PrometheusPrewarmMetrics;

mod hooks;
pub use hooks::TemplatePrewarmHooks;

#[cfg(feature = "metrics")]
mod metrics;

const LOG_TARGET: &str = "tari::validator_node::template_prewarm";

/// Templates the queue holds before [`TemplatePrewarmer::enqueue`] starts dropping them.
///
/// A drop costs the latency this subsystem exists to remove and nothing else, so the queue is sized
/// to absorb a burst of distinct templates rather than to never overflow. Duplicates do not occupy
/// it: a template already queued or in flight collects another waiter instead of another slot.
const QUEUE_CAPACITY: usize = 2048;

/// Upper bound on worker threads, whatever the machine's core count.
///
/// The pool's thread count is its whole concurrency budget. Each worker in a compile holds one of
/// the shared provider's `CONCURRENT_ACCESS_LIMIT` (100) permits, so the bound must stay a small
/// fraction of that limit for executor lookups to keep finding permits free.
const MAX_WORKERS: usize = 4;

/// Everyone waiting on each template that is queued or in flight.
type Waiters = HashMap<TemplateAddress, Vec<oneshot::Sender<()>>>;

/// An address leaves [`Waiters`] only when its compile has finished, which is what both
/// deduplicates the queue and releases the waits.
type Queued = Arc<Mutex<Waiters>>;

/// Cloneable handle to the prewarm pool. Enqueueing never blocks.
#[derive(Clone)]
pub struct TemplatePrewarmer {
    tx: SyncSender<TemplateAddress>,
    queued: Queued,
    residency: Arc<dyn ResidentTemplateProvider + Send + Sync>,
    components: Arc<dyn ComponentTemplateLookup>,
    #[cfg(feature = "metrics")]
    metrics: PrometheusPrewarmMetrics,
}

impl TemplatePrewarmer {
    /// Queue the templates a validated transaction needs and does not already have compiled, and
    /// return a handle that completes once each of those compiles has.
    ///
    /// The caller must have validated the transaction first, and should only prewarm one it is
    /// involved in.
    pub fn prewarm_transaction(&self, transaction: &Transaction) -> PrewarmWait {
        // A published template's address exists only once its substate is committed, so the
        // templates a transaction publishes are kept by the publish execution itself and, for a node
        // that learns of one without executing its publish, by TemplatePrewarmHooks.
        let named = transaction.referenced_templates_iter().copied();
        // A `CallMethod` names a component rather than a template, so the component is read here to
        // find out which. The read is a point lookup, and doing it now rather than on a worker is
        // what makes the returned wait exact: the caller blocks on the compiles it needs and on
        // nothing else.
        let instantiated = transaction
            .as_referenced_components()
            .filter_map(|component| self.components.template_of(component));

        let receivers = named.chain(instantiated).filter_map(|a| self.enqueue(a)).collect();
        PrewarmWait { receivers }
    }

    /// Queue a template without waiting for it.
    pub fn prewarm_template(&self, address: TemplateAddress) {
        let _ignore = self.enqueue(address);
    }

    /// Queue `address` unless it is already compiled, and return what completes when its compile
    /// does. `None` means there is nothing to wait for: the template is resident, or the queue was
    /// full and this request was dropped.
    fn enqueue(&self, address: TemplateAddress) -> Option<oneshot::Receiver<()>> {
        // Builtins are compiled at startup and held for the life of the provider.
        if is_builtin_template_address(&address) || self.residency.is_resident(&address) {
            return None;
        }

        let (tx, rx) = oneshot::channel();
        {
            let mut queued = self.queued();
            match queued.entry(address) {
                Entry::Occupied(mut waiters) => {
                    waiters.get_mut().push(tx);
                    return Some(rx);
                },
                Entry::Vacant(slot) => {
                    slot.insert(vec![tx]);
                },
            }
        }

        if self.tx.try_send(address).is_err() {
            // Dropping the waiters releases everyone blocked on this address, which is what a full
            // queue degrades to: the compile happens during execution instead.
            self.queued().remove(&address);
            debug!(target: LOG_TARGET, "Prewarm queue is full, dropping template {address}");
            #[cfg(feature = "metrics")]
            self.metrics.on_dropped();
            return None;
        }

        #[cfg(feature = "metrics")]
        self.metrics.on_enqueued(self.queued().len());
        Some(rx)
    }

    fn queued(&self) -> MutexGuard<'_, Waiters> {
        self.queued.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

impl fmt::Debug for TemplatePrewarmer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TemplatePrewarmer")
            .field("queued", &self.queued().len())
            .finish_non_exhaustive()
    }
}

/// What a caller holds while the compiles it asked for are in flight.
#[must_use = "a prewarm that is never waited on is fire-and-forget"]
pub struct PrewarmWait {
    receivers: Vec<oneshot::Receiver<()>>,
}

impl PrewarmWait {
    /// Complete once every compile this wait covers has finished, or `timeout` elapses.
    ///
    /// The timeout abandons the wait, never the compile: the worker runs on, so an executor that
    /// asks for the template next joins the compile already in flight through the provider's
    /// per-address semaphore.
    pub async fn wait(self, timeout: Duration) {
        if self.receivers.is_empty() {
            return;
        }

        // A sender is dropped rather than sent on, so every outcome — compiled, failed, unknown
        // template — arrives here as a completed receiver.
        let all = futures::future::join_all(self.receivers);
        if tokio::time::timeout(timeout, all).await.is_err() {
            debug!(target: LOG_TARGET, "Prewarm did not finish within {timeout:?}");
        }
    }
}

/// Resolves the template a component instantiates.
///
/// Its own seam because a `CallMethod` names a component, not a template, and everything else the
/// pool does needs only the template provider.
pub trait ComponentTemplateLookup: Send + Sync + 'static {
    /// The template `component` instantiates, if this node holds the component. A component in
    /// another shard group is absent from this node's state, and its template is not one this node
    /// executes against.
    fn template_of(&self, component: &ComponentAddress) -> Option<TemplateAddress>;
}

/// Reads components out of the local state store at their latest version.
#[derive(Debug, Clone)]
pub struct StateStoreComponentLookup<TStore>(TStore);

impl<TStore: StateStore + Send + Sync + 'static> ComponentTemplateLookup for StateStoreComponentLookup<TStore> {
    fn template_of(&self, component: &ComponentAddress) -> Option<TemplateAddress> {
        let id = SubstateId::Component(*component);
        let records = match self
            .0
            .with_read_tx(|tx| tx.substates_get_any_max_version(iter::once(&id)))
        {
            Ok(records) => records,
            Err(e) => {
                debug!(target: LOG_TARGET, "Prewarm could not read component {component}: {e}");
                return None;
            },
        };
        records
            .into_iter()
            .find_map(|record| record.into_substate_value())
            .as_ref()
            .and_then(|value| value.component())
            .map(Component::template_address)
            .copied()
    }
}

/// Start the prewarm pool and return the handle its triggers enqueue through.
///
/// `provider` must be a clone of the provider the executor uses, which is what lets a prewarm and an
/// execution of the same template coalesce onto one compile.
///
/// The workers are OS threads rather than tokio tasks: a compile is CPU-bound and synchronous, and
/// would hold a runtime worker for its whole duration. They run until the handle and all its clones
/// are dropped, which closes the queue.
pub fn spawn<TProvider, TStore>(
    provider: TProvider,
    store: TStore,
    #[cfg(feature = "metrics")] registry: &mut prometheus_client::registry::Registry,
) -> TemplatePrewarmer
where
    TProvider: TemplateProvider + ResidentTemplateProvider,
    TStore: StateStore + Send + Sync + 'static,
{
    let num_workers = worker_count();
    let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
    let queued: Queued = Arc::new(Mutex::new(HashMap::new()));
    #[cfg(feature = "metrics")]
    let metrics = PrometheusPrewarmMetrics::new(registry);

    // One receiver shared by the pool. A worker holds the lock only while waiting for the next
    // address, so the thread that takes one releases the lock before compiling it and the next
    // worker starts waiting immediately.
    let rx = Arc::new(Mutex::new(rx));
    for i in 0..num_workers {
        let worker = Worker {
            rx: rx.clone(),
            queued: queued.clone(),
            provider: provider.clone(),
            #[cfg(feature = "metrics")]
            metrics: metrics.clone(),
        };
        thread::Builder::new()
            .name(format!("template-prewarm-{i}"))
            .spawn(move || worker.run())
            .expect("failed to spawn template prewarm worker");
    }

    info!(target: LOG_TARGET, "🔥 Template prewarm pool running with {num_workers} worker(s)");

    TemplatePrewarmer {
        tx,
        queued,
        residency: Arc::new(provider),
        components: Arc::new(StateStoreComponentLookup(store)),
        #[cfg(feature = "metrics")]
        metrics,
    }
}

/// Workers are sized off the machine rather than configured, and left well under both the core count
/// and [`MAX_WORKERS`]: prewarming competes with consensus execution for the same cores, and the
/// latency it removes is not worth the latency it would add by crowding out an executing block.
fn worker_count() -> usize {
    let cores = thread::available_parallelism().map_or(1, |n| n.get());
    (cores / 4).clamp(1, MAX_WORKERS)
}

struct Worker<TProvider> {
    rx: Arc<Mutex<Receiver<TemplateAddress>>>,
    queued: Queued,
    provider: TProvider,
    #[cfg(feature = "metrics")]
    metrics: PrometheusPrewarmMetrics,
}

impl<TProvider> Worker<TProvider>
where TProvider: TemplateProvider + ResidentTemplateProvider
{
    fn run(self) {
        loop {
            let address = {
                let rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
                rx.recv()
            };
            let Ok(address) = address else {
                debug!(target: LOG_TARGET, "Prewarm queue closed, worker exiting");
                return;
            };

            self.prewarm(&address);

            // Held in the queued map until the work is done, so that a template wanted again while
            // this compile is in flight collects a waiter rather than a second queue slot and a
            // second worker. Removing it releases every waiter, whatever the outcome.
            let _queue_depth = {
                let mut queued = self.queued.lock().unwrap_or_else(PoisonError::into_inner);
                queued.remove(&address);
                queued.len()
            };
            #[cfg(feature = "metrics")]
            self.metrics.on_finished(_queue_depth);
        }
    }

    fn prewarm(&self, address: &TemplateAddress) {
        if self.provider.is_resident(address) {
            debug!(target: LOG_TARGET, "Template {address} was resident before its prewarm ran");
            #[cfg(feature = "metrics")]
            self.metrics.on_already_resident();
            return;
        }

        // Compiles and caches as a side effect. The template itself is of no interest here: the
        // point is that the executor's next lookup finds it resident.
        match self.provider.get_template(address) {
            Ok(Some(_)) => {
                debug!(target: LOG_TARGET, "Prewarmed template {address}");
                #[cfg(feature = "metrics")]
                self.metrics.on_compiled();
            },
            Ok(None) => {
                debug!(target: LOG_TARGET, "Template {address} is not known to this node");
                #[cfg(feature = "metrics")]
                self.metrics.on_failed();
            },
            Err(e) => {
                debug!(target: LOG_TARGET, "Prewarm of template {address} failed: {e}");
                #[cfg(feature = "metrics")]
                self.metrics.on_failed();
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{
        collections::HashSet,
        sync::{
            atomic::{AtomicUsize, Ordering},
            mpsc::TryRecvError,
        },
    };

    use tari_template_builtin::ACCOUNT_TEMPLATE_ADDRESS;

    use super::*;

    fn template(n: u8) -> TemplateAddress {
        TemplateAddress::from_array([n; 32])
    }

    #[derive(Clone, Default)]
    struct FakeProvider {
        loads: Arc<AtomicUsize>,
        resident: Arc<Mutex<HashSet<TemplateAddress>>>,
    }

    impl FakeProvider {
        fn with_resident(address: TemplateAddress) -> Self {
            let provider = Self::default();
            provider.resident.lock().unwrap().insert(address);
            provider
        }

        fn loads(&self) -> usize {
            self.loads.load(Ordering::SeqCst)
        }
    }

    #[derive(Debug, thiserror::Error)]
    #[error("no template")]
    struct NoTemplate;

    impl TemplateProvider for FakeProvider {
        type Error = NoTemplate;
        type Template = ();

        fn get_template(&self, address: &TemplateAddress) -> Result<Option<Self::Template>, Self::Error> {
            self.loads.fetch_add(1, Ordering::SeqCst);
            self.resident.lock().unwrap().insert(*address);
            Ok(Some(()))
        }
    }

    impl ResidentTemplateProvider for FakeProvider {
        fn is_resident(&self, address: &TemplateAddress) -> bool {
            self.resident.lock().unwrap().contains(address)
        }
    }

    #[derive(Default)]
    struct FakeComponents(HashMap<ComponentAddress, TemplateAddress>);

    impl ComponentTemplateLookup for FakeComponents {
        fn template_of(&self, component: &ComponentAddress) -> Option<TemplateAddress> {
            self.0.get(component).copied()
        }
    }

    /// A handle whose queue nothing drains, so that a test sees exactly what was enqueued.
    fn undrained(provider: FakeProvider) -> (TemplatePrewarmer, Receiver<TemplateAddress>) {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let prewarmer = TemplatePrewarmer {
            tx,
            queued: Arc::new(Mutex::new(HashMap::new())),
            residency: Arc::new(provider),
            components: Arc::new(FakeComponents::default()),
            #[cfg(feature = "metrics")]
            metrics: PrometheusPrewarmMetrics::new(&mut prometheus_client::registry::Registry::default()),
        };
        (prewarmer, rx)
    }

    /// Runs one worker over `addresses` and returns once it has drained them all.
    fn drain(provider: FakeProvider, addresses: &[TemplateAddress]) -> Queued {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let queued: Queued = Arc::new(Mutex::new(HashMap::new()));
        for address in addresses {
            queued.lock().unwrap().insert(*address, Vec::new());
            tx.send(*address).unwrap();
        }
        drop(tx);

        let worker = Worker {
            rx: Arc::new(Mutex::new(rx)),
            queued: queued.clone(),
            provider,
            #[cfg(feature = "metrics")]
            metrics: PrometheusPrewarmMetrics::new(&mut prometheus_client::registry::Registry::default()),
        };
        // The worker returns when the closed queue runs dry, which is what bounds this test.
        thread::spawn(move || worker.run()).join().unwrap();
        queued
    }

    #[test]
    fn a_builtin_is_never_queued() {
        let (prewarmer, rx) = undrained(FakeProvider::default());
        prewarmer.prewarm_template(ACCOUNT_TEMPLATE_ADDRESS);
        assert!(prewarmer.queued().is_empty());
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn a_resident_template_is_never_queued() {
        let (prewarmer, rx) = undrained(FakeProvider::with_resident(template(1)));
        assert!(prewarmer.enqueue(template(1)).is_none());
        assert!(prewarmer.queued().is_empty());
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn a_template_already_queued_collects_a_waiter_rather_than_a_slot() {
        let (prewarmer, rx) = undrained(FakeProvider::default());
        assert!(prewarmer.enqueue(template(1)).is_some());
        assert!(prewarmer.enqueue(template(1)).is_some());
        assert_eq!(prewarmer.queued()[&template(1)].len(), 2);
        assert_eq!(rx.try_recv(), Ok(template(1)));
        assert_eq!(rx.try_recv(), Err(TryRecvError::Empty));
    }

    #[test]
    fn a_full_queue_drops_rather_than_blocks() {
        let (prewarmer, _rx) = undrained(FakeProvider::default());
        for i in 0..QUEUE_CAPACITY + 10 {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
            prewarmer.prewarm_template(TemplateAddress::from_array(bytes));
        }
        assert_eq!(prewarmer.queued().len(), QUEUE_CAPACITY);

        // A dropped request leaves the caller with nothing to wait for.
        let mut bytes = [0u8; 32];
        bytes[..8].copy_from_slice(&u64::MAX.to_le_bytes());
        assert!(prewarmer.enqueue(TemplateAddress::from_array(bytes)).is_none());
    }

    #[tokio::test]
    async fn a_wait_completes_when_its_compile_does() {
        let (prewarmer, rx) = undrained(FakeProvider::default());
        let wait = PrewarmWait {
            receivers: vec![prewarmer.enqueue(template(1)).unwrap()],
        };
        assert_eq!(rx.try_recv(), Ok(template(1)));

        // What a worker does when it finishes an address.
        prewarmer.queued().remove(&template(1));

        tokio::time::timeout(Duration::from_secs(5), wait.wait(Duration::from_secs(5)))
            .await
            .expect("wait outlived its own timeout");
    }

    #[tokio::test]
    async fn a_wait_gives_up_on_its_timeout_without_cancelling_the_compile() {
        let (prewarmer, _rx) = undrained(FakeProvider::default());
        let wait = PrewarmWait {
            receivers: vec![prewarmer.enqueue(template(1)).unwrap()],
        };

        wait.wait(Duration::from_millis(50)).await;

        assert!(
            prewarmer.queued().contains_key(&template(1)),
            "the timeout must leave the compile queued",
        );
    }

    #[tokio::test]
    async fn a_wait_on_nothing_completes_immediately() {
        let (prewarmer, _rx) = undrained(FakeProvider::with_resident(template(1)));
        let wait = PrewarmWait {
            receivers: vec![prewarmer.enqueue(template(1))].into_iter().flatten().collect(),
        };
        wait.wait(Duration::ZERO).await;
    }

    #[test]
    fn a_template_that_is_already_resident_is_not_loaded_again() {
        let provider = FakeProvider::with_resident(template(1));
        drain(provider.clone(), &[template(1)]);
        assert_eq!(provider.loads(), 0);
    }

    #[test]
    fn a_drained_template_leaves_the_queue() {
        let provider = FakeProvider::default();
        let queued = drain(provider.clone(), &[template(3)]);
        assert_eq!(provider.loads(), 1);
        assert!(queued.lock().unwrap().is_empty());
    }
}
