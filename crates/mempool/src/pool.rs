//! T8.1 — Transaction Pool (per spec §17)
//!
//! EVM-only mempool with capacity limits, eviction, and anti-spam.

use call_primitives::{Address, TxHash};
use call_crypto::keccak256;
use call_evm::EvmTransaction;
use std::collections::{HashMap, HashSet};

use crate::priority::{MempoolEntry, PoolKind, PoolLimits, PriorityPool};

// ── Mempool Config ───────────────────────────────────────────────────

/// Mempool configuration for anti-spam and validation
#[derive(Debug, Clone)]
pub struct MempoolConfig {
    /// Minimum fee in CALL equivalent (anti-spam)
    pub min_fee: u128,
    /// Minimum gas price (wei per gas unit)
    pub min_gas_price: u128,
    /// Maximum gas limit per transaction
    pub max_gas_limit: u64,
}

impl Default for MempoolConfig {
    fn default() -> Self {
        Self {
            min_fee: 1_000,           // 1000 wei minimum
            min_gas_price: 1,         // 1 wei per gas unit
            max_gas_limit: 10_000_000, // 10M gas limit
        }
    }
}

// ── Mempool Error ────────────────────────────────────────────────────

/// Mempool insertion/rejection errors
#[derive(Debug, thiserror::Error)]
pub enum MempoolError {
    #[error("duplicate transaction: {0}")]
    Duplicate(TxHash),
    #[error("pool capacity reached: {pool} full ({count}/{max})")]
    PoolFull { pool: String, count: usize, max: usize },
    #[error("per-address limit reached: {address}, {count}/{max}")]
    AddressLimitReached { address: Address, count: usize, max: usize },
    #[error("fee too low: {fee} < {min}")]
    FeeTooLow { fee: u128, min: u128 },
    #[error("gas limit exceeded: {gas} > {max}")]
    GasLimitExceeded { gas: u64, max: u64 },
    #[error("invalid nonce: expected {expected}, got {got}")]
    InvalidNonce { expected: u64, got: u64 },
    #[error("transaction not found: {0}")]
    NotFound(TxHash),
    #[error("mempool error: {0}")]
    Generic(String),
}

// ── Mempool (per spec §17.1) ─────────────────────────────────────────

/// The main mempool structure managing EVM transactions.
pub struct Mempool {
    /// EVM transactions (100K limit)
    pub evm_pool: PriorityPool,
    /// Known transaction hashes for deduplication
    pub known_txs: HashSet<TxHash>,
    /// Pool limits
    pub limits: PoolLimits,
    /// Anti-spam config
    pub config: MempoolConfig,
    /// Current block height (for lifetime expiry)
    pub current_block: u64,
    /// Base fee (for priority scoring)
    pub base_fee: u128,
    /// Gap 2 — Expected next nonce per sender address
    pub expected_nonces: HashMap<Address, u64>,
}

impl Mempool {
    /// Create a new mempool with default limits and config
    pub fn new() -> Self {
        Self {
            evm_pool: PriorityPool::new(),
            known_txs: HashSet::new(),
            limits: PoolLimits::default(),
            config: MempoolConfig::default(),
            current_block: 0,
            base_fee: 1,
            expected_nonces: HashMap::new(),
        }
    }

    /// Update current block height (for lifetime expiry)
    pub fn set_current_block(&mut self, block: u64) {
        self.current_block = block;
    }

    /// Update base fee (for priority scoring)
    pub fn set_base_fee(&mut self, fee: u128) {
        self.base_fee = fee;
    }

    // ── EVM Transactions ─────────────────────────────────────────────

    /// Insert an EVM transaction into the mempool
    pub fn insert_evm_tx(
        &mut self,
        tx: EvmTransaction,
    ) -> Result<TxHash, MempoolError> {
        // Serialize full EvmTransaction for block execution (decode_evm_tx supports JSON fallback)
        let data = serde_json::to_vec(&tx).map_err(|e| MempoolError::Generic(format!("serialize: {e}")))?;
        let hash = keccak256(&data);

        // Deduplication check
        if self.known_txs.contains(&hash) {
            return Err(MempoolError::Duplicate(hash));
        }

        // Validate gas price
        if tx.gas_price < self.config.min_gas_price {
            return Err(MempoolError::FeeTooLow {
                fee: tx.gas_price,
                min: self.config.min_gas_price,
            });
        }

        // Check per-address limit
        let addr_count = self.evm_pool.count_per_address(&tx.caller);
        if addr_count >= self.limits.max_per_address {
            return Err(MempoolError::AddressLimitReached {
                address: tx.caller,
                count: addr_count,
                max: self.limits.max_per_address,
            });
        }

        // Check pool capacity
        if self.evm_pool.len() >= self.limits.max_evm_txs {
            return Err(MempoolError::PoolFull {
                pool: "evm".into(),
                count: self.evm_pool.len(),
                max: self.limits.max_evm_txs,
            });
        }

        let entry = MempoolEntry::new(
            hash,
            tx.gas_price,
            tx.caller,
            tx.nonce,
            PoolKind::Evm,
            data,
            self.current_block,
        );

        self.evm_pool.insert(entry);
        self.known_txs.insert(hash);

        Ok(hash)
    }

    // ── Transaction Selection (for block building) ─────────────────

    /// Select the best transactions for the next block.
    ///
    /// Returns EVM transactions sorted by gas price descending.
    pub fn select_transactions(&mut self) -> MempoolSelection {
        let evm_txs = self.evm_pool.drain_sorted();

        // NOTE: Pools are NOT cleared here. Transactions remain in the mempool
        // until they are confirmed in a committed block via `confirm_transactions`.
        // This prevents nonce-validation races where RPC sees an empty mempool
        // between selection and block commitment.
        //
        // `known_txs` is also kept so duplicate submissions are still rejected
        // while a tx is awaiting block inclusion.

        MempoolSelection {
            evm_txs,
        }
    }

    // ── Maintenance ────────────────────────────────────────────────

    /// Remove expired transactions from the pool.
    ///
    /// Called each block to enforce the 72-block lifetime limit.
    pub fn prune_expired(&mut self) -> usize {
        self.evm_pool.remove_expired(
            self.current_block,
            self.limits.lifetime_blocks,
        )
    }

    /// Remove transactions confirmed in a block (by hash).
    /// Also increments expected nonces for included senders.
    pub fn confirm_transactions(&mut self, hashes: &[TxHash]) {
        for hash in hashes {
            if let Some(entry) = self.evm_pool.remove(hash) {
                let next = entry.nonce.saturating_add(1);
                self.expected_nonces.insert(entry.sender, next);
            }
            self.known_txs.remove(hash);
        }
    }

    /// Increment expected nonce for a sender after their tx is included in a block.
    pub fn increment_nonce(&mut self, sender: Address) {
        let current = self.expected_nonces.get(&sender).copied().unwrap_or(0);
        self.expected_nonces.insert(sender, current.saturating_add(1));
    }

    /// Get the expected next EVM nonce for a sender, accounting for pending mempool txs.
    /// Returns max(committed_nonce, highest_pending_nonce + 1).
    pub fn get_expected_evm_nonce(&self, sender: Address, committed_nonce: u64) -> u64 {
        let pending_nonce = self.evm_pool.get_address_nonce(&sender);
        committed_nonce.max(pending_nonce)
    }

    /// Get total number of pending transactions
    pub fn total_pending(&self) -> usize {
        self.evm_pool.len()
    }

    /// Get pool stats
    pub fn pool_stats(&self) -> PoolStats {
        PoolStats {
            evm_count: self.evm_pool.len(),
            known_tx_count: self.known_txs.len(),
        }
    }
}

// ── Mempool Selection Result ─────────────────────────────────────────

/// Selected transactions for block building
#[derive(Debug)]
pub struct MempoolSelection {
    pub evm_txs: Vec<MempoolEntry>,
}

// ── Pool Stats ───────────────────────────────────────────────────────

/// Current pool statistics
#[derive(Debug)]
pub struct PoolStats {
    pub evm_count: usize,
    pub known_tx_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;
    use call_evm::EvmTransaction;
    use call_primitives::U256;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn make_evm_tx(nonce: u64, gas_price: u128) -> EvmTransaction {
        // Include nonce in data so each test tx has a unique hash
        let data = call_evm::Bytes::from(nonce.to_be_bytes().to_vec());
        EvmTransaction {
            caller: test_addr(1),
            nonce,
            gas_limit: 21_000,
            gas_price,
            to: Some(test_addr(2)),
            value: U256::from(100),
            data,
            chain_id: 1,
        }
    }

    #[test]
    fn test_mempool_insert_evm_tx() {
        let mut mempool = Mempool::new();
        let tx = make_evm_tx(0, 100);

        let hash = mempool.insert_evm_tx(tx);
        assert!(hash.is_ok());
        assert_eq!(mempool.total_pending(), 1);
    }

    #[test]
    fn test_mempool_duplicate_tx_rejected() {
        let mut mempool = Mempool::new();
        let tx = make_evm_tx(0, 100);

        mempool.insert_evm_tx(tx.clone()).unwrap();
        let result = mempool.insert_evm_tx(tx);
        assert!(matches!(result, Err(MempoolError::Duplicate(_))));
    }

    #[test]
    fn test_mempool_fee_too_low() {
        let mut mempool = Mempool::new();
        let tx = make_evm_tx(0, 0); // zero gas price

        let result = mempool.insert_evm_tx(tx);
        assert!(matches!(result, Err(MempoolError::FeeTooLow { .. })));
    }

    #[test]
    fn test_mempool_per_address_limit() {
        let mut mempool = Mempool::new();
        mempool.limits.max_per_address = 3;

        for nonce in 0..3 {
            mempool.insert_evm_tx(make_evm_tx(nonce, 100)).unwrap();
        }

        // 4th tx should fail
        let result = mempool.insert_evm_tx(make_evm_tx(3, 100));
        assert!(matches!(result, Err(MempoolError::AddressLimitReached { .. })));
    }

    #[test]
    fn test_mempool_capacity_limit() {
        let mut mempool = Mempool::new();
        mempool.limits.max_evm_txs = 3;

        // Fill the pool
        for nonce in 0..3 {
            mempool.insert_evm_tx(make_evm_tx(nonce, 100)).unwrap();
        }

        // 4th tx should fail with PoolFull
        let result = mempool.insert_evm_tx(make_evm_tx(99, 200));
        assert!(matches!(result, Err(MempoolError::PoolFull { .. })));
    }

    #[test]
    fn test_mempool_lifetime_expiry() {
        let mut mempool = Mempool::new();
        mempool.limits.lifetime_blocks = 10;

        // Insert tx at block 0
        mempool.current_block = 0;
        mempool.insert_evm_tx(make_evm_tx(0, 100)).unwrap();

        // At block 9, should still be valid
        mempool.current_block = 9;
        let pruned = mempool.prune_expired();
        assert_eq!(pruned, 0);

        // At block 10, should be expired
        mempool.current_block = 10;
        let pruned = mempool.prune_expired();
        assert_eq!(pruned, 1);
        assert_eq!(mempool.total_pending(), 0);
    }

    #[test]
    fn test_mempool_evm_tx_priority() {
        let mut mempool = Mempool::new();

        mempool.insert_evm_tx(make_evm_tx(0, 100)).unwrap();
        mempool.insert_evm_tx(make_evm_tx(1, 200)).unwrap();
        mempool.insert_evm_tx(make_evm_tx(2, 150)).unwrap();

        let selection = mempool.select_transactions();
        assert_eq!(selection.evm_txs.len(), 3);
        // EVM txs sorted by gas price desc
        assert_eq!(selection.evm_txs[0].score, 200);
        assert_eq!(selection.evm_txs[1].score, 150);
        assert_eq!(selection.evm_txs[2].score, 100);
    }

    #[test]
    fn test_mempool_confirm_transactions() {
        let mut mempool = Mempool::new();

        let tx = make_evm_tx(0, 100);
        let hash = mempool.insert_evm_tx(tx).unwrap();

        assert_eq!(mempool.total_pending(), 1);

        mempool.confirm_transactions(&[hash]);
        assert_eq!(mempool.total_pending(), 0);
        assert!(!mempool.known_txs.contains(&hash));
    }

    #[test]
    fn test_mempool_pool_stats() {
        let mut mempool = Mempool::new();

        mempool.insert_evm_tx(make_evm_tx(0, 100)).unwrap();
        mempool.insert_evm_tx(make_evm_tx(1, 200)).unwrap();

        let stats = mempool.pool_stats();
        assert_eq!(stats.evm_count, 2);
        assert_eq!(stats.known_tx_count, 2);
    }
}
