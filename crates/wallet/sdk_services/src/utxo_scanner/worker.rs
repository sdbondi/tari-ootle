//   Copyright 2025 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    collections::{HashMap, VecDeque},
    fmt::Display,
    future::poll_fn,
    task::{Context, Poll},
    time::Duration,
};

use futures_bounded::PushError;
use log::{info, warn};
use tari_ootle_wallet_sdk::{WalletSdk, WalletSdkSpec, models::WalletEvent};
use tari_template_lib_types::{ComponentAddress, ResourceAddress};
use tokio::{
    sync::{mpsc, oneshot, watch},
    task::JoinHandle,
};

use crate::{
    Reply,
    notify::Notify,
    utxo_scanner::{StealthScannerApiError, UtxoScanRoundStats, UtxoScanner},
};

const LOG_TARGET: &str = "tari::ootle::wallet_services::stealth_utxo_scanner";

const MAX_CONCURRENT_SCANS: usize = 10;
const SCAN_TIMEOUT: Duration = Duration::from_secs(300);

type ScanResult = Result<UtxoScanRoundStats, StealthScannerApiError>;

#[derive(Debug, Clone)]
pub struct UtxoScannerHandle {
    tx: mpsc::UnboundedSender<UtxoScanRequest>,
    notify_sub: watch::Receiver<()>,
}

impl UtxoScannerHandle {
    /// Requests a scan without waiting for it. A request for a scan that is already queued or running is merged into
    /// it.
    pub fn request_scan(&self, account_address: ComponentAddress, resource_address: ResourceAddress) {
        let request = UtxoScanRequest {
            key: UtxoScanKey {
                account_address,
                resource_address,
            },
            reply: None,
        };
        if let Err(e) = self.tx.send(request) {
            warn!(target: LOG_TARGET, "❓️ NEVER HAPPEN: UTXO scan request channel disconnected: {}", e);
        }
    }

    /// Scans for UTXOs and waits for the scan to finish. The scan starts after any scan already running for the same
    /// account and resource, so it covers every UTXO update the indexer has at the time of this call.
    pub async fn scan(
        &self,
        account_address: ComponentAddress,
        resource_address: ResourceAddress,
    ) -> Result<UtxoScanRoundStats, StealthScannerApiError> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(UtxoScanRequest {
                key: UtxoScanKey {
                    account_address,
                    resource_address,
                },
                reply: Some(reply_tx),
            })
            .map_err(|_| StealthScannerApiError::ScannerShutdown)?;
        reply_rx.await.map_err(|_| StealthScannerApiError::ScannerShutdown)?
    }

    pub fn subscribe_notifications(&self) -> watch::Receiver<()> {
        self.notify_sub.clone()
    }
}

pub struct StealthUtxoScannerWorker<TSpec: WalletSdkSpec> {
    scanner: StealthUtxoScanner<TSpec>,
}

impl<TSpec> StealthUtxoScannerWorker<TSpec>
where
    TSpec: WalletSdkSpec + Send + 'static,
    TSpec::Store: Clone + Send + Sync + 'static,
    TSpec::NetworkInterface: Clone + Send + Sync + 'static,
    TSpec::KeyStore: Clone + Send + Sync + 'static,
{
    pub fn new(sdk: WalletSdk<TSpec>, notify: Notify<WalletEvent>) -> Self {
        Self {
            scanner: StealthUtxoScanner::new(sdk, notify),
        }
    }

    pub fn spawn(self) -> (JoinHandle<anyhow::Result<()>>, UtxoScannerHandle) {
        let (tx, rx) = mpsc::unbounded_channel();
        let notify_sub = self.scanner.subscribe_notifications();

        let handle = tokio::spawn(async move {
            let mut worker = self;
            worker.run(rx).await;
            Ok(())
        });

        (handle, UtxoScannerHandle { tx, notify_sub })
    }

    async fn run(&mut self, mut work_queue: mpsc::UnboundedReceiver<UtxoScanRequest>) {
        info!(target: LOG_TARGET, "🔍️ Stealth UTXO scanner worker started");
        loop {
            let poll_fut = poll_fn(|cx| self.scanner.poll(cx));
            tokio::select! {
                biased;
                maybe_req = work_queue.recv() => {
                    match maybe_req {
                        Some(req) => {
                            self.scanner.enqueue_work(req);
                        },
                        None => break, // Channel closed
                    };
                },
                _ = poll_fut => {
                    // All work completed - continue
                },
            }
        }

        info!(target: LOG_TARGET, "🔍️ Stealth UTXO scanner worker exiting");
    }
}

pub struct StealthUtxoScanner<TSpec: WalletSdkSpec> {
    in_progress_work: futures_bounded::FuturesMap<UtxoScanKey, ScanResult>,
    /// The reply for each running scan that a caller is waiting on.
    in_progress_replies: HashMap<UtxoScanKey, Reply<ScanResult>>,
    /// Requests that wait for a free slot, or for the running scan of the same key to finish.
    pending: VecDeque<UtxoScanRequest>,
    sdk: WalletSdk<TSpec>,
    notify_tx: watch::Sender<()>,
    wallet_notify: Notify<WalletEvent>,
}

impl<TSpec> StealthUtxoScanner<TSpec>
where
    TSpec: WalletSdkSpec + 'static,
    TSpec::Store: Clone + Send + Sync + 'static,
    TSpec::NetworkInterface: Clone + Send + Sync + 'static,
    TSpec::KeyStore: Clone + Send + Sync + 'static,
{
    pub(self) fn new(sdk: WalletSdk<TSpec>, wallet_events: Notify<WalletEvent>) -> Self {
        let (notify_tx, _) = watch::channel::<()>(());
        Self {
            in_progress_work: futures_bounded::FuturesMap::new(SCAN_TIMEOUT, MAX_CONCURRENT_SCANS),
            in_progress_replies: HashMap::new(),
            pending: VecDeque::new(),
            sdk,
            notify_tx,
            wallet_notify: wallet_events,
        }
    }

    pub fn subscribe_notifications(&self) -> watch::Receiver<()> {
        self.notify_tx.subscribe()
    }

    pub(self) fn enqueue_work(&mut self, request: UtxoScanRequest) {
        info!(target: LOG_TARGET, "🔍️ Received scan request for {}", request.key);

        if request.reply.is_none() &&
            (self.in_progress_work.contains(request.key) || self.pending.iter().any(|r| r.key == request.key))
        {
            info!(target: LOG_TARGET, "🔍️ Scan for {} is already queued, ignoring request", request.key);
            return;
        }

        if let Some(request) = self.try_start(request) {
            self.pending.push_back(request);
        }
    }

    /// Starts the scan, or hands the request back if it has to wait.
    fn try_start(&mut self, request: UtxoScanRequest) -> Option<UtxoScanRequest> {
        if self.in_progress_work.contains(request.key) || self.in_progress_work.len() >= MAX_CONCURRENT_SCANS {
            return Some(request);
        }
        let UtxoScanRequest { key, reply } = request;
        match self.in_progress_work.try_push(
            key,
            do_work(
                self.sdk.clone(),
                self.notify_tx.clone(),
                key,
                self.wallet_notify.clone(),
            ),
        ) {
            Ok(()) => {
                if let Some(reply) = reply {
                    self.in_progress_replies.insert(key, reply);
                }
                None
            },
            Err(PushError::BeyondCapacity(_)) => Some(UtxoScanRequest { key, reply }),
            Err(PushError::Replaced(_)) => {
                unreachable!("BUG: Already checked for existing work but got Replaced error")
            },
        }
    }

    fn start_pending(&mut self) {
        for _ in 0..self.pending.len() {
            let Some(request) = self.pending.pop_front() else {
                break;
            };
            if let Some(request) = self.try_start(request) {
                self.pending.push_back(request);
            }
        }
    }

    pub(self) fn poll(&mut self, cx: &mut Context<'_>) -> Poll<()> {
        while let Poll::Ready((key, result)) = self.in_progress_work.poll_unpin(cx) {
            let result = match result {
                Ok(Ok(stats)) => {
                    info!(target: LOG_TARGET, "🔍️ Completed scan for {}", key);
                    Ok(stats)
                },
                Ok(Err(e)) => {
                    warn!(target: LOG_TARGET, "❓️ Error during UTXO scan for {}: {}", key, e);
                    Err(e)
                },
                Err(_) => {
                    warn!(target: LOG_TARGET, "❓️ UTXO scan for {} timed out", key);
                    Err(StealthScannerApiError::ScanTimedOut { timeout: SCAN_TIMEOUT })
                },
            };
            if let Some(reply) = self.in_progress_replies.remove(&key) {
                let _ignore = reply.send(result);
            }
            self.start_pending();
        }
        // NOTE: do not return Ready here. The caller is polling in a loop, and if there is no work to do, the loop will
        // spin.
        Poll::Pending
    }
}

async fn do_work<TSpec: WalletSdkSpec>(
    sdk: WalletSdk<TSpec>,
    notify_tx: watch::Sender<()>,
    key: UtxoScanKey,
    wallet_notify: Notify<WalletEvent>,
) -> ScanResult {
    info!(target: LOG_TARGET, "🔍 Scanning for UTXOs for {}", key);
    let account = sdk.accounts_api().get_account_by_address(&key.account_address)?;
    let stats = UtxoScanner::new(sdk, wallet_notify)
        .scan_and_enqueue_utxos(&account, &key.resource_address)
        .await?;

    // UTXOs were found, notify the Utxo recovery worker that there is work to do
    if stats.num_potential_recoveries > 0 {
        let _ = notify_tx.send(());
    }

    Ok(stats)
}

#[derive(Debug)]
struct UtxoScanRequest {
    key: UtxoScanKey,
    reply: Option<Reply<ScanResult>>,
}

#[derive(Debug, Clone, Copy, Hash, PartialEq, Eq)]
struct UtxoScanKey {
    account_address: ComponentAddress,
    resource_address: ResourceAddress,
}

impl Display for UtxoScanKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} ({})", self.account_address, self.resource_address)
    }
}
