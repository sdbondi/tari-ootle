//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

//! The state tree diffs of blocks that have not yet committed.
//!
//! The table answers every pending state tree diff read. Its persistent form is one record per block holding the
//! block's diff to each shard it changes, written when the block's change set is saved and deleted when the block
//! commits or is orphaned. See [`crate::pending_state`] for how changes are staged and published.

use std::{collections::HashMap, sync::Arc};

use tari_consensus_types::BlockId;

use crate::column_families::pending_state_tree_diff::ShardStateTreeDiff;

#[derive(Debug, Clone, Default)]
pub(crate) struct TreeDiffTable {
    blocks: HashMap<BlockId, Arc<[ShardStateTreeDiff]>>,
}

impl TreeDiffTable {
    pub fn get(&self, block_id: &BlockId) -> Option<&Arc<[ShardStateTreeDiff]>> {
        self.blocks.get(block_id)
    }

    /// Sets the diffs `block_id` makes, replacing any it made before.
    pub fn insert(&mut self, block_id: BlockId, diffs: Arc<[ShardStateTreeDiff]>) {
        self.blocks.insert(block_id, diffs);
    }

    pub fn remove(&mut self, block_id: &BlockId) -> Option<Arc<[ShardStateTreeDiff]>> {
        self.blocks.remove(block_id)
    }
}
