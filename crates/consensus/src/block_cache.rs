//! Block cache for digest-to-block mapping.
//!
//! The commonware-consensus Simplex BFT engine operates on digests only.
//! This cache maps `ConsensusDigest` → `Block` so the `Automaton` can look up
//! full blocks during `propose` and `verify`, and the `Relay` can broadcast
//! full block bytes.

use crate::block::Block;
use crate::digest::ConsensusDigest;
use std::collections::HashMap;

/// LRU-ish block cache keyed by consensus digest.
pub struct BlockCache {
    blocks: HashMap<ConsensusDigest, Block>,
    order: Vec<ConsensusDigest>,
    max_size: usize,
}

impl BlockCache {
    /// Create a new block cache with the given capacity.
    pub fn new(max_size: usize) -> Self {
        Self {
            blocks: HashMap::with_capacity(max_size),
            order: Vec::with_capacity(max_size),
            max_size,
        }
    }

    /// Insert a block into the cache.
    pub fn insert(&mut self, digest: ConsensusDigest, block: Block) {
        if self.blocks.contains_key(&digest) {
            return;
        }
        if self.blocks.len() >= self.max_size {
            if let Some(oldest) = self.order.first().copied() {
                self.blocks.remove(&oldest);
                self.order.remove(0);
            }
        }
        self.order.push(digest);
        self.blocks.insert(digest, block);
    }

    /// Look up a block by digest.
    pub fn get(&self, digest: &ConsensusDigest) -> Option<&Block> {
        self.blocks.get(digest)
    }

    /// Remove a block from the cache (e.g. after finalization).
    pub fn remove(&mut self, digest: &ConsensusDigest) -> Option<Block> {
        self.order.retain(|d| d != digest);
        self.blocks.remove(digest)
    }

    /// Current number of cached blocks.
    pub fn len(&self) -> usize {
        self.blocks.len()
    }

    /// Whether the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.blocks.is_empty()
    }

    /// Clear all blocks from the cache.
    pub fn clear(&mut self) {
        self.blocks.clear();
        self.order.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::BlockHash;

    fn dummy_block(height: u64) -> Block {
        Block::new(
            height,
            BlockHash::ZERO,
            0,
            0,
            vec![],
            vec![],
            vec![],
            vec![],
        )
    }

    #[test]
    fn test_cache_insert_and_get() {
        let mut cache = BlockCache::new(10);
        let digest = ConsensusDigest([1u8; 32]);
        let block = dummy_block(1);
        cache.insert(digest, block.clone());
        assert!(cache.get(&digest).is_some());
    }

    #[test]
    fn test_cache_eviction() {
        let mut cache = BlockCache::new(2);
        let d1 = ConsensusDigest([1u8; 32]);
        let d2 = ConsensusDigest([2u8; 32]);
        let d3 = ConsensusDigest([3u8; 32]);
        cache.insert(d1, dummy_block(1));
        cache.insert(d2, dummy_block(2));
        cache.insert(d3, dummy_block(3));
        assert!(cache.get(&d1).is_none());
        assert!(cache.get(&d2).is_some());
        assert!(cache.get(&d3).is_some());
    }

    #[test]
    fn test_cache_remove() {
        let mut cache = BlockCache::new(10);
        let digest = ConsensusDigest([1u8; 32]);
        cache.insert(digest, dummy_block(1));
        assert_eq!(cache.len(), 1);
        cache.remove(&digest);
        assert!(cache.is_empty());
    }
}
