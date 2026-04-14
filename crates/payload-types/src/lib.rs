//! T9.2 — Payload Types (per spec §13.5.1)
//!
//! Block limits, payload attributes, and configuration types
//! for the payload builder and consensus integration.

use serde::{Deserialize, Serialize};

// ── Block Limits (per spec §13.5.1) ──────────────────────────────────

/// Per-block limits enforced by the payload builder
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
pub struct BlockLimits {
    /// Maximum block size in bytes (5 MB)
    pub max_block_size: usize,
    /// Maximum transactions per block (10,000)
    pub max_transactions: usize,
    /// Maximum shielded transactions per block (50)
    pub max_shielded_per_block: usize,
    /// Maximum instructions per transaction (1,000)
    pub max_instructions_per_tx: usize,
    /// Maximum transaction size in bytes (256 KB)
    pub max_tx_size: usize,
    /// Maximum batch payments per transaction (5,000)
    pub max_batch_payments: usize,
    /// Maximum EVM gas per block (30,000,000)
    pub max_evm_gas_per_block: u64,
}

impl Default for BlockLimits {
    fn default() -> Self {
        Self {
            max_block_size: 5 * 1024 * 1024,     // 5 MB
            max_transactions: 10_000,
            max_shielded_per_block: 50,
            max_instructions_per_tx: 1_000,
            max_tx_size: 256 * 1024,              // 256 KB
            max_batch_payments: 5_000,
            max_evm_gas_per_block: 30_000_000,
        }
    }
}

// ── Payload Attributes ───────────────────────────────────────────────

/// Attributes for building a payload from the mempool
#[derive(Debug, Clone)]
pub struct PayloadAttributes {
    /// Block height
    pub height: u64,
    /// Parent block hash
    pub parent_hash: call_primitives::BlockHash,
    /// Block timestamp in milliseconds
    pub timestamp_millis: u64,
    /// Validator ID of the proposer
    pub proposer: call_primitives::ValidatorId,
    /// Block limits to enforce
    pub limits: BlockLimits,
}

impl PayloadAttributes {
    /// Create new payload attributes
    pub fn new(
        height: u64,
        parent_hash: call_primitives::BlockHash,
        timestamp_millis: u64,
        proposer: call_primitives::ValidatorId,
    ) -> Self {
        Self {
            height,
            parent_hash,
            timestamp_millis,
            proposer,
            limits: BlockLimits::default(),
        }
    }

    /// Set custom block limits
    pub fn with_limits(mut self, limits: BlockLimits) -> Self {
        self.limits = limits;
        self
    }

    /// Estimate approximate block size from tx counts
    pub fn estimate_size(&self, protocol_count: usize, evm_count: usize) -> usize {
        // Rough estimate: protocol txs ~200 bytes, EVM txs ~500 bytes average
        protocol_count.saturating_mul(200) + evm_count.saturating_mul(500)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::BlockHash;

    #[test]
    fn test_block_limits_defaults() {
        let limits = BlockLimits::default();
        assert_eq!(limits.max_block_size, 5 * 1024 * 1024);
        assert_eq!(limits.max_transactions, 10_000);
        assert_eq!(limits.max_shielded_per_block, 50);
        assert_eq!(limits.max_instructions_per_tx, 1_000);
        assert_eq!(limits.max_tx_size, 256 * 1024);
        assert_eq!(limits.max_batch_payments, 5_000);
        assert_eq!(limits.max_evm_gas_per_block, 30_000_000);
    }

    #[test]
    fn test_payload_attributes_defaults() {
        let attrs = PayloadAttributes::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
        );
        assert_eq!(attrs.height, 1);
        assert_eq!(attrs.parent_hash, BlockHash::ZERO);
        assert_eq!(attrs.timestamp_millis, 1000);
        assert_eq!(attrs.proposer, 1);
        // Default limits should be set
        assert_eq!(attrs.limits.max_transactions, 10_000);
    }

    #[test]
    fn test_payload_attributes_with_limits() {
        let custom = BlockLimits {
            max_transactions: 100,
            ..Default::default()
        };
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1)
            .with_limits(custom);
        assert_eq!(attrs.limits.max_transactions, 100);
        assert_eq!(attrs.limits.max_block_size, 5 * 1024 * 1024); // unchanged
    }

    #[test]
    fn test_payload_attributes_estimate_size() {
        let attrs = PayloadAttributes::new(1, BlockHash::ZERO, 1000, 1);
        let size = attrs.estimate_size(100, 50);
        // 100 * 200 + 50 * 500 = 20_000 + 25_000 = 45_000
        assert_eq!(size, 45_000);
    }

    #[test]
    fn test_block_limits_serialization() {
        let limits = BlockLimits::default();
        let json = serde_json::to_string(&limits).unwrap();
        let parsed: BlockLimits = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.max_transactions, limits.max_transactions);
        assert_eq!(parsed.max_evm_gas_per_block, limits.max_evm_gas_per_block);
    }
}
