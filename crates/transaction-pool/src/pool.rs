//! T8.1 — Transaction Pool (per spec §17)
//!
//! Mempool with multi-pool support, capacity limits, eviction, and anti-spam.

use call_primitives::{Address, TxHash};
use call_protocol::transaction::ProtocolTransaction;
use call_bridge::BridgeOp;
use call_crypto::keccak256;
use call_evm::EvmTransaction;
use std::collections::{HashMap, HashSet, VecDeque};

use crate::priority::{
        protocol_priority_score, MempoolEntry, PoolKind, PoolLimits, PriorityPool,
    };

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

// ── Memo size helper ─────────────────────────────────────────────────

/// Compute total memo bytes across all instructions in a transaction.
fn total_memo_bytes(instructions: &[call_protocol::instructions::Instruction]) -> usize {
    use call_protocol::instructions::Instruction;
    let mut total = 0usize;
    for instr in instructions {
        match instr {
            Instruction::Transfer { memo, .. } => {
                if let Some(m) = memo {
                    total += m.message.len();
                    if let Some(ref r) = m.reference {
                        total += r.len();
                    }
                    if let Some(ref md) = m.metadata {
                        total += md.len();
                    }
                }
            }
            Instruction::BatchTransfer { payments, .. } => {
                for p in payments {
                    if let Some(m) = &p.memo {
                        total += m.message.len();
                        if let Some(ref r) = m.reference {
                            total += r.len();
                        }
                        if let Some(ref md) = m.metadata {
                            total += md.len();
                        }
                    }
                }
            }
            _ => {}
        }
    }
    total
}

// ── Mempool (per spec §17.1) ─────────────────────────────────────────

/// The main mempool structure managing all transaction pools.
///
/// Per spec §17.1:
/// - protocol_pool: high priority (ProtocolTransaction)
/// - agent_pool: medium-high priority (Agent transactions)
/// - evm_pool: standard priority (EVM transactions)
/// - pending_bridges: FIFO bridge operations
/// - known_txs: deduplication cache
pub struct Mempool {
    /// Protocol transactions (50K limit)
    pub protocol_pool: PriorityPool,
    /// EVM transactions (100K limit)
    pub evm_pool: PriorityPool,
    /// Bridge operations (FIFO)
    pub pending_bridges: VecDeque<BridgeEntry>,
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

/// A bridge operation entry
#[derive(Debug, Clone)]
pub struct BridgeEntry {
    pub op: BridgeOp,
    pub entered_at_block: u64,
}

impl Mempool {
    /// Create a new mempool with default limits and config
    pub fn new() -> Self {
        Self {
            protocol_pool: PriorityPool::new(),
            evm_pool: PriorityPool::new(),
            pending_bridges: VecDeque::new(),
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

    // ── Protocol Transactions ─────────────────────────────────────

    /// Insert a protocol transaction into the mempool
    pub fn insert_protocol_tx(
        &mut self,
        tx: ProtocolTransaction,
    ) -> Result<TxHash, MempoolError> {
        // Compute tx hash via JSON serialization + keccak256
        let hash = self.compute_protocol_tx_hash(&tx);

        // Deduplication check
        if self.known_txs.contains(&hash) {
            return Err(MempoolError::Duplicate(hash));
        }

        // Gap 2 — Sequential nonce enforcement
        // Accept tx if nonce >= expected; block builder only executes the exact next nonce.
        let expected = self.expected_nonces.get(&tx.sender).copied().unwrap_or(0);
        if tx.nonce < expected {
            return Err(MempoolError::InvalidNonce {
                expected,
                got: tx.nonce,
            });
        }

        // Gap 9 — Instruction count limit
        use call_protocol::transaction::MAX_INSTRUCTIONS_PER_TX;
        if tx.instructions.len() > MAX_INSTRUCTIONS_PER_TX {
            return Err(MempoolError::Generic(format!(
                "instruction count {} exceeds max {}",
                tx.instructions.len(),
                MAX_INSTRUCTIONS_PER_TX
            )));
        }

        // Gap 10 — Memo size limit
        use call_protocol::transaction::MAX_TOTAL_MEMO_BYTES;
        let memo_bytes = total_memo_bytes(&tx.instructions);
        if memo_bytes > MAX_TOTAL_MEMO_BYTES {
            return Err(MempoolError::Generic(format!(
                "total memo size {} bytes exceeds limit {}",
                memo_bytes, MAX_TOTAL_MEMO_BYTES
            )));
        }

        // Gap 3 — Reject unimplemented sponsor configs
        match tx.gas_config {
            call_protocol::transaction::GasConfig::PoolSponsor => {
                return Err(MempoolError::Generic(
                    "PoolSponsor not yet enabled".into(),
                ));
            }
            call_protocol::transaction::GasConfig::PerTxSponsor { .. } => {
                return Err(MempoolError::Generic(
                    "PerTxSponsor not yet enabled".into(),
                ));
            }
            _ => {}
        }

        // Gap 5 — Validate fee with priority component
        let score = protocol_priority_score(&tx, self.base_fee);
        if score < self.config.min_fee {
            return Err(MempoolError::FeeTooLow {
                fee: score,
                min: self.config.min_fee,
            });
        }

        // Validate gas limit
        if tx.gas_limit > self.config.max_gas_limit {
            return Err(MempoolError::GasLimitExceeded {
                gas: tx.gas_limit,
                max: self.config.max_gas_limit,
            });
        }

        // Check per-address limit
        let addr_count = self.protocol_pool.count_per_address(&tx.sender);
        if addr_count >= self.limits.max_per_address {
            return Err(MempoolError::AddressLimitReached {
                address: tx.sender,
                count: addr_count,
                max: self.limits.max_per_address,
            });
        }

        // Check pool capacity
        if self.protocol_pool.len() >= self.limits.max_protocol_txs {
            // Try to evict lowest score entry to make room
            let lowest = self.protocol_pool.lowest_score_entry();
            if lowest.is_some_and(|e| e.score >= score) {
                return Err(MempoolError::PoolFull {
                    pool: "protocol".into(),
                    count: self.protocol_pool.len(),
                    max: self.limits.max_protocol_txs,
                });
            }
            self.protocol_pool.evict_lowest();
        }

        // Serialize tx as raw bytes
        let data = serde_json::to_vec(&tx).map_err(|e| MempoolError::Generic(e.to_string()))?;

        let entry = MempoolEntry::new(
            hash,
            score,
            tx.sender,
            tx.nonce,
            PoolKind::Protocol,
            data,
            self.current_block,
        );

        self.protocol_pool.insert(entry);
        self.known_txs.insert(hash);

        Ok(hash)
    }

    // ── EVM Transactions ─────────────────────────────────────────────

    /// Insert an EVM transaction into the mempool
    pub fn insert_evm_tx(
        &mut self,
        tx: EvmTransaction,
    ) -> Result<TxHash, MempoolError> {
        let data = serde_json::to_vec(&tx).map_err(|e| MempoolError::Generic(e.to_string()))?;
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

    // ── Bridge Operations ──────────────────────────────────────────

    /// Insert a bridge operation into the mempool
    pub fn insert_bridge_op(&mut self, op: BridgeOp) {
        if self.pending_bridges.len() < self.limits.max_bridges {
            self.pending_bridges.push_back(BridgeEntry {
                op,
                entered_at_block: self.current_block,
            });
        }
    }

    // ── Transaction Selection (for block building) ─────────────────

    /// Select the best transactions for the next block.
    ///
    /// Returns transactions in priority order: Protocol > Agent > EVM > Bridge.
    /// Protocol transactions sorted by score desc, EVM by gas price desc, Bridge FIFO.
    pub fn select_transactions(&mut self) -> MempoolSelection {
        let protocol_txs = self.protocol_pool.drain_sorted();
        let evm_txs = self.evm_pool.drain_sorted();

        // Clear known_txs for selected transactions
        for tx in &protocol_txs {
            self.known_txs.remove(&tx.hash);
        }
        for tx in &evm_txs {
            self.known_txs.remove(&tx.hash);
        }

        // Clear pools
        self.protocol_pool.clear();
        self.evm_pool.clear();

        MempoolSelection {
            protocol_txs,
            evm_txs,
            bridge_ops: std::mem::take(&mut self.pending_bridges)
                .into_iter()
                .map(|e| e.op)
                .collect(),
        }
    }

    // ── Maintenance ────────────────────────────────────────────────

    /// Remove expired transactions from all pools.
    ///
    /// Called each block to enforce the 72-block lifetime limit.
    pub fn prune_expired(&mut self) -> usize {
        let mut total_removed = 0;

        total_removed += self.protocol_pool.remove_expired(
            self.current_block,
            self.limits.lifetime_blocks,
        );
        total_removed += self.evm_pool.remove_expired(
            self.current_block,
            self.limits.lifetime_blocks,
        );

        // Remove expired bridges
        while let Some(front) = self.pending_bridges.front() {
            if front.entered_at_block + self.limits.lifetime_blocks <= self.current_block {
                self.pending_bridges.pop_front();
                total_removed += 1;
            } else {
                break;
            }
        }

        total_removed
    }

    /// Remove transactions confirmed in a block (by hash).
    /// Also increments expected nonces for included senders.
    pub fn confirm_transactions(&mut self, hashes: &[TxHash]) {
        for hash in hashes {
            if let Some(entry) = self.protocol_pool.remove(hash) {
                let next = entry.nonce.saturating_add(1);
                self.expected_nonces.insert(entry.sender, next);
            }
            self.evm_pool.remove(hash);
            self.known_txs.remove(hash);
        }
    }

    /// Increment expected nonce for a sender after their tx is included in a block.
    pub fn increment_nonce(&mut self, sender: Address) {
        let current = self.expected_nonces.get(&sender).copied().unwrap_or(0);
        self.expected_nonces.insert(sender, current.saturating_add(1));
    }

    /// Get total number of pending transactions
    pub fn total_pending(&self) -> usize {
        self.protocol_pool.len()
            + self.evm_pool.len()
            + self.pending_bridges.len()
    }

    /// Get pool stats
    pub fn pool_stats(&self) -> PoolStats {
        PoolStats {
            protocol_count: self.protocol_pool.len(),
            evm_count: self.evm_pool.len(),
            bridge_count: self.pending_bridges.len(),
            known_tx_count: self.known_txs.len(),
        }
    }

    // ── Placeholder Hash Computation ───────────────────────────────

    fn compute_protocol_tx_hash(&self, tx: &ProtocolTransaction) -> TxHash {
        let data = serde_json::to_vec(tx).unwrap_or_default();
        keccak256(&data)
    }
}

// ── Mempool Selection Result ─────────────────────────────────────────

/// Selected transactions for block building
#[derive(Debug)]
pub struct MempoolSelection {
    pub protocol_txs: Vec<MempoolEntry>,
    pub evm_txs: Vec<MempoolEntry>,
    pub bridge_ops: Vec<BridgeOp>,
}

// ── Pool Stats ───────────────────────────────────────────────────────

/// Current pool statistics
#[derive(Debug)]
pub struct PoolStats {
    pub protocol_count: usize,
    pub evm_count: usize,
    pub bridge_count: usize,
    pub known_tx_count: usize,
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;
    use call_protocol::instructions::Instruction;
    use call_protocol::transaction::{AuthScheme, GasConfig};
    use call_evm::EvmTransaction;
    use call_primitives::U256;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn make_test_tx(nonce: u64, fee: u128) -> ProtocolTransaction {
        ProtocolTransaction {
            sender: test_addr(1),
            nonce,
            instructions: vec![Instruction::Transfer {
                asset_id: 1,
                to: test_addr(2),
                amount: 100,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: fee,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        }
    }

    fn make_evm_tx(nonce: u64, gas_price: u128) -> EvmTransaction {
        EvmTransaction {
            caller: test_addr(1),
            nonce,
            gas_limit: 21_000,
            gas_price,
            to: Some(test_addr(2)),
            value: U256::from(100),
            data: call_evm::Bytes::default(),
            chain_id: 1,
        }
    }

    #[test]
    fn test_mempool_insert_protocol_tx() {
        let mut mempool = Mempool::new();
        let tx = make_test_tx(0, 1_000_000);

        let hash = mempool.insert_protocol_tx(tx);
        assert!(hash.is_ok());
        assert_eq!(mempool.total_pending(), 1);
    }

    #[test]
    fn test_mempool_duplicate_tx_rejected() {
        let mut mempool = Mempool::new();
        let tx = make_test_tx(0, 1_000_000);

        mempool.insert_protocol_tx(tx.clone()).unwrap();
        let result = mempool.insert_protocol_tx(tx);
        assert!(matches!(result, Err(MempoolError::Duplicate(_))));
    }

    #[test]
    fn test_mempool_fee_too_low() {
        let mut mempool = Mempool::new();
        let tx = make_test_tx(0, 10); // very low fee

        let result = mempool.insert_protocol_tx(tx);
        assert!(matches!(result, Err(MempoolError::FeeTooLow { .. })));
    }

    #[test]
    fn test_mempool_gas_limit_exceeded() {
        let mut mempool = Mempool::new();
        let mut tx = make_test_tx(0, 1_000_000);
        tx.gas_limit = 20_000_000; // exceeds default 10M

        let result = mempool.insert_protocol_tx(tx);
        assert!(matches!(result, Err(MempoolError::GasLimitExceeded { .. })));
    }

    #[test]
    fn test_mempool_per_address_limit() {
        let mut mempool = Mempool::new();
        mempool.limits.max_per_address = 3;

        for nonce in 0..3 {
            let mut tx = make_test_tx(nonce, 1_000_000);
            tx.sender = test_addr(1);
            mempool.insert_protocol_tx(tx).unwrap();
        }

        // 4th tx should fail
        let tx = make_test_tx(3, 1_000_000);
        let result = mempool.insert_protocol_tx(tx);
        assert!(matches!(result, Err(MempoolError::AddressLimitReached { .. })));
    }

    #[test]
    fn test_mempool_capacity_limit() {
        let mut mempool = Mempool::new();
        mempool.limits.max_protocol_txs = 3;

        // Fill the pool
        for nonce in 0..3 {
            let mut tx = make_test_tx(nonce, 1_000_000);
            tx.sender = test_addr(1 + nonce as u8); // different senders
            mempool.insert_protocol_tx(tx).unwrap();
        }

        // Pool full, new tx with higher fee should evict lowest
        let tx = make_test_tx(99, 2_000_000); // higher fee
        let result = mempool.insert_protocol_tx(tx);
        assert!(result.is_ok());
        assert_eq!(mempool.protocol_pool.len(), 3);

        // New tx with lower fee (but still above min after gas cost) should fail
        // base_fee=1, gas_units=10_000, so score = max_fee - 10_000
        // Need score > min_fee(1_000), so max_fee > 11_000
        // Pool entries have score ~990_000, so this tx score should be lower
        let mut tx = make_test_tx(100, 15_000); // score = 5_000, below pool entries
        tx.sender = test_addr(200); // unique sender to avoid address limit
        let result = mempool.insert_protocol_tx(tx);
        assert!(matches!(result, Err(MempoolError::PoolFull { .. })), "expected PoolFull, got {:?}", result);
    }

    #[test]
    fn test_mempool_eviction_low_fee() {
        let mut mempool = Mempool::new();
        mempool.limits.max_protocol_txs = 3;

        // Insert with increasing fees
        for i in 0..3 {
            let mut tx = make_test_tx(i, (i as u128 + 1) * 1_000_000);
            tx.sender = test_addr(1 + i as u8);
            mempool.insert_protocol_tx(tx).unwrap();
        }

        assert_eq!(mempool.protocol_pool.len(), 3);

        // Insert a high-fee tx — should evict the lowest
        let tx = make_test_tx(99, 10_000_000);
        mempool.insert_protocol_tx(tx).unwrap();

        // Should still be at capacity, lowest evicted
        assert_eq!(mempool.protocol_pool.len(), 3);
    }

    #[test]
    fn test_mempool_lifetime_expiry() {
        let mut mempool = Mempool::new();
        mempool.limits.lifetime_blocks = 10;

        // Insert tx at block 0
        mempool.current_block = 0;
        let tx = make_test_tx(0, 1_000_000);
        mempool.insert_protocol_tx(tx).unwrap();

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

        assert_eq!(mempool.evm_pool.len(), 3);

        // Insert protocol tx — should be higher priority
        let tx = make_test_tx(0, 500_000);
        mempool.insert_protocol_tx(tx).unwrap();

        let selection = mempool.select_transactions();
        assert_eq!(selection.protocol_txs.len(), 1);
        assert_eq!(selection.evm_txs.len(), 3);
        // EVM txs sorted by gas price desc
        assert_eq!(selection.evm_txs[0].score, 200);
        assert_eq!(selection.evm_txs[1].score, 150);
        assert_eq!(selection.evm_txs[2].score, 100);
    }

    #[test]
    fn test_mempool_bridge_operations() {
        let mut mempool = Mempool::new();
        mempool.limits.max_bridges = 3;

        let op1 = BridgeOp::DepositToEvm {
            asset_id: 1,
            from: test_addr(1),
            to: test_addr(2),
            amount: 1000,
        };
        let op2 = BridgeOp::WithdrawToProtocol {
            asset_id: 1,
            from: test_addr(2),
            to: test_addr(1),
            amount: 500,
        };

        mempool.insert_bridge_op(op1.clone());
        mempool.insert_bridge_op(op2.clone());

        assert_eq!(mempool.pending_bridges.len(), 2);

        let selection = mempool.select_transactions();
        assert_eq!(selection.bridge_ops.len(), 2);
    }

    #[test]
    fn test_mempool_confirm_transactions() {
        let mut mempool = Mempool::new();

        let tx = make_test_tx(0, 1_000_000);
        let hash = mempool.insert_protocol_tx(tx).unwrap();

        assert_eq!(mempool.total_pending(), 1);

        mempool.confirm_transactions(&[hash]);
        assert_eq!(mempool.total_pending(), 0);
        assert!(!mempool.known_txs.contains(&hash));
    }

    #[test]
    fn test_mempool_pool_stats() {
        let mut mempool = Mempool::new();

        let tx = make_test_tx(0, 1_000_000);
        mempool.insert_protocol_tx(tx).unwrap();
        mempool.insert_evm_tx(make_evm_tx(0, 100)).unwrap();

        let stats = mempool.pool_stats();
        assert_eq!(stats.protocol_count, 1);
        assert_eq!(stats.evm_count, 1);
        assert_eq!(stats.bridge_count, 0);
        assert_eq!(stats.known_tx_count, 2);
    }
}
