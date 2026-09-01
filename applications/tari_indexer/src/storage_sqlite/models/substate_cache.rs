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

/// A substate a synced transition has retired every cached version of up to and including
/// `retires_up_to`.
///
/// A substate's first creation retires nothing and is not one of these. Before it there was nothing
/// to retire and no fetch to veto either: the only result a lookup could have returned is
/// `DoesNotExist`, which is never cached. Substates that are created once and never updated -
/// transaction receipts, most of the stream by count - therefore produce none of these at all.
#[derive(Debug, Clone)]
pub struct SubstateCacheInvalidation {
    substate_id: SubstateId,
    retires_up_to: u32,
}

impl SubstateCacheInvalidation {
    /// The substate was created at `version`, so every lower version is spent. `version` itself and
    /// anything above it are untouched: a cached head can legitimately run ahead of the transition
    /// stream, having been fetched straight from the committee.
    pub fn created(substate_id: SubstateId, version: u32) -> Option<Self> {
        version.checked_sub(1).map(|retires_up_to| Self {
            substate_id,
            retires_up_to,
        })
    }

    /// `version` was destroyed, with or without a successor.
    pub fn destroyed(substate_id: SubstateId, version: u32) -> Self {
        Self {
            substate_id,
            retires_up_to: version,
        }
    }

    pub fn substate_id(&self) -> &SubstateId {
        &self.substate_id
    }

    /// The highest cached head version this retires.
    pub fn retires_up_to(&self) -> u32 {
        self.retires_up_to
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn substate() -> SubstateId {
        format!("component_{:064x}", 1).parse().unwrap()
    }

    #[test]
    fn a_first_creation_retires_nothing() {
        assert!(SubstateCacheInvalidation::created(substate(), 0).is_none());
    }

    #[test]
    fn a_creation_retires_every_version_below_it() {
        let invalidation = SubstateCacheInvalidation::created(substate(), 6).unwrap();
        assert_eq!(invalidation.retires_up_to(), 5);
    }

    #[test]
    fn a_destroy_retires_the_version_it_names() {
        assert_eq!(SubstateCacheInvalidation::destroyed(substate(), 6).retires_up_to(), 6);
    }
}
