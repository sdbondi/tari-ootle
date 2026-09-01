//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::ops::Deref;

use log::warn;
use tari_ootle_common_types::{optional::Optional, shard::Shard};
use tari_state_tree::{
    JmtStorageError,
    Node,
    NodeKey,
    StaleTreeNode,
    StateTreePayload,
    TreeStoreBatchWriter,
    TreeStoreReader,
    Version,
};

use crate::{StateStoreReadTransaction, StateStoreWriteTransaction};

const LOG_TARGET: &str = "tari::ootle::storage::sharded_state_tree";

/// Tree store that is scoped to a specific shard
#[derive(Debug)]
pub struct ShardScopedTreeStoreReader<'a, TTx> {
    shard: Shard,
    tx: &'a TTx,
}

impl<'a, TTx> ShardScopedTreeStoreReader<'a, TTx> {
    pub fn new(tx: &'a TTx, shard: Shard) -> Self {
        Self { shard, tx }
    }
}

impl<TTx: StateStoreReadTransaction> TreeStoreReader<StateTreePayload> for ShardScopedTreeStoreReader<'_, TTx> {
    fn get_node(&self, key: &NodeKey) -> Result<Node<StateTreePayload>, tari_state_tree::JmtStorageError> {
        self.tx
            .state_tree_nodes_get(self.shard, key)
            .optional()
            .map_err(|e| tari_state_tree::JmtStorageError::UnexpectedError(e.to_string()))?
            .ok_or_else(|| {
                warn!(
                    target: LOG_TARGET,
                    "ShardScopedTreeStoreReader: Node not found in shard {} with key: {}", self.shard, key
                );
                tari_state_tree::JmtStorageError::NotFound(key.clone())
            })
    }
}
#[derive(Debug)]
pub struct ShardScopedTreeStoreWriter<'a, TTx> {
    shard: Shard,
    tx: &'a mut TTx,
}

impl<'a, TTx: StateStoreWriteTransaction> ShardScopedTreeStoreWriter<'a, TTx> {
    pub fn new(tx: &'a mut TTx, shard: Shard) -> Self {
        Self { shard, tx }
    }

    /// Advances the shard's committed tree version.
    ///
    /// JMT nodes are keyed by `(version, nibble_path)`, so a version may only ever be written once:
    /// writing one a second time overwrites live nodes with keys that the same write records as stale,
    /// and the stale-node GC then deletes them from under the current tree. Every production writer -
    /// consensus block commit, state sync and genesis - funnels through here, so this is where that is
    /// enforced. Rewinds reset the pointer through `state_tree_shard_versions_set` directly, after the
    /// versions above the target no longer exist.
    pub fn set_state_version(&mut self, version: Version) -> Result<(), JmtStorageError>
    where
        TTx: Deref,
        TTx::Target: StateStoreReadTransaction,
    {
        let current_version = self
            .tx
            .state_tree_versions_get_latest(self.shard)
            .map_err(|e| JmtStorageError::UnexpectedError(e.to_string()))?;

        if let Some(current_version) = current_version &&
            version <= current_version
        {
            return Err(JmtStorageError::UnexpectedError(format!(
                "Refusing to write state tree version {version} for shard {} on top of committed version \
                 {current_version}: the next version must be greater than the current version",
                self.shard,
            )));
        }

        self.tx
            .state_tree_shard_versions_set(self.shard, version)
            .map_err(|e| JmtStorageError::UnexpectedError(e.to_string()))
    }

    pub fn record_stale_tree_nodes(
        &mut self,
        version: Version,
        nodes: Vec<StaleTreeNode>,
    ) -> Result<(), JmtStorageError> {
        self.tx
            .state_tree_nodes_record_stale_tree_nodes(self.shard, version, nodes)
            .map_err(|e| JmtStorageError::UnexpectedError(e.to_string()))
    }

    pub fn insert_nodes(&mut self, nodes: Vec<(NodeKey, Node<StateTreePayload>)>) -> Result<(), JmtStorageError> {
        self.tx
            .state_tree_nodes_batch_insert(self.shard, nodes)
            .map_err(|e| JmtStorageError::UnexpectedError(e.to_string()))
    }

    pub fn transaction(&mut self) -> &mut TTx {
        self.tx
    }
}

impl<TTx> TreeStoreReader<StateTreePayload> for ShardScopedTreeStoreWriter<'_, TTx>
where
    TTx: StateStoreWriteTransaction + Deref,
    TTx::Target: StateStoreReadTransaction,
{
    fn get_node(&self, key: &NodeKey) -> Result<Node<StateTreePayload>, JmtStorageError> {
        self.tx
            .state_tree_nodes_get(self.shard, key)
            .optional()
            .map_err(|e| JmtStorageError::UnexpectedError(e.to_string()))?
            .ok_or_else(|| {
                warn!(
                    target: LOG_TARGET,
                    "ShardScopedTreeStoreWriter: Node not found in shard {} with key: {}", self.shard, key
                );
                JmtStorageError::NotFound(key.clone())
            })
    }
}

impl<TTx: StateStoreWriteTransaction> TreeStoreBatchWriter<StateTreePayload> for ShardScopedTreeStoreWriter<'_, TTx> {
    fn batch_insert_nodes(&mut self, nodes: Vec<(NodeKey, Node<StateTreePayload>)>) -> Result<(), JmtStorageError> {
        self.tx
            .state_tree_nodes_batch_insert(self.shard, nodes)
            .map_err(|e| JmtStorageError::UnexpectedError(e.to_string()))
    }

    fn record_stale_tree_nodes(&mut self, version: Version, nodes: Vec<StaleTreeNode>) -> Result<(), JmtStorageError> {
        self.tx
            .state_tree_nodes_record_stale_tree_nodes(self.shard, version, nodes)
            .map_err(|e| JmtStorageError::UnexpectedError(e.to_string()))
    }
}
