//   Copyright 2026 The Tari Project
//   SPDX-License-Identifier: BSD-3-Clause

use crate::{JmtHashScheme, LeafKey, SPARSE_MERKLE_PLACEHOLDER_HASH, TreeHash, jmt_internal_hash, jmt_leaf_hash};

/// The number of bits in a [`LeafKey`], and the depth a leaf sits at in the binary tree a root hash is computed over.
const KEY_BITS: usize = 256;

/// Computes the root hash of the tree holding a set of leaves, from the leaves in strictly ascending key order, without
/// storing the tree.
///
/// The root equals [`JellyfishMerkleTree::get_root_hash`](crate::JellyfishMerkleTree::get_root_hash) of a tree holding
/// the same keys and value hashes. A root depends only on the leaves' keys and value hashes: it is the binary sparse
/// Merkle tree over the key bits in which a subtree holding one leaf hashes as that leaf, an empty subtree as the
/// placeholder hash, and any other subtree as its two halves.
///
/// Memory is bounded by the key length: the builder holds one pending left subtree per bit of the key at most.
pub struct SortedLeafRootBuilder {
    scheme: JmtHashScheme,
    /// The finished left halves of the branches on the path to the last leaf, by strictly ascending depth.
    branches: Vec<Branch>,
    /// The subtree holding the last leaf pushed, under the deepest branch.
    last: Option<Subtree>,
    num_leaves: usize,
}

/// A branch whose left half is finished and whose right half holds the last leaf pushed.
struct Branch {
    /// The number of key bits every leaf under the branch shares.
    depth: usize,
    /// The hash of the left half, as a child of the branch.
    left: TreeHash,
}

/// A finished subtree whose leaves share their first `depth` key bits.
struct Subtree {
    depth: usize,
    /// The last key under the subtree, which carries the shared bits.
    key: LeafKey,
    hash: TreeHash,
}

impl Subtree {
    fn is_leaf(&self) -> bool {
        self.depth == KEY_BITS
    }
}

impl SortedLeafRootBuilder {
    pub fn new(scheme: JmtHashScheme) -> Self {
        Self {
            scheme,
            branches: Vec::new(),
            last: None,
            num_leaves: 0,
        }
    }

    /// The number of leaves pushed.
    pub fn num_leaves(&self) -> usize {
        self.num_leaves
    }

    /// The key of the last leaf pushed.
    pub fn last_key(&self) -> Option<&LeafKey> {
        self.last.as_ref().map(|last| &last.key)
    }

    /// Adds the next leaf. Fails, leaving the builder unchanged, if `key` is not above every key pushed before it.
    pub fn push(&mut self, key: LeafKey, value_hash: &TreeHash) -> Result<(), SortedLeafRootError> {
        let leaf = Subtree {
            depth: KEY_BITS,
            key,
            hash: jmt_leaf_hash(self.scheme, &key, value_hash),
        };
        let Some(mut last) = self.last.take() else {
            self.last = Some(leaf);
            self.num_leaves = 1;
            return Ok(());
        };
        if key <= last.key {
            let previous = last.key;
            self.last = Some(last);
            return Err(SortedLeafRootError::NotAscending { previous, key });
        }
        // Keys are 256 bits, so two distinct keys differ within them
        let depth = last.key.common_prefix_bits_len(&key);
        // Every branch deeper than where the new leaf forks is complete: the leaves still to come sort after it
        while let Some(branch) = self.branches.pop_if(|branch| branch.depth > depth) {
            last = self.join(branch, last);
        }
        self.branches.push(Branch {
            depth,
            left: self.lift(&last, depth + 1),
        });
        self.last = Some(leaf);
        self.num_leaves += 1;
        Ok(())
    }

    /// The root hash of the tree holding every leaf pushed.
    pub fn finish(mut self) -> TreeHash {
        let Some(mut last) = self.last.take() else {
            return SPARSE_MERKLE_PLACEHOLDER_HASH;
        };
        while let Some(branch) = self.branches.pop() {
            last = self.join(branch, last);
        }
        self.lift(&last, 0)
    }

    fn join(&self, branch: Branch, right: Subtree) -> Subtree {
        let right_hash = self.lift(&right, branch.depth + 1);
        Subtree {
            depth: branch.depth,
            key: right.key,
            hash: jmt_internal_hash(self.scheme, &branch.left, &right_hash),
        }
    }

    /// The hash of `subtree` as the node at `depth` on the path to its leaves. A lone leaf hashes as itself at any
    /// depth; a subtree of several leaves is paired with an empty sibling at each depth above its own.
    fn lift(&self, subtree: &Subtree, depth: usize) -> TreeHash {
        if subtree.is_leaf() {
            return subtree.hash;
        }
        (depth..subtree.depth).rev().fold(subtree.hash, |hash, bit| {
            if key_bit(&subtree.key, bit) {
                jmt_internal_hash(self.scheme, &SPARSE_MERKLE_PLACEHOLDER_HASH, &hash)
            } else {
                jmt_internal_hash(self.scheme, &hash, &SPARSE_MERKLE_PLACEHOLDER_HASH)
            }
        })
    }
}

/// Bit `index` of `key`, most significant bit of the first byte first, which is the order the tree branches on.
fn key_bit(key: &LeafKey, index: usize) -> bool {
    (key.bytes[index / 8] >> (7 - index % 8)) & 1 == 1
}

#[derive(Debug, thiserror::Error)]
pub enum SortedLeafRootError {
    #[error("leaf {key} does not follow leaf {previous}: leaves must be pushed in strictly ascending key order")]
    NotAscending { previous: LeafKey, key: LeafKey },
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        JellyfishMerkleTree,
        JmtHashScheme::V1,
        TreeStoreWriter,
        jmt_node_hash,
        memory_store::MemoryTreeStore,
    };

    fn value_hash(seed: u64) -> TreeHash {
        jmt_node_hash(&(seed, "value"))
    }

    fn stored_root(leaves: &[(LeafKey, TreeHash)]) -> TreeHash {
        let mut mem = MemoryTreeStore::<()>::new();
        let values = leaves.iter().map(|(key, hash)| (*key, Some((*hash, ()))));
        let (root, diff) = JellyfishMerkleTree::new(&mem, V1)
            .batch_put_value_set(values, None, 1)
            .unwrap();
        for (key, node) in diff.node_batch {
            mem.insert_node(key, node).unwrap();
        }
        assert_eq!(JellyfishMerkleTree::new(&mem, V1).get_root_hash(1).unwrap(), root);
        root
    }

    fn built_root(leaves: &[(LeafKey, TreeHash)]) -> TreeHash {
        let mut sorted = leaves.to_vec();
        sorted.sort_by_key(|(key, _)| *key);
        let mut builder = SortedLeafRootBuilder::new(V1);
        for (key, hash) in &sorted {
            builder.push(*key, hash).unwrap();
        }
        assert_eq!(builder.num_leaves(), leaves.len());
        builder.finish()
    }

    /// A key that shares its first `shared_bits` bits with `base` and differs from it in the next one.
    fn sibling_key(base: &LeafKey, shared_bits: usize, tail_seed: u64) -> LeafKey {
        let mut bytes = jmt_node_hash(&(tail_seed, "tail")).into_array();
        for bit in 0..shared_bits {
            let mask = 1 << (7 - bit % 8);
            if key_bit(base, bit) {
                bytes[bit / 8] |= mask;
            } else {
                bytes[bit / 8] &= !mask;
            }
        }
        let mask = 1 << (7 - shared_bits % 8);
        if key_bit(base, shared_bits) {
            bytes[shared_bits / 8] &= !mask;
        } else {
            bytes[shared_bits / 8] |= mask;
        }
        LeafKey::new(TreeHash::new(bytes))
    }

    #[test]
    fn an_empty_tree_has_the_placeholder_root() {
        assert_eq!(built_root(&[]), SPARSE_MERKLE_PLACEHOLDER_HASH);
        assert_eq!(built_root(&[]), stored_root(&[]));
    }

    #[test]
    fn a_single_leaf_is_the_root() {
        let leaves = [(LeafKey::new(jmt_node_hash(&1u64)), value_hash(1))];
        assert_eq!(built_root(&leaves), stored_root(&leaves));
    }

    #[test]
    fn it_matches_the_stored_root_for_random_leaf_sets() {
        for (trial, num_leaves) in [2usize, 3, 5, 16, 17, 100, 257, 1000].into_iter().enumerate() {
            let leaves = (0..num_leaves as u64)
                .map(|i| {
                    let seed = ((trial as u64) << 32) | i;
                    (LeafKey::new(jmt_node_hash(&seed)), value_hash(seed))
                })
                .collect::<Vec<_>>();
            assert_eq!(built_root(&leaves), stored_root(&leaves), "{num_leaves} leaves");
        }
    }

    #[test]
    fn it_matches_the_stored_root_for_keys_sharing_long_prefixes() {
        let base = LeafKey::new(jmt_node_hash(&7u64));
        // Prefixes that end inside a nibble, on a nibble boundary and deep into the key, which the tree stores as
        // chains of internal nodes with a single child
        for shared_bits in [0, 1, 3, 4, 5, 8, 31, 63, 64, 130, 255] {
            let leaves = [
                (base, value_hash(0)),
                (sibling_key(&base, shared_bits, 1), value_hash(1)),
            ];
            assert_eq!(built_root(&leaves), stored_root(&leaves), "{shared_bits} shared bits");
        }
        // A cluster of keys under one long prefix, next to keys elsewhere in the key space
        let mut leaves = vec![(base, value_hash(0))];
        for (i, shared_bits) in [40, 41, 44, 70, 71, 200].into_iter().enumerate() {
            leaves.push((sibling_key(&base, shared_bits, i as u64), value_hash(i as u64 + 1)));
        }
        for seed in 100..110u64 {
            leaves.push((LeafKey::new(jmt_node_hash(&seed)), value_hash(seed)));
        }
        assert_eq!(built_root(&leaves), stored_root(&leaves));
    }

    #[test]
    fn it_commits_to_each_value_hash() {
        let leaves = (0..20u64)
            .map(|i| (LeafKey::new(jmt_node_hash(&i)), value_hash(i)))
            .collect::<Vec<_>>();
        let mut tampered = leaves.clone();
        tampered[11].1 = value_hash(99);
        assert_ne!(built_root(&leaves), built_root(&tampered));
        assert_ne!(built_root(&leaves), built_root(&leaves[..19]));
    }

    #[test]
    fn it_rejects_a_key_not_above_the_last() {
        let (low, high) = {
            let a = LeafKey::new(jmt_node_hash(&1u64));
            let b = LeafKey::new(jmt_node_hash(&2u64));
            if a < b { (a, b) } else { (b, a) }
        };
        let mut builder = SortedLeafRootBuilder::new(V1);
        builder.push(high, &value_hash(1)).unwrap();
        assert!(matches!(
            builder.push(low, &value_hash(2)),
            Err(SortedLeafRootError::NotAscending { .. })
        ));
        assert!(matches!(
            builder.push(high, &value_hash(2)),
            Err(SortedLeafRootError::NotAscending { .. })
        ));
        assert_eq!(builder.num_leaves(), 1);
        assert_eq!(builder.finish(), stored_root(&[(high, value_hash(1))]));
    }
}
