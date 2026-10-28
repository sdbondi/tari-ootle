//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_ootle_common_types::{Epoch, StateVersion, shard::Shard};
use tari_ootle_storage::StorageError;
use tari_state_tree::TransitionAccumulator;

/// A staged state version's transition accumulator, and who streamed it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedAccumulator {
    pub state_version: StateVersion,
    pub epoch: Epoch,
    /// The shard's transition accumulator after the version.
    pub transition_acc: TransitionAccumulator,
    /// The peer id bytes of the committee member that streamed it.
    pub streamed_by: Vec<u8>,
}

/// A streamed state version awaiting a verified shard leaf.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagedVersion {
    pub accumulator: StagedAccumulator,
    /// The version's length-delimited, prost-encoded `SubstateBatch` chunks, in the order they were streamed.
    pub batch: Vec<u8>,
}

/// A shard's state version and transition accumulator as a quorum-signed checkpoint states them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CheckpointShardLeaf {
    pub shard: Shard,
    pub state_version: StateVersion,
    pub transition_acc: TransitionAccumulator,
}

/// A row of `staged_versions` as read: `(state_version, epoch, transition_acc, streamed_by)`.
pub(crate) type StagedAccumulatorRow = (i64, i64, Vec<u8>, Vec<u8>);

pub(crate) fn decode_staged_accumulator(
    (state_version, epoch, transition_acc, streamed_by): StagedAccumulatorRow,
) -> Result<StagedAccumulator, StorageError> {
    Ok(StagedAccumulator {
        state_version: StateVersion::new(state_version as u64),
        epoch: Epoch(epoch as u64),
        transition_acc: TransitionAccumulator::try_from_bytes(&transition_acc).map_err(|e| {
            StorageError::DataInconsistency {
                details: format!("staged v{state_version} has an invalid transition accumulator: {e}"),
            }
        })?,
        streamed_by,
    })
}
