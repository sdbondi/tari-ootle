//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! Storage operations exclusive to the offline rollback tool.
//!
//! Functions here take a concrete `RocksDb*Transaction` and reach into the rocksdb
//! crate's CF types directly.
//!
//! `RollbackHistoryEntry` (the only one of these types that is *stored*) and
//! `RollbackHistoryCf` stay in `tari_ootle_storage` / `tari_state_store_rocksdb`
//! respectively because they're part of the registered DB schema.

pub mod read;
pub mod types;
pub mod write;

pub use read::{rollback_history_list, rollback_plan_collect_blocks, rollback_plan_collect_substates};
pub use types::{BlocksAfterEpochRow, RewindTransitionKind, RollbackDeleteStats, SubstateRewindPlanRow};
pub use write::{rollback_delete_after_epoch, rollback_history_insert};
