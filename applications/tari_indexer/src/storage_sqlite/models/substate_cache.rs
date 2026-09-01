//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use tari_engine_types::substate::SubstateId;

#[derive(Debug, Clone, Queryable)]
pub(crate) struct SubstateCacheRow {
    #[allow(dead_code)]
    pub substate_id: String,
    pub version: i32,
    pub verified: bool,
    pub substate_result: Vec<u8>,
    pub cached_at: i64,
}

/// A substate a synced transition has touched, together with the version it reached. A cached head at
/// or below the bound the variant sets is no longer the substate's current state.
#[derive(Debug, Clone)]
pub enum SubstateCacheInvalidation {
    /// The substate was created at `version`, so every lower version is spent. `version` itself and
    /// anything above it are untouched: a cached head can legitimately run ahead of the transition
    /// stream, having been fetched straight from the committee.
    Created { substate_id: SubstateId, version: u32 },
    /// `version` was destroyed, with or without a successor.
    Destroyed { substate_id: SubstateId, version: u32 },
}

impl SubstateCacheInvalidation {
    pub fn substate_id(&self) -> &SubstateId {
        match self {
            Self::Created { substate_id, .. } | Self::Destroyed { substate_id, .. } => substate_id,
        }
    }

    /// The highest cached head version this invalidation retires. `None` when it retires nothing,
    /// which is a substate's first creation.
    pub fn retires_up_to(&self) -> Option<u32> {
        match *self {
            Self::Created { version, .. } => version.checked_sub(1),
            Self::Destroyed { version, .. } => Some(version),
        }
    }
}
