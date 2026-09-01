//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

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
use tari_validator_node_rpc::client::SubstateResult;
use tokio::{task, time};

use crate::{
    network_state_sync::ShardWatermarks,
    storage_sqlite::SqliteIndexerStore,
    store::{IndexerStore, IndexerStoreReadTransaction, IndexerStoreReader, IndexerStoreWriteTransaction},
};

const LOG_TARGET: &str = "tari::indexer::substate_cache";

fn now_unix_secs() -> Result<u64, SubstateCacheError> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .map_err(|e| SubstateCacheError(e.to_string()))
}

/// Substate cache backed by the indexer's own database, so that an entry and the sync watermark it
/// is justified by are written and read under one transaction.
///
/// Holds one entry per substate: its head version. An entry is served until the substate's shard
/// carries a transition that retires it - not until a timer expires - and that holds only for a shard
/// whose stream this indexer is demonstrably keeping up with, which [`ShardWatermarks`] decides.
///
/// A cached head also settles every version below it, without a validator and without a watermark.
/// Versions are contiguous and upping a substate downs its predecessor, so a substate that ever
/// reached version `L` has versions `0..L` down for good. Unlike a claim about the current state,
/// that conclusion cannot go stale: a head this indexer holds is a lower bound on the real one, so a
/// stale `L` only makes it more certainly true.
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
        let stored_id = id.clone();
        let entry = self
            .store
            .with_read_tx(move |tx| tx.substate_cache_get(&stored_id))
            .await
            .map_err(|e: StorageError| SubstateCacheError(e.to_string()))?;
        let Some(entry) = entry else {
            return Ok(None);
        };

        if let Some(version) = version &&
            version < entry.version
        {
            return Ok(Some(SubstateCacheEntry {
                version,
                substate_result: SubstateResult::Down { version },
                // Derived now rather than when the head was fetched. The head only has to have been
                // real at some point for this to hold, so the answer does not age.
                cached_at: now_unix_secs()?,
                verified: entry.verified,
            }));
        }

        // Anything else is a claim about the substate's current state, which holds only while its
        // shard is being kept up with.
        if self.watermarks.get(Self::shard_of(id), self.max_serve_lag).is_none() {
            return Ok(None);
        }

        // A version above the head is not something the cache knows anything about: this indexer is
        // behind, or the substate never reached it.
        if version.is_some_and(|version| version > entry.version) {
            return Ok(None);
        }

        Ok(Some(entry))
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
                    },
                    watermark,
                )
            })
            .await
            .map_err(|e: StorageError| SubstateCacheError(e.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tari_ootle_common_types::StateVersion;

    use super::*;
    use crate::storage_sqlite::SqliteIndexerStore;

    const MAX_SERVE_LAG: Duration = Duration::from_secs(60);

    fn substate(n: u8) -> SubstateId {
        format!("component_{:064x}", n).parse().unwrap()
    }

    async fn cache_with_head(
        version: u32,
        confirm_shard: bool,
    ) -> (tempfile::TempDir, SqliteSubstateCache, SubstateId) {
        let dir = tempfile::tempdir().unwrap();
        let store = SqliteIndexerStore::try_create(dir.path().join("indexer.db")).unwrap();
        let watermarks = Arc::new(ShardWatermarks::new());
        let id = substate(1);
        if confirm_shard {
            watermarks.confirm(SqliteSubstateCache::shard_of(&id), StateVersion::new(100));
        }
        let cache = SqliteSubstateCache::new(store, watermarks, MAX_SERVE_LAG, Duration::from_secs(300), 1000);
        // Whether the head itself is up or down is irrelevant to what it settles about lower versions.
        let result = SubstateResult::Down { version };
        cache
            .write(
                &id,
                SubstateCacheEntryRef {
                    version,
                    substate_result: &result,
                    cached_at: 0,
                    verified: true,
                },
                FetchWatermark::new(100),
            )
            .await
            .unwrap();
        (dir, cache, id)
    }

    #[tokio::test]
    async fn a_version_below_the_head_is_reported_down_without_a_committee() {
        let (_d, cache, id) = cache_with_head(6, true).await;
        let entry = cache.read(&id, Some(3)).await.unwrap().expect("no entry");
        assert_eq!(entry.version, 3);
        assert!(matches!(entry.substate_result, SubstateResult::Down { version: 3 }));
    }

    /// The head only has to have been real at some point: the real head is at or above it, so every
    /// version below is down for good. Nothing about that can go stale, so a shard this indexer has
    /// stopped keeping up with still answers.
    #[tokio::test]
    async fn the_down_inference_needs_no_watermark() {
        let (_d, cache, id) = cache_with_head(6, false).await;
        assert!(cache.read(&id, Some(3)).await.unwrap().is_some());
        // A claim about the substate's current state still needs one.
        assert!(cache.read(&id, None).await.unwrap().is_none());
        assert!(cache.read(&id, Some(6)).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn the_head_answers_for_itself_and_for_an_unversioned_read() {
        let (_d, cache, id) = cache_with_head(6, true).await;
        assert_eq!(cache.read(&id, None).await.unwrap().unwrap().version, 6);
        assert_eq!(cache.read(&id, Some(6)).await.unwrap().unwrap().version, 6);
    }

    /// Above the head the cache knows nothing: this indexer is behind, or the substate never got there.
    #[tokio::test]
    async fn a_version_above_the_head_is_a_miss() {
        let (_d, cache, id) = cache_with_head(6, true).await;
        assert!(cache.read(&id, Some(7)).await.unwrap().is_none());
    }
}
