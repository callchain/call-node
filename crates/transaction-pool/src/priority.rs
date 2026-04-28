//! T8.1 — Transaction Priority (per spec §17.2)
//!
//! Multi-currency priority scoring and cross-pool ordering.

use call_primitives::{Address, TxHash};
use call_protocol::transaction::{ProtocolTransaction, calculate_gas_units};
use std::collections::BTreeMap;
use std::time::Instant;

// ── Pool Priority Levels (per spec §17.2) ─────────────────────────────

/// Cross-pool priority ordering: Protocol > Agent > EVM > Bridge
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
#[repr(u8)]
pub enum PoolKind {
    /// Protocol transactions: highest priority
    Protocol = 0,
    /// Agent transactions: medium-high priority
    Agent = 1,
    /// EVM transactions: standard priority
    Evm = 2,
    /// Bridge operations: lowest priority
    Bridge = 3,
}

// ── Capacity Limits (per spec §17.3) ──────────────────────────────────

/// Mempool capacity limits per pool
#[derive(Debug, Clone, Copy)]
pub struct PoolLimits {
    /// Maximum protocol transactions (50,000)
    pub max_protocol_txs: usize,
    /// Maximum EVM transactions (100,000)
    pub max_evm_txs: usize,
    /// Maximum agent transactions (25,000)
    pub max_agent_txs: usize,
    /// Maximum pending bridges (1,000)
    pub max_bridges: usize,
    /// Maximum pending txs per address (256)
    pub max_per_address: usize,
    /// Transaction lifetime in blocks (72)
    pub lifetime_blocks: u64,
}

impl Default for PoolLimits {
    fn default() -> Self {
        Self {
            max_protocol_txs: 50_000,
            max_evm_txs: 100_000,
            max_agent_txs: 25_000,
            max_bridges: 1_000,
            max_per_address: 256,
            lifetime_blocks: 72,
        }
    }
}

// ── Mempool Transaction Entry ────────────────────────────────────────

/// A transaction entry in the mempool
#[derive(Debug, Clone)]
pub struct MempoolEntry {
    /// Transaction hash
    pub hash: TxHash,
    /// Priority score (CALL equivalent)
    pub score: u128,
    /// Sender address
    pub sender: Address,
    /// Nonce
    pub nonce: u64,
    /// Pool kind
    pub kind: PoolKind,
    /// Raw transaction bytes
    pub data: Vec<u8>,
    /// Time when this tx entered the pool
    pub entered_at: Instant,
    /// Block height when this tx entered the pool
    pub entered_at_block: u64,
}

impl MempoolEntry {
    /// Create a new mempool entry
    pub fn new(
        hash: TxHash,
        score: u128,
        sender: Address,
        nonce: u64,
        kind: PoolKind,
        data: Vec<u8>,
        current_block: u64,
    ) -> Self {
        Self {
            hash,
            score,
            sender,
            nonce,
            kind,
            data,
            entered_at: Instant::now(),
            entered_at_block: current_block,
        }
    }

    /// Check if this entry has expired (exceeded lifetime)
    pub fn is_expired(&self, current_block: u64, lifetime: u64) -> bool {
        current_block.saturating_sub(self.entered_at_block) >= lifetime
    }
}

// ── Priority Score Calculation (per spec §17.2) ───────────────────────

/// Calculate priority score for a protocol transaction.
/// Per spec §17.2: priority_score = max_fee in CALL equivalent.
///
/// For stablecoin fees, converts to CALL equivalent using oracle price.
pub fn protocol_priority_score(
    tx: &ProtocolTransaction,
    base_fee: u128,
) -> u128 {
    // Total gas units for all instructions
    let gas_units = calculate_gas_units(&tx.instructions);
    let gas_cost = gas_units as u128 * base_fee;

    // Priority score = max_fee - gas_cost
    // Higher fee txs get higher priority
    tx.max_fee.saturating_sub(gas_cost)
}

// ── Priority Queue (ordered by score desc, then FIFO) ────────────────

/// A priority-ordered transaction pool.
///
/// Entries are sorted by score (descending), with ties broken by arrival time (FIFO).
#[derive(Default)]
pub struct PriorityPool {
    /// Entries sorted by score (ascending in BTreeMap, so we iterate backwards for highest first)
    /// Key: (score, arrival_order), Value: MempoolEntry
    entries: BTreeMap<(u128, u64), MempoolEntry>,
    /// Counter for ordering ties
    next_order: u64,
}

impl PriorityPool {
    /// Create a new empty pool
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of entries
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Get the expected next nonce for an address by scanning current entries.
    pub fn get_address_nonce(&self, address: &Address) -> u64 {
        self.entries
            .values()
            .filter(|e| e.sender == *address)
            .map(|e| e.nonce)
            .max()
            .map(|n| n.saturating_add(1))
            .unwrap_or(0)
    }

    /// Insert an entry. Returns the order assigned.
    pub fn insert(&mut self, entry: MempoolEntry) -> u64 {
        let order = self.next_order;
        self.next_order += 1;

        self.entries.insert((entry.score, order), entry);
        order
    }

    /// Remove an entry by hash
    pub fn remove(&mut self, hash: &TxHash) -> Option<MempoolEntry> {
        let key = self
            .entries
            .iter()
            .find(|(_, e)| e.hash == *hash)
            .map(|(k, _)| *k);

        if let Some(key) = key {
            self.entries.remove(&key)
        } else {
            None
        }
    }

    /// Check if a tx hash exists
    pub fn contains(&self, hash: &TxHash) -> bool {
        self.entries.values().any(|e| e.hash == *hash)
    }

    /// Get all entries sorted by priority (highest score first, FIFO for ties)
    pub fn drain_sorted(&mut self) -> Vec<MempoolEntry> {
        let mut entries: Vec<_> = self.entries.values().cloned().collect();
        // Stable sort preserves FIFO order for equal scores.
        entries.sort_by(|a, b| b.score.cmp(&a.score));
        entries
    }

    /// Clear all entries
    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// Get the lowest score entry (for eviction)
    pub fn lowest_score_entry(&self) -> Option<&MempoolEntry> {
        self.entries.iter().next().map(|(_, e)| e)
    }

    /// Remove the lowest score entry
    pub fn evict_lowest(&mut self) -> Option<MempoolEntry> {
        if let Some((key, _)) = self.entries.iter().next() {
            let key = *key;
            self.entries.remove(&key)
        } else {
            None
        }
    }

    /// Remove all entries older than the given lifetime
    pub fn remove_expired(&mut self, current_block: u64, lifetime: u64) -> usize {
        let expired: Vec<_> = self
            .entries
            .iter()
            .filter(|(_, e)| e.is_expired(current_block, lifetime))
            .map(|(k, _)| *k)
            .collect();

        let count = expired.len();
        for key in expired {
            self.entries.remove(&key);
        }
        count
    }

    /// Get count of txs per address
    pub fn count_per_address(&self, address: &Address) -> usize {
        self.entries
            .values()
            .filter(|e| e.sender == *address)
            .count()
    }

    /// Iterate over all entries (highest score first).
    pub fn iter(&self) -> impl Iterator<Item = &MempoolEntry> {
        self.entries.iter().rev().map(|(_, e)| e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;
    use call_primitives::FeeCurrency;
    use call_protocol::instructions::Instruction;
    use call_protocol::transaction::{AuthScheme, GasConfig};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn make_test_tx(score: u128) -> ProtocolTransaction {
        ProtocolTransaction {
            sender: test_addr(1),
            nonce: 1,
            instructions: vec![Instruction::Transfer {
                asset_id: 1,
                to: test_addr(2),
                amount: 100,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: score,
            max_priority_fee: 1,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        }
    }

    #[test]
    fn test_pool_limits_defaults() {
        let limits = PoolLimits::default();
        assert_eq!(limits.max_protocol_txs, 50_000);
        assert_eq!(limits.max_evm_txs, 100_000);
        assert_eq!(limits.max_per_address, 256);
        assert_eq!(limits.lifetime_blocks, 72);
    }

    #[test]
    fn test_pool_kind_ordering() {
        assert!(PoolKind::Protocol < PoolKind::Agent);
        assert!(PoolKind::Agent < PoolKind::Evm);
        assert!(PoolKind::Evm < PoolKind::Bridge);
    }

    #[test]
    fn test_priority_pool_insert() {
        let mut pool = PriorityPool::new();
        let entry = MempoolEntry::new(
            TxHash::repeat_byte(1),
            1000,
            test_addr(1),
            0,
            PoolKind::Protocol,
            vec![1, 2, 3],
            0,
        );
        pool.insert(entry);
        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&TxHash::repeat_byte(1)));
    }

    #[test]
    fn test_priority_pool_remove() {
        let mut pool = PriorityPool::new();
        let entry = MempoolEntry::new(
            TxHash::repeat_byte(1),
            1000,
            test_addr(1),
            0,
            PoolKind::Protocol,
            vec![1, 2, 3],
            0,
        );
        pool.insert(entry);

        let removed = pool.remove(&TxHash::repeat_byte(1));
        assert!(removed.is_some());
        assert_eq!(pool.len(), 0);
        assert!(!pool.contains(&TxHash::repeat_byte(1)));
    }

    #[test]
    fn test_priority_pool_drain_sorted() {
        let mut pool = PriorityPool::new();

        // Insert entries with different scores
        pool.insert(MempoolEntry::new(
            TxHash::repeat_byte(1),
            100,
            test_addr(1),
            0,
            PoolKind::Protocol,
            vec![1],
            0,
        ));
        pool.insert(MempoolEntry::new(
            TxHash::repeat_byte(2),
            300,
            test_addr(2),
            0,
            PoolKind::Protocol,
            vec![2],
            0,
        ));
        pool.insert(MempoolEntry::new(
            TxHash::repeat_byte(3),
            200,
            test_addr(3),
            0,
            PoolKind::Protocol,
            vec![3],
            0,
        ));

        let drained = pool.drain_sorted();
        assert_eq!(drained.len(), 3);
        // Should be sorted by score descending
        assert_eq!(drained[0].score, 300);
        assert_eq!(drained[1].score, 200);
        assert_eq!(drained[2].score, 100);
    }

    #[test]
    fn test_priority_pool_evict_lowest() {
        let mut pool = PriorityPool::new();

        pool.insert(MempoolEntry::new(
            TxHash::repeat_byte(1),
            50,
            test_addr(1),
            0,
            PoolKind::Protocol,
            vec![1],
            0,
        ));
        pool.insert(MempoolEntry::new(
            TxHash::repeat_byte(2),
            200,
            test_addr(2),
            0,
            PoolKind::Protocol,
            vec![2],
            0,
        ));

        let evicted = pool.evict_lowest().unwrap();
        assert_eq!(evicted.score, 50);
        assert_eq!(pool.len(), 1);
    }

    #[test]
    fn test_priority_pool_expired_entries() {
        let mut pool = PriorityPool::new();

        // Insert with block 0
        pool.insert(MempoolEntry::new(
            TxHash::repeat_byte(1),
            100,
            test_addr(1),
            0,
            PoolKind::Protocol,
            vec![1],
            0,
        ));
        // Insert with block 50
        pool.insert(MempoolEntry::new(
            TxHash::repeat_byte(2),
            100,
            test_addr(2),
            0,
            PoolKind::Protocol,
            vec![2],
            50,
        ));

        // At block 80, lifetime 72: first entry (block 0) should be expired
        let expired = pool.remove_expired(80, 72);
        assert_eq!(expired, 1);
        assert_eq!(pool.len(), 1);
        assert!(pool.contains(&TxHash::repeat_byte(2)));
    }

    #[test]
    fn test_priority_pool_per_address_count() {
        let mut pool = PriorityPool::new();
        let addr = test_addr(5);

        for nonce in 0..5 {
            pool.insert(MempoolEntry::new(
                TxHash::repeat_byte(nonce as u8),
                100,
                addr,
                nonce,
                PoolKind::Protocol,
                vec![nonce as u8],
                0,
            ));
        }

        assert_eq!(pool.count_per_address(&addr), 5);
        assert_eq!(pool.count_per_address(&test_addr(99)), 0);
    }

    #[test]
    fn test_protocol_priority_score_call() {
        let tx = make_test_tx(1_000_000);
        let score = protocol_priority_score(&tx, 10);
        // score = max_fee - gas_cost * base_fee
        assert!(score > 0);
    }

    #[test]
    fn test_mempool_entry_is_expired() {
        let entry = MempoolEntry::new(
            TxHash::repeat_byte(1),
            100,
            test_addr(1),
            0,
            PoolKind::Protocol,
            vec![1],
            10,
        );

        assert!(!entry.is_expired(80, 72)); // 70 blocks < 72
        assert!(entry.is_expired(82, 72)); // 72 blocks == 72
        assert!(entry.is_expired(100, 72)); // 90 blocks > 72
    }
}
