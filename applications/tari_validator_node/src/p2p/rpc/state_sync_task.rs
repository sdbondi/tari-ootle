//   Copyright 2023 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::num::NonZeroUsize;

use log::*;
use tari_epoch_manager::{EpochManagerReader, service::EpochManagerHandle};
use tari_ootle_common_types::{Epoch, NumPreshards, committee::CommitteeInfo, optional::Optional, shard::Shard};
use tari_ootle_p2p::{PeerAddress, proto::rpc};
use tari_ootle_storage::{
    StateStore,
    StateStoreReadTransaction,
    StorageError,
    consensus_models::{StateTransition, StateVersionTransitions, SubstateValueFilterFlags},
};
use tari_rpc_framework::RpcStatus;
use tari_state_tree::Version;
use tokio::sync::mpsc;

use crate::consensus::ConsensusHandle;

const LOG_TARGET: &str = "tari::ootle::rpc::sync_task";

/// A validated resume point for a single shard.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardCursor {
    pub shard: Shard,
    pub start_state_version: Version,
}

impl ShardCursor {
    /// Validates a caller-supplied cursor list.
    ///
    /// Ascending order rules out duplicates and lets the responder stream each shard contiguously,
    /// which is what allows a consumer to finalise a shard the moment its completion marker arrives.
    pub fn validate_all(cursors: Vec<rpc::ShardCursor>) -> Result<Vec<Self>, RpcStatus> {
        if cursors.is_empty() {
            return Err(RpcStatus::bad_request("At least one shard cursor must be provided"));
        }
        // The global shard is streamable alongside every preshard, so one more than the preshard count.
        let max_cursors = NumPreshards::MAX.num_shards() + 1;
        if cursors.len() > max_cursors {
            return Err(RpcStatus::bad_request(format!(
                "Too many shard cursors: {}. At most {max_cursors} may be requested",
                cursors.len(),
            )));
        }

        let mut validated = Vec::with_capacity(cursors.len());
        let mut prev_shard = None::<Shard>;
        for cursor in cursors {
            let shard = Shard::from_u32(cursor.shard);
            if shard > NumPreshards::MAX_SHARD {
                return Err(RpcStatus::bad_request(format!(
                    "Shard {shard} out of range. Maximum shard is {}",
                    NumPreshards::MAX_SHARD
                )));
            }
            if prev_shard.is_some_and(|prev| shard <= prev) {
                return Err(RpcStatus::bad_request(
                    "Shard cursors must be ordered by strictly ascending shard",
                ));
            }
            // Genesis is committed at version 0 and is never synced - every node bootstraps it.
            if cursor.start_state_version == 0 {
                return Err(RpcStatus::bad_request("start_state_version must be greater than 0"));
            }
            prev_shard = Some(shard);
            validated.push(Self {
                shard,
                start_state_version: cursor.start_state_version,
            });
        }

        Ok(validated)
    }

    /// Rejects any cursor for a shard that `committee_info` does not cover.
    pub fn ensure_all_stored(cursors: &[Self], committee_info: &CommitteeInfo) -> Result<(), RpcStatus> {
        cursors
            .iter()
            .try_for_each(|cursor| ensure_shard_is_stored(cursor.shard, committee_info))
    }
}

/// Rejects a shard that `committee_info` does not cover. Every committee holds the global shard, so it
/// is always in range.
fn ensure_shard_is_stored(shard: Shard, committee_info: &CommitteeInfo) -> Result<(), RpcStatus> {
    let shard_group = committee_info.shard_group();
    if shard_group.contains_or_global(&shard) {
        Ok(())
    } else {
        Err(RpcStatus::bad_request(format!(
            "This node in {shard_group} does not store {shard}"
        )))
    }
}

pub struct StateSyncTask<TStateStore: StateStore> {
    store: TStateStore,
    sender: mpsc::Sender<Result<rpc::SyncStateResponse, RpcStatus>>,
    cursors: Vec<ShardCursor>,
    end_epoch: Option<Epoch>,
    consensus: ConsensusHandle,
    epoch_manager: EpochManagerHandle<PeerAddress>,
    /// The epoch whose committee was checked to store the requested shards, for a stream that claims
    /// to serve this node's tip. `None` for a bounded stream, which makes no such claim.
    tip_authorised_at: Option<Epoch>,
    batch_size: NonZeroUsize,
    value_filters: SubstateValueFilterFlags,
}

impl<TStateStore: StateStore> StateSyncTask<TStateStore> {
    pub fn new(
        store: TStateStore,
        sender: mpsc::Sender<Result<rpc::SyncStateResponse, RpcStatus>>,
        cursors: Vec<ShardCursor>,
        end_epoch: Option<Epoch>,
        consensus: ConsensusHandle,
        epoch_manager: EpochManagerHandle<PeerAddress>,
        tip_authorised_at: Option<Epoch>,
        batch_size: NonZeroUsize,
        value_filters: SubstateValueFilterFlags,
    ) -> Self {
        Self {
            store,
            sender,
            cursors,
            end_epoch,
            consensus,
            epoch_manager,
            tip_authorised_at,
            batch_size,
            value_filters,
        }
    }

    pub async fn run(mut self) -> Result<(), ()> {
        // Each shard's updates are streamed contiguously, in the order the caller listed them, so a
        // consumer can finalise a shard the moment its completion marker arrives.
        let cursors = std::mem::take(&mut self.cursors);
        let Some(last_index) = cursors.len().checked_sub(1) else {
            // Every stream must carry a final marker, so an empty request cannot be answered with an
            // empty stream. `ShardCursor::validate_all` rejects one before it reaches here.
            self.send(Err(RpcStatus::bad_request("No shard cursors were provided")))
                .await?;
            return Err(());
        };
        for (i, cursor) in cursors.into_iter().enumerate() {
            self.run_for_shard(&cursor, i == last_index).await?;
        }
        Ok(())
    }

    async fn run_for_shard(&mut self, cursor: &ShardCursor, is_final: bool) -> Result<(), ()> {
        let shard = cursor.shard;
        // For an unbounded (sync-to-tip) request, snapshot the committed tree tip before scanning. The
        // completion marker advances the client over trailing versions that stream no updates (all
        // filtered out for its subscription). Snapshotting first ensures the marker never reports
        // beyond what we streamed: anything committed after this point is left for the next round.
        let tip_at_start = if self.end_epoch.is_none() {
            match self.read_latest_tree_version(shard) {
                Ok(version) => version,
                Err(err) => {
                    error!(target: LOG_TARGET, "🌍 Error reading latest tree version for {}: {}", shard, err);
                    self.send(Err(RpcStatus::log_internal_error(LOG_TARGET)(err))).await?;
                    return Err(());
                },
            }
        } else {
            None
        };

        let mut current_state_version = cursor.start_state_version;
        let mut counter = 0usize;
        let mut last_sent_version: Option<Version> = None;
        loop {
            match self.fetch_next_batch(shard, current_state_version) {
                Ok(Some(transitions)) => {
                    if let Some(end_epoch) = self.end_epoch {
                        // TODO(perf): might be better to not load in the first place, however also might incur the cost
                        // of a db index, more complex keys or loading from db anyway
                        if transitions.epoch > end_epoch {
                            info!(target: LOG_TARGET, "🌍 Reached end of requested epoch: {}", end_epoch);
                            break;
                        }
                    }
                    if !transitions.updates.is_empty() {
                        debug!(target: LOG_TARGET, "🌍 Fetched {} state transition(s) for {} up to v{}", transitions.updates.len(), shard, transitions.state_version);
                    }

                    current_state_version = transitions.state_version + 1;
                    counter += transitions.updates.len();

                    let state_version = transitions.state_version;
                    let has_updates = !transitions.updates.is_empty();
                    self.send_batches(transitions).await?;
                    // A version whose updates are all filtered out streams no batch, so only versions we
                    // actually sent count towards the client's recorded progress.
                    if has_updates {
                        last_sent_version = Some(state_version);
                    }
                },
                Ok(None) => {
                    // TODO: differentiate between not found and end of stream
                    debug!(target: LOG_TARGET, "🌍sync complete for {} ({}). {} update(s) sent.", shard, current_state_version, counter);
                    break;
                },
                Err(err) => {
                    error!(target: LOG_TARGET, "🌍 Error fetching state transitions: {}", err);
                    self.send(Err(RpcStatus::log_internal_error(LOG_TARGET)(err))).await?;
                    return Err(());
                },
            }
        }

        self.send_complete(cursor, tip_at_start, last_sent_version, is_final)
            .await
    }

    fn read_latest_tree_version(&self, shard: Shard) -> Result<Option<Version>, StorageError> {
        self.store.with_read_tx(|tx| tx.state_tree_versions_get_latest(shard))
    }

    /// Closes off a shard with a `SyncComplete` stating the version the client is now synced to.
    ///
    /// For an unbounded request this is the committed tree tip (capped to what we streamed), letting the
    /// client advance over trailing versions that streamed no updates - e.g. a shard whose latest
    /// transitions are all substate types the client filtered out. Such a shard otherwise streams no
    /// message at all, so the client could never observe that it has caught up and would re-scan it from
    /// scratch every round, leaving any version comparison against the committed version unsatisfiable.
    ///
    /// For a bounded request the consumer verifies against its own checkpoint, so the reported version is
    /// just our last streamed version - the consumer does not trust it as the sync target.
    ///
    /// The marker asserts that the caller is level with this node as of the epoch it names, so its epoch
    /// and this node's standing to make that claim are one fact, established together at send time by
    /// `authorise_tip`.
    async fn send_complete(
        &mut self,
        cursor: &ShardCursor,
        tip_at_start: Option<Version>,
        last_sent_version: Option<Version>,
        is_final: bool,
    ) -> Result<(), ()> {
        let epoch = match self.authorise_tip(cursor.shard).await {
            Ok(epoch) => epoch,
            Err(status) => {
                self.send(Err(status)).await?;
                return Err(());
            },
        };

        let synced_to_version = match tip_at_start {
            // Unbounded: advance to the committed tip, but never past a version we actually streamed.
            Some(tip) => tip.max(last_sent_version.unwrap_or(0)),
            // Bounded, or an unbounded shard with no committed state: report the last streamed version.
            None => last_sent_version.unwrap_or_else(|| cursor.start_state_version.saturating_sub(1)),
        };

        self.send(Ok(rpc::SyncStateResponse {
            response: Some(rpc::sync_state_response::Response::Complete(rpc::SyncComplete {
                synced_to_version,
                epoch: Some(epoch.into()),
                shard: cursor.shard.as_u32(),
                is_final,
            })),
        }))
        .await
    }

    /// Establishes this node's standing to tell the caller that it is level with the committee for
    /// `shard`, and returns the epoch that claim is made as of.
    ///
    /// Only an unbounded stream makes the claim: a bounded stream is answered out of history, which the
    /// caller verifies against its own checkpoint, so it needs nothing of this node's present standing.
    ///
    /// The claim is anchored to a single epoch - the one whose committee was checked to store these
    /// shards - and a stream can outlive it. When the epoch moves on beneath the stream, membership is
    /// re-established at the new epoch before any further claim is made under it.
    async fn authorise_tip(&mut self, shard: Shard) -> Result<Epoch, RpcStatus> {
        let epoch = self.consensus.current_epoch();
        let Some(authorised_at) = self.tip_authorised_at else {
            return Ok(epoch);
        };

        if !self.consensus.can_serve_committed_state() {
            return Err(RpcStatus::general(ConsensusHandle::CANNOT_SERVE_COMMITTED_STATE));
        }

        if epoch != authorised_at {
            let committee_info = self
                .epoch_manager
                .get_local_committee_info(epoch)
                .await
                .map_err(RpcStatus::log_internal_error(LOG_TARGET))?;
            ensure_shard_is_stored(shard, &committee_info)?;
            self.tip_authorised_at = Some(epoch);
        }

        Ok(epoch)
    }

    fn fetch_next_batch(
        &self,
        shard: Shard,
        current_state_version: Version,
    ) -> Result<Option<StateVersionTransitions>, StorageError> {
        let transitions = self.store.with_read_tx(|tx| {
            StateTransition::get_for_shard(tx, shard, current_state_version, self.value_filters).optional()
        })?;
        Ok(transitions)
    }

    async fn send(&mut self, result: Result<rpc::SyncStateResponse, RpcStatus>) -> Result<(), ()> {
        if self.sender.send(result).await.is_err() {
            debug!(
                target: LOG_TARGET,
                "Peer stream closed by client before completing. Aborting"
            );
            return Err(());
        }
        Ok(())
    }

    async fn send_batches(&mut self, transitions: StateVersionTransitions) -> Result<(), ()> {
        let shard = transitions.shard;
        let chunks = transitions.into_chunks(self.batch_size);
        let num_chunks = chunks.len();

        for (i, chunk) in chunks.into_iter().enumerate() {
            let updates = chunk.updates.into_iter().map(Into::into).collect();

            self.send(Ok(rpc::SyncStateResponse {
                response: Some(rpc::sync_state_response::Response::Batch(rpc::SubstateBatch {
                    state_version: chunk.state_version,
                    updates,
                    has_more: i < num_chunks - 1,
                    epoch: Some(chunk.epoch.into()),
                    shard: shard.as_u32(),
                })),
            }))
            .await?;
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use tari_ootle_common_types::{ShardGroup, VotePower};

    use super::*;

    fn committee_info(start: u32, end_inclusive: u32) -> CommitteeInfo {
        CommitteeInfo::new(
            NumPreshards::P64,
            4,
            8,
            ShardGroup::new(start, end_inclusive),
            Epoch(1),
            VotePower::of(4),
        )
    }

    fn cursor(shard: u32, start_state_version: u64) -> rpc::ShardCursor {
        rpc::ShardCursor {
            shard,
            start_state_version,
        }
    }

    #[test]
    fn it_accepts_ascending_cursors_including_the_global_shard() {
        let validated = ShardCursor::validate_all(vec![cursor(0, 1), cursor(1, 5), cursor(256, 9)]).unwrap();
        assert_eq!(validated, vec![
            ShardCursor {
                shard: Shard::global(),
                start_state_version: 1
            },
            ShardCursor {
                shard: Shard::from_u32(1),
                start_state_version: 5
            },
            ShardCursor {
                shard: Shard::from_u32(256),
                start_state_version: 9
            },
        ]);
    }

    #[test]
    fn it_accepts_a_cursor_for_every_shard_plus_global() {
        let cursors = (0..=NumPreshards::MAX.as_u32())
            .map(|s| cursor(s, 1))
            .collect::<Vec<_>>();
        assert_eq!(cursors.len(), NumPreshards::MAX.num_shards() + 1);
        assert!(ShardCursor::validate_all(cursors).is_ok());
    }

    #[test]
    fn it_rejects_an_empty_cursor_list() {
        assert!(ShardCursor::validate_all(vec![]).is_err());
    }

    #[test]
    fn it_rejects_more_cursors_than_there_are_shards() {
        let cursors = (0..=NumPreshards::MAX.as_u32() + 1)
            .map(|s| cursor(s, 1))
            .collect::<Vec<_>>();
        assert!(ShardCursor::validate_all(cursors).is_err());
    }

    #[test]
    fn it_rejects_an_out_of_range_shard() {
        assert!(ShardCursor::validate_all(vec![cursor(NumPreshards::MAX.as_u32() + 1, 1)]).is_err());
    }

    #[test]
    fn it_rejects_duplicate_and_out_of_order_shards() {
        assert!(ShardCursor::validate_all(vec![cursor(1, 1), cursor(1, 2)]).is_err());
        assert!(ShardCursor::validate_all(vec![cursor(2, 1), cursor(1, 1)]).is_err());
    }

    #[test]
    fn it_rejects_a_zero_start_state_version() {
        assert!(ShardCursor::validate_all(vec![cursor(1, 1), cursor(2, 0)]).is_err());
    }

    #[test]
    fn it_accepts_cursors_for_stored_shards_and_the_global_shard() {
        let cursors = ShardCursor::validate_all(vec![cursor(0, 1), cursor(9, 1), cursor(16, 1)]).unwrap();
        ShardCursor::ensure_all_stored(&cursors, &committee_info(9, 16)).unwrap();
    }

    #[test]
    fn it_rejects_a_cursor_for_a_shard_this_node_does_not_store() {
        let cursors = ShardCursor::validate_all(vec![cursor(9, 1), cursor(17, 1)]).unwrap();
        let err = ShardCursor::ensure_all_stored(&cursors, &committee_info(9, 16)).unwrap_err();
        assert!(err.details().contains("does not store Shard(17)"), "{err}");
    }
}
