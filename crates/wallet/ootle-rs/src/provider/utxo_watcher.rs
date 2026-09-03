//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::{collections::BTreeMap, sync::Weak};

use futures::{Stream, StreamExt};
use tari_indexer_client::{
    error::IndexerRestClientError,
    protobuf::{self, UtxoUpdatePayload},
    protobuf_stream::ProtobufStreamError,
    rest_api_client::IndexerRestApiClient,
    types::GetUtxoUpdatesRequest,
};
use tari_ootle_common_types::{Epoch, NumPreshards, StateVersion, array_utils::copy_fixed_checked, shard::Shard};
use tari_template_lib_types::{
    ResourceAddress,
    UtxoId,
    crypto::{RistrettoPublicKeyBytes, UtxoTag},
};
use tracing::error;

/// Request to watch stealth UTXO updates for a resource, resuming from a per-shard cursor.
#[derive(Debug, Clone)]
pub struct StealthUtxoWatchRequest {
    pub resource_address: ResourceAddress,
    /// Lower bound epoch for the scan. Individual shards resume from `shard_state_versions`.
    pub from_epoch: Epoch,
    /// Per-shard resume cursor. Shards not present default to `StateVersion::zero` on the indexer.
    pub shard_state_versions: Vec<(Shard, StateVersion)>,
    /// When true, only currently-unspent UTXOs are streamed (no `Spent`/`Burnt` frames).
    /// A monitor that must track spends has to set this to `false`.
    pub unspent_only: bool,
    /// Target number of updates returned per shard per pass. A pass may return fewer and still have
    /// more to give, and may overrun this to keep one state version whole, so
    /// [`StealthUtxoFrame::StartOfShard::has_more`] — not a count comparison — says whether the
    /// shard has updates left to drain.
    pub per_shard_limit: u32,
}

impl StealthUtxoWatchRequest {
    fn into_request(self) -> GetUtxoUpdatesRequest {
        GetUtxoUpdatesRequest {
            from_epoch: self.from_epoch,
            shard_state_versions: self.shard_state_versions,
            resource_address: self.resource_address,
            unspent_only: self.unspent_only,
            per_shard_limit: self.per_shard_limit,
        }
    }
}

/// A single decoded frame from the stealth UTXO update stream.
///
/// Frames arrive grouped per shard: a `StartOfShard`, then zero or more `Unspent`/`Spent`/`Burnt`
/// updates, terminated by an `EndOfShard`. `EndOfShard::max_state_version` is the shard's resume
/// point — every update at or below it has either been delivered or was excluded by the request's
/// own filters — so advance the cursor from it via [`ShardCursor::observe`].
#[derive(Debug, Clone)]
pub enum StealthUtxoFrame {
    StartOfShard {
        shard: Shard,
        max_state_version: StateVersion,
        num_updates: u32,
        /// The shard has updates beyond this pass. Poll again to drain it rather than waiting for
        /// the next scheduled pass.
        has_more: bool,
    },
    /// A currently-unspent output. Carries only the `(tag, public_nonce)` fetch key — the commitment
    /// and viewable-balance ciphertext must be resolved via
    /// [`fetch_unspent_utxos`](crate::provider::IndexerProvider::fetch_unspent_utxos).
    Unspent {
        tag: UtxoTag,
        public_nonce: RistrettoPublicKeyBytes,
    },
    Spent {
        id: UtxoId,
        version: u32,
    },
    Burnt {
        id: UtxoId,
        version: u32,
    },
    /// Terminates a shard's updates. `max_state_version` is the resume point for the shard, chosen
    /// by the indexer: the last delivered state version while the shard has more to drain, and the
    /// shard's high watermark once it is drained.
    EndOfShard {
        shard: Shard,
        max_state_version: StateVersion,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum UtxoWatcherError {
    #[error("Indexer REST client has been dropped")]
    ClientDropped,
    #[error("Indexer REST client error: {0}")]
    IndexerClientError(#[from] IndexerRestClientError),
    #[error("UTXO stream error: {0}")]
    StreamError(#[from] ProtobufStreamError),
    #[error("Failed to decode UTXO update frame: {0}")]
    DecodeError(String),
}

/// A stream of stealth UTXO updates for a resource. Created via
/// [`IndexerProvider::watch_stealth_utxos`](crate::provider::IndexerProvider::watch_stealth_utxos).
///
/// Each call drains a single pass over the requested shards (paged by `per_shard_limit`) and then
/// ends. A monitor loops: build the request from its persisted [`ShardCursor`], drain the stream,
/// advance the cursor from `EndOfShard` watermarks, then poll again.
pub struct StealthUtxoStream {
    client: Weak<IndexerRestApiClient>,
    request: StealthUtxoWatchRequest,
}

impl StealthUtxoStream {
    pub(crate) fn new(client: Weak<IndexerRestApiClient>, request: StealthUtxoWatchRequest) -> Self {
        Self { client, request }
    }

    /// Consume into a futures Stream of typed [`StealthUtxoFrame`] items.
    pub fn into_stream(self) -> impl Stream<Item = Result<StealthUtxoFrame, UtxoWatcherError>> {
        async_stream::stream! {
            let client = match self.client.upgrade() {
                Some(client) => client,
                None => {
                    error!("Indexer REST client has been dropped");
                    yield Err(UtxoWatcherError::ClientDropped);
                    return;
                },
            };

            let per_shard_limit = self.request.per_shard_limit;
            let mut stream = match client.stream_utxo_updates_protobuf(self.request.into_request()).await {
                Ok(stream) => stream,
                Err(err) => {
                    error!(%err, "Failed to start stealth UTXO update stream");
                    yield Err(UtxoWatcherError::IndexerClientError(err));
                    return;
                },
            };

            // `EndOfShard` on the wire carries neither the shard number nor what the pass returned;
            // both come from the preceding `StartOfShard`, so each pass is held until its
            // `EndOfShard` arrives.
            let mut current_pass: Option<ShardPass> = None;
            loop {
                match stream.next().await {
                    Some(Ok(payload)) => {
                        let UtxoUpdatePayload { sos, update, eos } = payload;
                        if let Some(sos) = sos {
                            let shard = Shard::from(sos.shard);
                            let max_state_version = StateVersion::from(sos.max_state_version);
                            current_pass = Some(ShardPass {
                                shard,
                                max_state_version,
                                filled_limit: sos.num_updates >= per_shard_limit,
                            });
                            yield Ok(StealthUtxoFrame::StartOfShard {
                                shard,
                                max_state_version,
                                num_updates: sos.num_updates,
                                has_more: sos.has_more,
                            });
                        }
                        if let Some(update) = update {
                            match convert_update(update) {
                                Ok(frame) => yield Ok(frame),
                                Err(err) => {
                                    yield Err(err);
                                    return;
                                },
                            }
                        }
                        if let Some(eos) = eos {
                            // Take the pass so a following `EndOfShard` (or any frame) that is not
                            // preceded by a fresh `StartOfShard` errors out rather than being
                            // misattributed to this shard and corrupting its cursor.
                            let Some(pass) = current_pass.take() else {
                                yield Err(UtxoWatcherError::DecodeError(
                                    "EndOfShard received before any StartOfShard".to_string(),
                                ));
                                return;
                            };
                            yield Ok(StealthUtxoFrame::EndOfShard {
                                shard: pass.shard,
                                max_state_version: pass.resume_from(StateVersion::from(eos.max_state_version)),
                            });
                        }
                    },
                    Some(Err(err)) => {
                        error!(%err, "Error receiving stealth UTXO update");
                        yield Err(UtxoWatcherError::StreamError(err));
                        return;
                    },
                    None => return,
                }
            }
        }
    }
}

/// What one pass over a single shard returned, held from its `StartOfShard` to its `EndOfShard`.
struct ShardPass {
    shard: Shard,
    /// The highest state version delivered in the pass.
    max_state_version: StateVersion,
    /// The pass returned `per_shard_limit` updates or more.
    filled_limit: bool,
}

impl ShardPass {
    /// The state version to resume this shard from, given the `EndOfShard` watermark.
    ///
    /// An indexer that chooses the resume point itself already accounts for what it withheld, and
    /// the cap below is inert against one. An indexer predating that reports the shard's high
    /// watermark unconditionally — the newest version it holds, taken over the whole shard — which
    /// on a pass that filled the limit sits above updates it did not send. A pass that filled the
    /// limit is therefore never resumed above its own last delivered version.
    fn resume_from(&self, watermark: StateVersion) -> StateVersion {
        if self.filled_limit {
            watermark.min(self.max_state_version)
        } else {
            watermark
        }
    }
}

fn convert_update(update: protobuf::WalletUtxoUpdate) -> Result<StealthUtxoFrame, UtxoWatcherError> {
    match update {
        protobuf::WalletUtxoUpdate::Unspent(unspent) => {
            let public_nonce = RistrettoPublicKeyBytes::from_bytes(&unspent.public_nonce)
                .map_err(|e| UtxoWatcherError::DecodeError(format!("public nonce: {e}")))?;
            Ok(StealthUtxoFrame::Unspent {
                tag: unspent.tag.into(),
                public_nonce,
            })
        },
        protobuf::WalletUtxoUpdate::Spent(spent) => {
            let id = copy_fixed_checked(&spent.id)
                .map(UtxoId::from_array)
                .ok_or_else(|| UtxoWatcherError::DecodeError("UTXO id: incorrect length".to_string()))?;
            Ok(StealthUtxoFrame::Spent {
                id,
                version: spent.version,
            })
        },
        protobuf::WalletUtxoUpdate::Burnt(burnt) => {
            let id = copy_fixed_checked(&burnt.id)
                .map(UtxoId::from_array)
                .ok_or_else(|| UtxoWatcherError::DecodeError("UTXO id: incorrect length".to_string()))?;
            Ok(StealthUtxoFrame::Burnt {
                id,
                version: burnt.version,
            })
        },
    }
}

/// A per-shard resume cursor for the stealth UTXO stream.
///
/// Seed a fresh monitor with [`ShardCursor::genesis`] (all shards at `StateVersion::zero`), pass
/// [`ShardCursor::to_pairs`] into a [`StealthUtxoWatchRequest`], and advance it from each
/// [`StealthUtxoFrame::EndOfShard`] watermark via [`ShardCursor::observe`].
#[derive(Debug, Clone, Default)]
pub struct ShardCursor {
    versions: BTreeMap<Shard, StateVersion>,
}

impl ShardCursor {
    /// A cursor covering every shard of `num_preshards`, each at `StateVersion::zero` (full backfill).
    pub fn genesis(num_preshards: NumPreshards) -> Self {
        Self {
            versions: num_preshards
                .all_shards_iter()
                .map(|shard| (shard, StateVersion::zero()))
                .collect(),
        }
    }

    /// Build a cursor from persisted `(shard, state_version)` pairs.
    pub fn from_pairs(pairs: impl IntoIterator<Item = (Shard, StateVersion)>) -> Self {
        Self {
            versions: pairs.into_iter().collect(),
        }
    }

    /// The cursor as `(shard, state_version)` pairs (ascending by shard) for a watch request.
    pub fn to_pairs(&self) -> Vec<(Shard, StateVersion)> {
        self.versions.iter().map(|(shard, v)| (*shard, *v)).collect()
    }

    /// The resume state version for a shard (`StateVersion::zero` if untracked).
    pub fn get(&self, shard: Shard) -> StateVersion {
        self.versions.get(&shard).copied().unwrap_or_else(StateVersion::zero)
    }

    /// Advance a shard's cursor to `max_state_version`. Monotonic — never moves backwards.
    pub fn observe(&mut self, shard: Shard, max_state_version: StateVersion) {
        let entry = self.versions.entry(shard).or_insert_with(StateVersion::zero);
        if max_state_version > *entry {
            *entry = max_state_version;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pass(max_state_version: u64, filled_limit: bool) -> ShardPass {
        ShardPass {
            shard: Shard::from(1u32),
            max_state_version: StateVersion::from(max_state_version),
            filled_limit,
        }
    }

    #[test]
    fn a_pass_that_filled_the_limit_is_capped_at_its_last_delivered_version() {
        // An indexer that reports the shard's high watermark regardless of what it withheld.
        assert_eq!(
            pass(100, true).resume_from(StateVersion::from(250)),
            StateVersion::from(100)
        );
    }

    #[test]
    fn a_short_pass_takes_the_watermark_whole() {
        // Nothing was withheld, so the watermark also carries the cursor past filtered-out versions.
        assert_eq!(
            pass(100, false).resume_from(StateVersion::from(250)),
            StateVersion::from(250)
        );
    }

    #[test]
    fn the_cap_is_inert_when_the_indexer_chose_the_resume_point() {
        // Such an indexer sends its last delivered version as the watermark on a truncated pass.
        assert_eq!(
            pass(100, true).resume_from(StateVersion::from(100)),
            StateVersion::from(100)
        );
    }

    #[test]
    fn cursor_advances_monotonically() {
        let mut cursor = ShardCursor::default();
        let shard = Shard::from(1u32);
        cursor.observe(shard, StateVersion::from(250));
        cursor.observe(shard, StateVersion::from(100));
        assert_eq!(cursor.get(shard), StateVersion::from(250));
    }

    #[test]
    fn genesis_covers_every_shard_at_zero() {
        let cursor = ShardCursor::genesis(NumPreshards::P4);
        let pairs = cursor.to_pairs();
        assert_eq!(pairs.len(), 4);
        assert!(pairs.iter().all(|(_, v)| *v == StateVersion::zero()));
    }
}
