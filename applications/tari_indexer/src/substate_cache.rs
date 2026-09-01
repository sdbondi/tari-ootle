//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{sync::Arc, time::Duration};

use log::*;
use tari_engine_types::substate::SubstateId;
use tari_indexer_lib::substate_cache::{
    FetchWatermark,
    SubstateCache,
    SubstateCacheEntry,
    SubstateCacheEntryRef,
    SubstateCacheError,
};
use tari_ootle_common_types::{NumPreshards, SubstateAddress, shard::Shard};
use tari_ootle_storage::StorageError;
use tari_shutdown::ShutdownSignal;
use tokio::{task, time};

use crate::{
    network_state_sync::ShardWatermarks,
    storage_sqlite::SqliteIndexerStore,
    store::{IndexerStore, IndexerStoreReadTransaction, IndexerStoreReader, IndexerStoreWriteTransaction},
};

const LOG_TARGET: &str = "tari::indexer::substate_cache";

/// Substate cache backed by the indexer's own database, so that an entry and the sync watermark it
/// is justified by are written and read under one transaction.
///
/// An entry is served until the substate's shard carries a transition that retires it - not until a
/// timer expires. That holds only for a shard whose stream this indexer is demonstrably keeping up
/// with, which [`ShardWatermarks`] decides.
#[derive(Clone)]
pub struct SqliteSubstateCache {
    store: SqliteIndexerStore,
    watermarks: Arc<ShardWatermarks>,
    max_serve_lag: Duration,
    /// How long a substate stays journalled as recently changed. Only has to span a committee fetch.
    journal_retention: Duration,
    max_entries: usize,
}

impl std::fmt::Debug for SqliteSubstateCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSubstateCache")
            .field("max_serve_lag", &self.max_serve_lag)
            .field("max_entries", &self.max_entries)
            .finish_non_exhaustive()
    }
}

impl SqliteSubstateCache {
    pub fn new(
        store: SqliteIndexerStore,
        watermarks: Arc<ShardWatermarks>,
        max_serve_lag: Duration,
        journal_retention: Duration,
        max_entries: usize,
    ) -> Self {
        Self {
            store,
            watermarks,
            max_serve_lag,
            journal_retention,
            max_entries,
        }
    }

    /// Periodically drops journal entries that can no longer veto a write and evicts the oldest
    /// entries down to the configured cap.
    pub fn spawn_pruner(&self, interval: Duration, mut shutdown: ShutdownSignal) -> task::JoinHandle<()> {
        let cache = self.clone();
        task::spawn(async move {
            let mut interval = time::interval(interval.max(Duration::from_secs(1)));
            interval.set_missed_tick_behavior(time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = shutdown.wait() => {
                        info!(target: LOG_TARGET, "🧹 Substate cache pruner was shutdown.");
                        break;
                    },
                    _ = interval.tick() => {
                        if let Err(e) = cache.prune().await {
                            warn!(target: LOG_TARGET, "⚠️ Failed to prune the substate cache: {e}");
                        }
                    },
                }
            }
        })
    }

    async fn prune(&self) -> Result<(), StorageError> {
        let journal_retention = self.journal_retention;
        let max_entries = self.max_entries;
        self.store
            .with_write_tx(move |tx| tx.substate_cache_prune(journal_retention, max_entries))
            .await
    }

    fn shard_of(id: &SubstateId) -> Shard {
        if id.is_global() {
            return Shard::global();
        }
        SubstateAddress::from_substate_id(id, 0).to_shard(NumPreshards::current())
    }
}

impl SubstateCache for SqliteSubstateCache {
    async fn watermark(&self, id: &SubstateId) -> Result<Option<FetchWatermark>, SubstateCacheError> {
        Ok(self
            .watermarks
            .get(Self::shard_of(id), self.max_serve_lag)
            .map(|version| FetchWatermark::new(version.as_u64())))
    }

    async fn read(
        &self,
        id: &SubstateId,
        version: Option<u32>,
    ) -> Result<Option<SubstateCacheEntry>, SubstateCacheError> {
        if self.watermarks.get(Self::shard_of(id), self.max_serve_lag).is_none() {
            return Ok(None);
        }
        let id = id.clone();
        self.store
            .with_read_tx(move |tx| tx.substate_cache_get(&id, version))
            .await
            .map_err(|e: StorageError| SubstateCacheError(e.to_string()))
    }

    async fn write(
        &self,
        id: &SubstateId,
        entry: SubstateCacheEntryRef<'_>,
        watermark: FetchWatermark,
    ) -> Result<(), SubstateCacheError> {
        let id = id.clone();
        let substate_result = entry.substate_result.clone();
        let SubstateCacheEntryRef {
            version,
            cached_at,
            verified,
            is_latest,
            ..
        } = entry;
        self.store
            .with_write_tx(move |tx| {
                tx.substate_cache_put(
                    &id,
                    SubstateCacheEntryRef {
                        version,
                        substate_result: &substate_result,
                        cached_at,
                        verified,
                        is_latest,
                    },
                    watermark,
                )
            })
            .await
            .map_err(|e: StorageError| SubstateCacheError(e.to_string()))?;
        Ok(())
    }
}
