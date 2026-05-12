//! Nullifier set for double-spend detection (per spec §3.8.2)
//!
//! Nullifiers are hashed unique identifiers for spent notes.
//! The nullifier set supports efficient insert, check, and BitSet compression.

use crate::Nullifier;
use std::collections::HashSet;

/// Nullifier set: tracks spent nullifiers with BitSet compression
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct NullifierSet {
    /// Primary set of spent nullifiers
    spent: HashSet<Nullifier>,
    /// BitSet compression: 1 bit per nullifier bucket
    /// Each bucket covers 64 nullifiers (by hash prefix)
    #[serde(skip, default)]
    bitset: Vec<u64>,
}

impl NullifierSet {
    /// Create a new empty nullifier set
    pub fn new() -> Self {
        Self {
            spent: HashSet::new(),
            bitset: Vec::new(),
        }
    }

    /// Insert a nullifier (mark as spent)
    pub fn insert(&mut self, nullifier: &Nullifier) -> bool {
        let inserted = self.spent.insert(nullifier.clone());
        if inserted {
            // Update bitset
            let bucket = self.nullifier_bucket(nullifier);
            let bit_index = self.nullifier_bit(nullifier);
            self.ensure_bucket(bucket);
            self.bitset[bucket] |= 1u64 << bit_index;
        }
        inserted
    }

    /// Check if a nullifier has been spent
    pub fn is_spent(&self, nullifier: &Nullifier) -> bool {
        self.spent.contains(nullifier)
    }

    /// Quick check using BitSet (may have false positives, no false negatives)
    pub fn maybe_spent(&self, nullifier: &Nullifier) -> bool {
        let bucket = self.nullifier_bucket(nullifier);
        let bit_index = self.nullifier_bit(nullifier);
        if bucket >= self.bitset.len() {
            return false;
        }
        (self.bitset[bucket] & (1u64 << bit_index)) != 0
    }

    /// Get the number of spent nullifiers
    pub fn len(&self) -> usize {
        self.spent.len()
    }

    /// Get all spent nullifiers
    pub fn spent_nullifiers(&self) -> &HashSet<Nullifier> {
        &self.spent
    }

    /// Check if the set is empty
    pub fn is_empty(&self) -> bool {
        self.spent.is_empty()
    }

    /// Clear all nullifiers (for testing)
    pub fn clear(&mut self) {
        self.spent.clear();
        self.bitset.clear();
    }

    /// Get BitSet memory usage in bytes (compression metric)
    pub fn bitset_bytes(&self) -> usize {
        self.bitset.len() * 8
    }

    /// Compute bucket index from nullifier hash
    fn nullifier_bucket(&self, nullifier: &Nullifier) -> usize {
        // Use first 8 bytes of hash as bucket index
        let prefix = u64::from_le_bytes(
            nullifier.as_hash().as_slice()[0..8]
                .try_into()
                .expect("invariant: 8-byte prefix"),
        );
        (prefix % 64) as usize
    }

    /// Compute bit index within bucket
    fn nullifier_bit(&self, nullifier: &Nullifier) -> u32 {
        let prefix = u64::from_le_bytes(
            nullifier.as_hash().as_slice()[8..16]
                .try_into()
                .expect("invariant: 8-byte prefix"),
        );
        (prefix % 64) as u32
    }

    /// Ensure the bucket exists
    fn ensure_bucket(&mut self, bucket: usize) {
        while self.bitset.len() <= bucket {
            self.bitset.push(0);
        }
    }
}

impl Default for NullifierSet {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_hash;

    #[test]
    fn test_nullifier_insert_and_check() {
        let mut set = NullifierSet::new();
        let nf = Nullifier::new(test_hash(1));

        assert!(!set.is_spent(&nf));
        assert!(set.insert(&nf));
        assert!(set.is_spent(&nf));
        assert!(set.maybe_spent(&nf));
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn test_nullifier_double_spend_detection() {
        let mut set = NullifierSet::new();
        let nf = Nullifier::new(test_hash(1));

        assert!(set.insert(&nf));
        // Second insert returns false (already present)
        assert!(!set.insert(&nf));
        assert!(set.is_spent(&nf));
    }

    #[test]
    fn test_nullifier_bitset_compression() {
        let mut set = NullifierSet::new();

        // Insert many nullifiers
        for i in 0..200u8 {
            let nf = Nullifier::new(test_hash(i));
            set.insert(&nf);
        }

        // BitSet should be compact
        assert!(set.bitset_bytes() < set.len() * 32);

        // All should be found
        for i in 0..200u8 {
            let nf = Nullifier::new(test_hash(i));
            assert!(set.is_spent(&nf));
        }

        // Unspent should not be found
        let unspent = Nullifier::new(test_hash(200));
        assert!(!set.is_spent(&unspent));
    }

    #[test]
    fn test_nullifer_bitset_no_false_negatives() {
        let mut set = NullifierSet::new();
        let nf = Nullifier::new(test_hash(42));
        set.insert(&nf);

        // maybe_spent must return true for actually spent nullifiers
        assert!(set.maybe_spent(&nf));
    }

    #[test]
    fn test_nullifier_set_clear() {
        let mut set = NullifierSet::new();
        set.insert(&Nullifier::new(test_hash(1)));
        set.insert(&Nullifier::new(test_hash(2)));
        set.clear();
        assert!(set.is_empty());
        assert!(!set.is_spent(&Nullifier::new(test_hash(1))));
    }
}
