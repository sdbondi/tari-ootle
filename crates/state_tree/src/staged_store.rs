//   Copyright 2024 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use std::collections::HashMap;

use tari_jellyfish::{JmtStorageError, Node, NodeKey, StaleTreeNode, TreeStoreReader, TreeStoreWriter};

use crate::{IndexedTreeDiff, StateHashTreeDiff};

/// A tree store that reads through the diffs of pending (uncommitted) versions to a readable store, and stages the
/// nodes the next version writes.
pub struct StagedTreeStore<'s, S, P> {
    readable_store: &'s S,
    /// Pending diffs, oldest first. A node key names one node for good, and the tree only reads nodes its current
    /// root reaches, so a node a later pending diff marks stale is never looked up and the diffs are read unpruned.
    pending_diffs: Vec<IndexedTreeDiff<P>>,
    new_tree_nodes: HashMap<NodeKey, Node<P>>,
    new_stale_nodes: Vec<StaleTreeNode>,
}

impl<'s, S: TreeStoreReader<P>, P> StagedTreeStore<'s, S, P> {
    pub fn new(readable_store: &'s S) -> Self {
        Self {
            readable_store,
            pending_diffs: Vec::new(),
            new_tree_nodes: HashMap::new(),
            new_stale_nodes: Vec::new(),
        }
    }

    /// Layers the diff of the next pending version over those already applied.
    pub fn apply_pending_diff(&mut self, diff: IndexedTreeDiff<P>) {
        self.pending_diffs.push(diff);
    }

    pub fn into_diff(self) -> StateHashTreeDiff<P> {
        StateHashTreeDiff {
            new_nodes: self.new_tree_nodes.into_iter().collect(),
            stale_tree_nodes: self.new_stale_nodes,
        }
    }
}

impl<S: TreeStoreReader<P>, P: Clone> TreeStoreReader<P> for StagedTreeStore<'_, S, P> {
    fn get_node(&self, key: &NodeKey) -> Result<Node<P>, JmtStorageError> {
        if let Some(node) = self.new_tree_nodes.get(key) {
            return Ok(node.clone());
        }
        if let Some(node) = self.pending_diffs.iter().rev().find_map(|diff| diff.get_node(key)) {
            return Ok(node.clone());
        }

        self.readable_store.get_node(key)
    }
}

impl<S, P> TreeStoreWriter<P> for StagedTreeStore<'_, S, P> {
    fn insert_node(&mut self, key: NodeKey, node: Node<P>) -> Result<(), JmtStorageError> {
        if self.new_tree_nodes.insert(key.clone(), node).is_some() {
            return Err(JmtStorageError::Conflict(key));
        }
        Ok(())
    }

    fn record_stale_tree_node(&mut self, stale: StaleTreeNode) -> Result<(), JmtStorageError> {
        self.new_stale_nodes.push(stale);
        Ok(())
    }
}
