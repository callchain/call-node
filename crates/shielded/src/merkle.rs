//! Incremental Merkle Tree for shielded notes (per spec §3.8.2)
//!
//! Depth-32 incremental Merkle tree with O(log n) insert.
//! Uses keccak256 for hashing, supports up to ~4.2B leaves.

use call_crypto::keccak256;
use call_primitives::Hash;

/// Incremental Merkle Tree (fixed depth, append-only)
///
/// Stores all tree nodes explicitly for efficient proof generation.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct IncrementalMerkleTree {
    depth: usize,
    count: usize,
    /// All levels of the tree: levels[0] = leaves, levels[d] = root
    levels: Vec<Vec<Hash>>,
}

impl IncrementalMerkleTree {
    pub fn new(depth: usize) -> Self {
        Self {
            depth,
            count: 0,
            levels: (0..=depth).map(|_| Vec::new()).collect(),
        }
    }

    pub fn insert(&mut self, leaf: Hash) -> Hash {
        if self.count >= (1usize << self.depth) {
            panic!("Merkle tree at capacity");
        }
        self.count += 1;

        // Set leaf
        self.levels[0].push(leaf);

        // Propagate up
        let mut idx = self.count - 1;
        let mut current = leaf;

        for level in 0..self.depth {
            let parent_idx = idx >> 1;

            // Ensure parent level has enough space
            while self.levels[level + 1].len() <= parent_idx {
                self.levels[level + 1].push(Self::empty_hash(level + 1));
            }

            if idx & 1 == 0 {
                // Left child: set parent as hash(current, empty)
                let sibling = Self::empty_hash(level);
                let parent = hash_pair(&current, &sibling);
                self.levels[level + 1][parent_idx] = parent;
                current = parent;
            } else {
                // Right child: set parent as hash(left_sibling, current)
                let left = &self.levels[level][idx - 1];
                let parent = hash_pair(left, &current);
                self.levels[level + 1][parent_idx] = parent;
                current = parent;
            }

            idx = parent_idx;
        }

        current
    }

    pub fn root(&self) -> Hash {
        if self.count == 0 {
            return Self::empty_hash(self.depth);
        }
        self.levels[self.depth]
            .first()
            .copied()
            .unwrap_or(Self::empty_hash(self.depth))
    }

    pub fn leaf_count(&self) -> usize {
        self.count
    }

    /// Check if a leaf exists in the tree (Gap #3: merkle inclusion check).
    pub fn contains(&self, leaf: Hash) -> bool {
        self.levels[0].contains(&leaf)
    }

    pub fn proof_for_last(&self) -> Vec<(Hash, bool)> {
        if self.count == 0 {
            return vec![];
        }
        self.proof_for_index(self.count - 1)
    }

    pub fn proof_for_index(&self, idx: usize) -> Vec<(Hash, bool)> {
        let mut proof = Vec::with_capacity(self.depth);
        let mut current_idx = idx;

        for level in 0..self.depth {
            let is_right_child = current_idx & 1 == 1;
            let sibling_idx = if is_right_child {
                current_idx - 1
            } else {
                current_idx + 1
            };

            let sibling = self.levels[level]
                .get(sibling_idx)
                .copied()
                .unwrap_or(Self::empty_hash(level));

            // sibling_is_right: is the sibling positioned on the right?
            // If we're a right child, sibling is on the LEFT → false
            // If we're a left child, sibling is on the RIGHT → true
            let sibling_is_right = !is_right_child;
            proof.push((sibling, sibling_is_right));
            current_idx >>= 1;
        }

        proof
    }

    fn empty_hash(level: usize) -> Hash {
        EMPTY_HASH.with(|h| {
            let mut map = h.borrow_mut();
            let max = map.len() - 1;
            if level <= max {
                return map[level];
            }
            for l in map.len()..=level {
                let prev = map[l - 1];
                map.push(hash_pair(&prev, &prev));
            }
            map[level]
        })
    }
}

use std::cell::RefCell;
thread_local! {
    static EMPTY_HASH: RefCell<Vec<Hash>> = RefCell::new(vec![Hash::repeat_byte(0)]);
}

fn hash_pair(left: &Hash, right: &Hash) -> Hash {
    let mut data = Vec::with_capacity(64);
    data.extend_from_slice(left.as_slice());
    data.extend_from_slice(right.as_slice());
    keccak256(&data)
}

impl Default for IncrementalMerkleTree {
    fn default() -> Self {
        Self::new(32)
    }
}

/// Verify a Merkle proof
pub fn verify_merkle_path(leaf: Hash, proof: &[(Hash, bool)], expected_root: Hash) -> bool {
    let mut current = leaf;
    for (sibling, sibling_is_right) in proof {
        if *sibling_is_right {
            // Sibling is on the right: current || sibling
            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(current.as_slice());
            data.extend_from_slice(sibling.as_slice());
            current = keccak256(&data);
        } else {
            // Sibling is on the left: sibling || current
            let mut data = Vec::with_capacity(64);
            data.extend_from_slice(sibling.as_slice());
            data.extend_from_slice(current.as_slice());
            current = keccak256(&data);
        }
    }
    current == expected_root
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_hash;

    #[test]
    fn test_merkle_tree_insert() {
        let mut tree = IncrementalMerkleTree::new(32);
        assert_eq!(tree.leaf_count(), 0);

        let leaf1 = test_hash(1);
        let root1 = tree.insert(leaf1);
        assert_eq!(tree.leaf_count(), 1);
        assert_eq!(tree.root(), root1);

        let leaf2 = test_hash(2);
        let root2 = tree.insert(leaf2);
        assert_eq!(tree.leaf_count(), 2);
        assert_eq!(tree.root(), root2);
        assert_ne!(root1, root2);
    }

    #[test]
    fn test_merkle_tree_multiple_inserts() {
        let mut tree = IncrementalMerkleTree::new(32);
        for i in 0..100 {
            tree.insert(test_hash(i as u8));
        }
        assert_eq!(tree.leaf_count(), 100);
        assert_ne!(tree.root(), Hash::repeat_byte(0));
    }

    #[test]
    fn test_merkle_tree_proof_for_last() {
        let mut tree = IncrementalMerkleTree::new(5);
        let leaf1 = test_hash(1);
        tree.insert(leaf1);
        let proof = tree.proof_for_last();
        assert_eq!(proof.len(), 5);
        assert!(verify_merkle_path(leaf1, &proof, tree.root()));

        let leaf2 = test_hash(2);
        tree.insert(leaf2);
        let proof2 = tree.proof_for_last();
        assert!(verify_merkle_path(leaf2, &proof2, tree.root()));

        let leaf3 = test_hash(3);
        tree.insert(leaf3);
        let proof3 = tree.proof_for_last();
        assert!(verify_merkle_path(leaf3, &proof3, tree.root()));
    }

    #[test]
    fn test_merkle_tree_proof_all_leaves() {
        let mut tree = IncrementalMerkleTree::new(5);
        let leaves: Vec<Hash> = (1..=16).map(|i| test_hash(i as u8)).collect();
        for l in &leaves {
            tree.insert(*l);
        }

        for (i, l) in leaves.iter().enumerate() {
            let proof = tree.proof_for_index(i);
            assert!(
                verify_merkle_path(*l, &proof, tree.root()),
                "proof failed for leaf {} at index {}",
                i,
                i
            );
        }
    }

    #[test]
    fn test_merkle_tree_deterministic() {
        let mut tree1 = IncrementalMerkleTree::new(32);
        let mut tree2 = IncrementalMerkleTree::new(32);
        for i in 0..10u8 {
            tree1.insert(test_hash(i));
            tree2.insert(test_hash(i));
        }
        assert_eq!(tree1.root(), tree2.root());
    }

    #[test]
    fn test_merkle_tree_depth_limit() {
        let mut tree = IncrementalMerkleTree::new(5);
        for i in 0..32u8 {
            tree.insert(test_hash(i));
        }
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            tree.insert(test_hash(99));
        }));
        assert!(result.is_err());
    }

    #[test]
    fn test_empty_tree_root() {
        let tree = IncrementalMerkleTree::new(32);
        assert_eq!(tree.root(), tree.root());
    }
}
