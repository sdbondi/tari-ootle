//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Background compilation of templates a node is about to need.
//!
//! Compiling a WASM template is unpriced work on the critical path: the first transaction in a block
//! to call a template the process has not seen pays its Cranelift compile inside consensus
//! execution. This subsystem moves that compile earlier, to the moment the node learns it will need
//! the template, so that execution finds a resident module.
//!
//! Two properties hold it in place:
//!
//! - The cache is a memo, never an authority. A prewarm that has not finished, or never ran, is a cache miss, and
//!   `get_template` compiles inline exactly as it does today. Nothing here can change the outcome of an execution, only
//!   its latency.
//! - Only validated transactions are prewarmed. Compiling for anything that reached the node unvalidated is free CPU
//!   for whoever can gossip.
//!
//! Work goes through the *shared* [`MemoryCacheTemplateProvider`], not a private copy: its
//! per-address semaphore is then what coalesces a prewarm with an execution that wants the same
//! template, so the two never compile it twice and no single-flight machinery is needed here.
//!
//! [`MemoryCacheTemplateProvider`]: tari_ootle_template_provider::MemoryCacheTemplateProvider

use std::{
    collections::HashSet,
    iter,
    sync::{
        Arc,
        Mutex,
        MutexGuard,
        PoisonError,
        mpsc::{self, Receiver, SyncSender, TrySendError},
    },
    thread,
};

use log::*;
use tari_engine_types::{component::Component, substate::SubstateId};
use tari_ootle_common_types::services::template_provider::TemplateProvider;
use tari_ootle_storage::{StateStore, StateStoreReadTransaction};
use tari_ootle_template_provider::ResidentTemplateProvider;
use tari_ootle_transaction::Transaction;
use tari_template_builtin::is_builtin_template_address;
use tari_template_lib::types::{ComponentAddress, TemplateAddress};

#[cfg(feature = "metrics")]
use crate::template_prewarm::metrics::PrometheusPrewarmMetrics;

mod hooks;
pub use hooks::TemplatePrewarmHooks;

#[cfg(feature = "metrics")]
mod metrics;

const LOG_TARGET: &str = "tari::validator_node::template_prewarm";

/// Requests the queue holds before [`TemplatePrewarmer::enqueue`] starts dropping them.
///
/// A drop costs the latency this subsystem exists to remove and nothing else, so the queue is sized
/// to absorb a burst of distinct templates rather than to never overflow. Duplicates do not occupy
/// it: a target already queued is not queued again.
const QUEUE_CAPACITY: usize = 2048;

/// Upper bound on worker threads, whatever the machine's core count.
///
/// The pool's thread count is its whole concurrency budget. Each worker in a compile holds one of
/// the shared provider's `CONCURRENT_ACCESS_LIMIT` (100) permits, so the bound must stay a small
/// fraction of that limit for executor lookups to keep finding permits free.
const MAX_WORKERS: usize = 4;

/// What a prewarm request names.
///
/// A component is a request to prewarm whatever template it instantiates: a `CallMethod` is the one
/// instruction shape that does not name its template, and reading the component to find out costs a
/// state-store lookup that belongs on a worker rather than on the caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum PrewarmTarget {
    Template(TemplateAddress),
    Component(ComponentAddress),
}

/// Cloneable handle to the prewarm pool. Enqueueing never blocks.
#[derive(Debug, Clone)]
pub struct TemplatePrewarmer {
    tx: SyncSender<PrewarmTarget>,
    queued: Queued,
    #[cfg(feature = "metrics")]
    metrics: PrometheusPrewarmMetrics,
}

impl TemplatePrewarmer {
    /// Queue every template a transaction will need that this node could have to compile.
    ///
    /// The caller must have validated the transaction first.
    pub fn prewarm_transaction(&self, transaction: &Transaction) {
        // A published template's address exists only once its substate is committed, so the
        // templates a transaction publishes are prewarmed by TemplatePrewarmHooks instead.
        for address in transaction.referenced_templates_iter() {
            self.enqueue(PrewarmTarget::Template(*address));
        }
        for component in transaction.as_referenced_components() {
            self.enqueue(PrewarmTarget::Component(*component));
        }
    }

    /// Queue a single template by address.
    pub fn prewarm_template(&self, address: TemplateAddress) {
        self.enqueue(PrewarmTarget::Template(address));
    }

    fn enqueue(&self, target: PrewarmTarget) {
        // Builtins are compiled at startup and held for the life of the provider, so they are never
        // work.
        if let PrewarmTarget::Template(address) = target &&
            is_builtin_template_address(&address)
        {
            return;
        }

        if !self.queued().insert(target) {
            return;
        }

        match self.tx.try_send(target) {
            Ok(_) => {
                #[cfg(feature = "metrics")]
                self.metrics.on_enqueued(self.queued().len());
            },
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.queued().remove(&target);
                debug!(target: LOG_TARGET, "Prewarm queue is full, dropping {:?}", target);
                #[cfg(feature = "metrics")]
                self.metrics.on_dropped();
            },
        }
    }

    fn queued(&self) -> MutexGuard<'_, HashSet<PrewarmTarget>> {
        self.queued.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

type Queued = Arc<Mutex<HashSet<PrewarmTarget>>>;

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
    TStore: StateStore + Clone + Send + Sync + 'static,
{
    let components = StateStoreComponentLookup(store);
    let num_workers = worker_count();
    let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
    let queued: Queued = Arc::new(Mutex::new(HashSet::new()));
    #[cfg(feature = "metrics")]
    let metrics = PrometheusPrewarmMetrics::new(registry);

    // One receiver shared by the pool. A worker holds the lock only while waiting for the next
    // target, so the thread that takes a target releases the lock before compiling it and the next
    // worker starts waiting immediately.
    let rx = Arc::new(Mutex::new(rx));
    for i in 0..num_workers {
        let worker = Worker {
            rx: rx.clone(),
            queued: queued.clone(),
            provider: provider.clone(),
            components: components.clone(),
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

/// Resolves the template a component instantiates.
///
/// Its own seam because a `CallMethod` names a component, not a template, and everything else the
/// pool does needs only the template provider.
pub trait ComponentTemplateLookup: Send + Sync + Clone + 'static {
    /// The template `component` instantiates, if this node holds the component. A component in
    /// another shard group is absent from this node's state, and its template is not one this node
    /// executes against.
    fn template_of(&self, component: &ComponentAddress) -> Option<TemplateAddress>;
}

/// Reads components out of the local state store at their latest version.
#[derive(Debug, Clone)]
pub struct StateStoreComponentLookup<TStore>(TStore);

impl<TStore: StateStore + Clone + Send + Sync + 'static> ComponentTemplateLookup for StateStoreComponentLookup<TStore> {
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

struct Worker<TProvider, TLookup> {
    rx: Arc<Mutex<Receiver<PrewarmTarget>>>,
    queued: Queued,
    provider: TProvider,
    components: TLookup,
    #[cfg(feature = "metrics")]
    metrics: PrometheusPrewarmMetrics,
}

impl<TProvider, TLookup> Worker<TProvider, TLookup>
where
    TProvider: TemplateProvider + ResidentTemplateProvider,
    TLookup: ComponentTemplateLookup,
{
    fn run(self) {
        loop {
            let target = {
                let rx = self.rx.lock().unwrap_or_else(PoisonError::into_inner);
                rx.recv()
            };
            let Ok(target) = target else {
                debug!(target: LOG_TARGET, "Prewarm queue closed, worker exiting");
                return;
            };

            let address = match target {
                PrewarmTarget::Template(address) => Some(address),
                PrewarmTarget::Component(component) => self.components.template_of(&component),
            };
            if let Some(address) = address {
                self.prewarm(&address);
            }

            // Held in the queued set until the work is done, so that a target wanted again while
            // this compile is in flight is dropped rather than sent to a second worker that would
            // block on the provider's semaphore for the whole of this compile. The pool has a
            // handful of threads; each one waiting on a compile another is already doing is the
            // whole pool.
            let _queue_depth = {
                let mut queued = self.queued.lock().unwrap_or_else(PoisonError::into_inner);
                queued.remove(&target);
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
        collections::HashMap,
        sync::atomic::{AtomicUsize, Ordering},
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

    #[derive(Clone, Default)]
    struct FakeComponents(Arc<HashMap<ComponentAddress, TemplateAddress>>);

    impl ComponentTemplateLookup for FakeComponents {
        fn template_of(&self, component: &ComponentAddress) -> Option<TemplateAddress> {
            self.0.get(component).copied()
        }
    }

    /// A handle whose queue nothing drains, so that a test sees exactly what was enqueued.
    fn undrained() -> (TemplatePrewarmer, Receiver<PrewarmTarget>) {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let prewarmer = TemplatePrewarmer {
            tx,
            queued: Arc::new(Mutex::new(HashSet::new())),
            #[cfg(feature = "metrics")]
            metrics: PrometheusPrewarmMetrics::new(&mut prometheus_client::registry::Registry::default()),
        };
        (prewarmer, rx)
    }

    /// Runs one worker over `targets` and returns once it has drained them all.
    fn drain(provider: FakeProvider, components: FakeComponents, targets: &[PrewarmTarget]) {
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        let queued: Queued = Arc::new(Mutex::new(HashSet::new()));
        for target in targets {
            queued.lock().unwrap().insert(*target);
            tx.send(*target).unwrap();
        }
        drop(tx);

        let worker = Worker {
            rx: Arc::new(Mutex::new(rx)),
            queued,
            provider,
            components,
            #[cfg(feature = "metrics")]
            metrics: PrometheusPrewarmMetrics::new(&mut prometheus_client::registry::Registry::default()),
        };
        // The worker returns when the closed queue runs dry, which is what bounds this test.
        thread::spawn(move || worker.run()).join().unwrap();
    }

    #[test]
    fn a_builtin_is_never_queued() {
        let (prewarmer, rx) = undrained();
        prewarmer.prewarm_template(ACCOUNT_TEMPLATE_ADDRESS);
        assert!(prewarmer.queued().is_empty());
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_target_already_queued_is_not_queued_again() {
        let (prewarmer, rx) = undrained();
        prewarmer.prewarm_template(template(1));
        prewarmer.prewarm_template(template(1));
        assert_eq!(prewarmer.queued().len(), 1);
        assert_eq!(rx.try_recv().unwrap(), PrewarmTarget::Template(template(1)));
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn a_full_queue_drops_rather_than_blocks() {
        let (prewarmer, _rx) = undrained();
        for i in 0..QUEUE_CAPACITY + 10 {
            let mut bytes = [0u8; 32];
            bytes[..8].copy_from_slice(&(i as u64).to_le_bytes());
            prewarmer.prewarm_template(TemplateAddress::from_array(bytes));
        }
        assert_eq!(prewarmer.queued().len(), QUEUE_CAPACITY);
    }

    #[test]
    fn a_template_that_is_already_resident_is_not_loaded_again() {
        let provider = FakeProvider::with_resident(template(1));
        drain(provider.clone(), FakeComponents::default(), &[PrewarmTarget::Template(
            template(1),
        )]);
        assert_eq!(provider.loads(), 0);
    }

    #[test]
    fn a_component_target_loads_the_template_it_instantiates() {
        let component = ComponentAddress::from_array([7u8; 32]);
        let provider = FakeProvider::default();
        let components = FakeComponents(Arc::new([(component, template(2))].into_iter().collect()));

        drain(provider.clone(), components, &[PrewarmTarget::Component(component)]);

        assert_eq!(provider.loads(), 1);
        assert!(provider.is_resident(&template(2)));
    }

    #[test]
    fn a_component_this_node_does_not_hold_is_no_work() {
        let provider = FakeProvider::default();
        drain(provider.clone(), FakeComponents::default(), &[
            PrewarmTarget::Component(ComponentAddress::from_array([9u8; 32])),
        ]);
        assert_eq!(provider.loads(), 0);
    }

    #[test]
    fn a_drained_target_can_be_queued_again() {
        let queued: Queued = Arc::new(Mutex::new(HashSet::new()));
        let (tx, rx) = mpsc::sync_channel(QUEUE_CAPACITY);
        queued.lock().unwrap().insert(PrewarmTarget::Template(template(3)));
        tx.send(PrewarmTarget::Template(template(3))).unwrap();
        drop(tx);

        let worker = Worker {
            rx: Arc::new(Mutex::new(rx)),
            queued: queued.clone(),
            provider: FakeProvider::default(),
            components: FakeComponents::default(),
            #[cfg(feature = "metrics")]
            metrics: PrometheusPrewarmMetrics::new(&mut prometheus_client::registry::Registry::default()),
        };
        thread::spawn(move || worker.run()).join().unwrap();

        assert!(queued.lock().unwrap().is_empty());
    }
}
