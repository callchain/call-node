//! Incremental Merkle Tree using Poseidon hash for BN254.
//!
//! This is a ZK-circuit-friendly Merkle tree that uses Poseidon hashing
//! instead of keccak256. The API mirrors `IncrementalMerkleTree` from
//! `merkle.rs` but operates with `[u8; 32]` bytes internally converted to
//! `ark_bn254::Fr` for Poseidon hashing.
//!
//! All operations are gated behind the `poseidon` feature.

use std::cell::RefCell;
use crate::poseidon::poseidon_hash_pair;

// ============================================================================
// PoseidonMerkleTree
// ============================================================================

/// Incremental Poseidon Merkle Tree (fixed depth, append-only).
///
/// Stores all tree nodes explicitly for efficient proof generation.
/// Uses Poseidon hash over BN254 Fr field, suitable for ZK circuit verification.
#[derive(Debug, Clone)]
pub struct PoseidonMerkleTree {
    depth: usize,
    count: usize,
    /// All levels of the tree: levels[0] = leaves, levels[d] = root.
    levels: Vec<Vec<[u8; 32]>>,
}

impl PoseidonMerkleTree {
    /// Creates a new incremental Merkle tree with the given depth.
    ///
    /// Default depth is 32, supporting up to 2^32 leaves.
    pub fn new(depth: usize) -> Self {
        Self {
            depth,
            count: 0,
            levels: (0..=depth).map(|_| Vec::new()).collect(),
        }
    }

    /// Inserts a leaf into the tree and returns the new root.
    ///
    /// # Panics
    /// Panics if the tree is full (2^depth leaves already inserted).
    pub fn insert(&mut self, leaf: &[u8; 32]) -> [u8; 32] {
        if self.count >= (1usize << self.depth) {
            panic!("Poseidon Merkle tree at capacity");
        }
        self.count += 1;

        // Set leaf
        self.levels[0].push(*leaf);

        // Propagate up
        let mut idx = self.count - 1;
        let mut current = *leaf;

        for level in 0..self.depth {
            let parent_idx = idx >> 1;

            // Ensure parent level has enough space
            while self.levels[level + 1].len() <= parent_idx {
                self.levels[level + 1].push(Self::empty_hash(level + 1));
            }

            if idx & 1 == 0 {
                // Left child: parent = hash(current, empty)
                let sibling = Self::empty_hash(level);
                let parent = poseidon_hash_pair(&current, &sibling);
                self.levels[level + 1][parent_idx] = parent;
                current = parent;
            } else {
                // Right child: parent = hash(left_sibling, current)
                let left = &self.levels[level][idx - 1];
                let parent = poseidon_hash_pair(left, &current);
                self.levels[level + 1][parent_idx] = parent;
                current = parent;
            }

            idx = parent_idx;
        }

        current
    }

    /// Returns the current Merkle root.
    ///
    /// For an empty tree, returns the deterministic empty root.
    pub fn root(&self) -> [u8; 32] {
        if self.count == 0 {
            return Self::empty_hash(self.depth);
        }
        self.levels[self.depth]
            .first()
            .copied()
            .unwrap_or(Self::empty_hash(self.depth))
    }

    /// Returns the number of leaves in the tree.
    pub fn leaf_count(&self) -> usize {
        self.count
    }

    /// Check if a leaf exists in the tree.
    pub fn contains(&self, leaf: &[u8; 32]) -> bool {
        self.levels[0].contains(leaf)
    }

    /// Returns a Merkle proof for the most recently inserted leaf.
    pub fn proof_for_last(&self) -> Vec<([u8; 32], bool)> {
        if self.count == 0 {
            return vec![];
        }
        self.proof_for_index(self.count - 1).unwrap()
    }

    /// Returns a Merkle proof for the leaf at the given index.
    ///
    /// Each entry in the returned vector is `(sibling_hash, sibling_is_right)`,
    /// where `sibling_is_right` indicates whether the sibling is positioned
    /// to the right of the current node.
    ///
    /// Returns `None` if the index is out of bounds.
    pub fn proof_for_index(&self, index: usize) -> Option<Vec<([u8; 32], bool)>> {
        if index >= self.count {
            return None;
        }

        let mut proof = Vec::with_capacity(self.depth);
        let mut current_idx = index;

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

            // If we're a right child, sibling is on the LEFT -> sibling_is_right = false
            // If we're a left child, sibling is on the RIGHT -> sibling_is_right = true
            let sibling_is_right = !is_right_child;
            proof.push((sibling, sibling_is_right));
            current_idx >>= 1;
        }

        Some(proof)
    }

    /// Returns the empty hash for the given level (deterministic).
    ///
    /// Level 0: hash(0, 0)
    /// Level N: hash(empty_hash(N-1), empty_hash(N-1))
    fn empty_hash(level: usize) -> [u8; 32] {
        EMPTY_HASH.with(|h| {
            let mut map = h.borrow_mut();
            let max = map.len() - 1;
            if level <= max {
                return map[level];
            }
            for l in map.len()..=level {
                let prev = map[l - 1];
                map.push(poseidon_hash_pair(&prev, &prev));
            }
            map[level]
        })
    }
}

impl Default for PoseidonMerkleTree {
    fn default() -> Self {
        Self::new(32)
    }
}

// ============================================================================
// Standalone verifier
// ============================================================================

/// Verify a Poseidon Merkle proof.
///
/// Recomputes the root from the leaf and proof path, comparing against
/// `expected_root`. Returns `true` if the proof is valid.
pub fn verify_poseidon_proof(
    root: &[u8; 32],
    leaf: &[u8; 32],
    proof: &[([u8; 32], bool)],
) -> bool {
    let mut current = *leaf;
    for (sibling, sibling_is_right) in proof {
        if *sibling_is_right {
            // Sibling is on the right: current || sibling
            current = crate::poseidon::poseidon_hash_pair(&current, sibling);
        } else {
            // Sibling is on the left: sibling || current
            current = crate::poseidon::poseidon_hash_pair(sibling, &current);
        }
    }
    current == *root
}

// ============================================================================
// Internal helpers
// ============================================================================

// Thread-local cache of empty hashes per level.
thread_local! {
    static EMPTY_HASH: RefCell<Vec<[u8; 32]>> = RefCell::new(vec![[0u8; 32]]);
}

// ============================================================================
// Tests
// ============================================================================

#[cfg(all(test, feature = "poseidon"))]
mod tests {
    use super::*;

    fn test_bytes(n: u8) -> [u8; 32] {
        let mut out = [0u8; 32];
        out[0] = n;
        out
    }

    #[test]
    fn test_poseidon_merkle_insert_single() {
        let mut tree = PoseidonMerkleTree::new(32);
        assert_eq!(tree.leaf_count(), 0);

        let leaf = test_bytes(1);
        let root1 = tree.insert(&leaf);
        assert_eq!(tree.leaf_count(), 1);
        assert_eq!(tree.root(), root1);

        // Root changed from empty root
        let empty_root = PoseidonMerkleTree::empty_hash(32);
        assert_ne!(root1, empty_root);
    }

    #[test]
    fn test_poseidon_merkle_insert_multiple() {
        let mut tree = PoseidonMerkleTree::new(32);
        for i in 0..10u8 {
            tree.insert(&test_bytes(i));
        }
        assert_eq!(tree.leaf_count(), 10);

        let root = tree.root();
        // Root is non-zero
        assert!(root.iter().any(|&b| b != 0));
    }

    #[test]
    fn test_poseidon_merkle_proof_last() {
        let mut tree = PoseidonMerkleTree::new(5);

        let leaf1 = test_bytes(1);
        tree.insert(&leaf1);
        let proof = tree.proof_for_last();
        assert_eq!(proof.len(), 5);
        assert!(verify_poseidon_proof(&tree.root(), &leaf1, &proof));

        let leaf2 = test_bytes(2);
        tree.insert(&leaf2);
        let proof2 = tree.proof_for_last();
        assert!(verify_poseidon_proof(&tree.root(), &leaf2, &proof2));

        let leaf3 = test_bytes(3);
        tree.insert(&leaf3);
        let proof3 = tree.proof_for_last();
        assert!(verify_poseidon_proof(&tree.root(), &leaf3, &proof3));
    }

    #[test]
    fn test_poseidon_merkle_proof_index() {
        let mut tree = PoseidonMerkleTree::new(5);
        let leaves: Vec<[u8; 32]> = (0..16u8).map(test_bytes).collect();
        for l in &leaves {
            tree.insert(l);
        }

        // Verify proof for every leaf by index
        for (i, leaf) in leaves.iter().enumerate() {
            let proof = tree.proof_for_index(i).unwrap();
            assert!(
                verify_poseidon_proof(&tree.root(), leaf, &proof),
                "proof failed for leaf at index {i}"
            );
        }

        // Out-of-b index returns None
        assert!(tree.proof_for_index(16).is_none());
        assert!(tree.proof_for_index(100).is_none());
    }

    #[test]
    fn test_poseidon_merkle_verify_valid() {
        let mut tree = PoseidonMerkleTree::new(32);
        let leaf = test_bytes(42);
        tree.insert(&leaf);
        let proof = tree.proof_for_last();

        assert!(verify_poseidon_proof(&tree.root(), &leaf, &proof));
    }

    #[test]
    fn test_poseidon_merkle_verify_invalid_path() {
        let mut tree = PoseidonMerkleTree::new(32);
        let leaf = test_bytes(42);
        tree.insert(&leaf);
        let proof = tree.proof_for_last();

        // Corrupt the first sibling
        let mut bad_proof = proof.clone();
        let mut bad_sibling = bad_proof[0].0;
        bad_sibling[0] ^= 0xFF;
        bad_proof[0].0 = bad_sibling;

        assert!(!verify_poseidon_proof(&tree.root(), &leaf, &bad_proof));
    }

    #[test]
    fn test_poseidon_merkle_verify_wrong_index() {
        let mut tree = PoseidonMerkleTree::new(32);
        let leaf1 = test_bytes(1);
        let leaf2 = test_bytes(2);
        tree.insert(&leaf1);
        tree.insert(&leaf2);

        // Get proof for leaf1 (index 0), but try to verify leaf2 with it
        let proof_leaf1 = tree.proof_for_index(0).unwrap();
        assert!(!verify_poseidon_proof(&tree.root(), &leaf2, &proof_leaf1));

        // Get proof for leaf2 (index 1), but try to verify leaf1 with it
        let proof_leaf2 = tree.proof_for_index(1).unwrap();
        assert!(!verify_poseidon_proof(&tree.root(), &leaf1, &proof_leaf2));
    }

    #[test]
    fn test_poseidon_merkle_depth_32() {
        let mut tree = PoseidonMerkleTree::new(32);
        for i in 0..100u8 {
            tree.insert(&test_bytes(i));
        }
        assert_eq!(tree.leaf_count(), 100);

        // All proofs should be exactly 32 elements long
        for i in 0..100 {
            let proof = tree.proof_for_index(i).unwrap();
            assert_eq!(proof.len(), 32);
            let leaf = test_bytes(i as u8);
            assert!(
                verify_poseidon_proof(&tree.root(), &leaf, &proof),
                "proof failed at depth 32 for index {i}"
            );
        }
    }

    #[test]
    fn test_poseidon_merkle_empty_tree() {
        let tree = PoseidonMerkleTree::new(32);
        assert_eq!(tree.leaf_count(), 0);

        // Empty tree root is consistent
        let root1 = tree.root();
        let root2 = tree.root();
        assert_eq!(root1, root2);

        // Empty tree root is non-trivial (hash of zeros, not all zeros)
        assert!(root1.iter().any(|&b| b != 0));

        // proof_for_last returns empty vec for empty tree
        assert!(tree.proof_for_last().is_empty());
    }

    #[test]
    fn test_poseidon_merkle_root_deterministic() {
        let mut tree1 = PoseidonMerkleTree::new(32);
        let mut tree2 = PoseidonMerkleTree::new(32);

        for i in 0..50u8 {
            tree1.insert(&test_bytes(i));
            tree2.insert(&test_bytes(i));
        }

        assert_eq!(tree1.root(), tree2.root());

        // Independent insertion produces different root
        let mut tree3 = PoseidonMerkleTree::new(32);
        for i in 0..50u8 {
            tree3.insert(&test_bytes(i));
        }
        tree3.insert(&test_bytes(99));
        assert_ne!(tree1.root(), tree3.root());
    }
}
