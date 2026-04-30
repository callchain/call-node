//! Security & Attack Prevention (per spec §13)
//!
//! Block limits, mempool attack prevention, shielded pool defense,
//! P2P rate limiting, consensus attack defense, and MEV protection.

use call_primitives::{Address, TxHash, ValidatorId};
use std::collections::{HashMap, HashSet};

// ─── Block Limits (per spec §13.5) ─────────────────────────────────

/// Block and transaction size limits
#[derive(Debug, Clone, Copy)]
pub struct BlockLimits {
    /// Maximum transaction size in bytes (64 KB)
    pub max_tx_size: usize,
    /// Maximum instructions per transaction (256)
    pub max_instructions: usize,
    /// Maximum batch transfer recipients (100)
    pub max_batch_recipients: usize,
    /// Maximum shielded proofs per block (50)
    pub max_shielded_proofs_per_block: usize,
    /// Maximum total block tx count (10000)
    pub max_txs_per_block: usize,
    /// Maximum block size in bytes (4 MB)
    pub max_block_size: usize,
}

impl Default for BlockLimits {
    fn default() -> Self {
        Self {
            max_tx_size: 64 * 1024,
            max_instructions: 256,
            max_batch_recipients: 100,
            max_shielded_proofs_per_block: 50,
            max_txs_per_block: 10_000,
            max_block_size: 4 * 1024 * 1024,
        }
    }
}

impl BlockLimits {
    /// Validate a block's total size
    pub fn validate_block_size(&self, tx_count: usize, total_size: usize) -> Result<(), SecurityError> {
        if tx_count > self.max_txs_per_block {
            return Err(SecurityError::TooManyTxsInBlock {
                count: tx_count,
                max: self.max_txs_per_block,
            });
        }
        if total_size > self.max_block_size {
            return Err(SecurityError::BlockTooLarge {
                size: total_size,
                max: self.max_block_size,
            });
        }
        Ok(())
    }
}

// ─── Mempool Attack Prevention ─────────────────────────────────────

/// Rate limiter for mempool and P2P
#[derive(Debug, Clone)]
pub struct RateLimiter {
    /// Maximum requests per window
    pub max_requests: u32,
    /// Window duration in milliseconds
    pub window_ms: u64,
    /// Per-address request counters: (address -> (count, window_start))
    counters: HashMap<Address, (u32, u64)>,
}

impl RateLimiter {
    pub fn new(max_requests: u32, window_ms: u64) -> Self {
        Self {
            max_requests,
            window_ms,
            counters: HashMap::new(),
        }
    }

    /// Check if a request is allowed. Returns false if rate limited.
    pub fn allow(&mut self, address: Address, current_time_ms: u64) -> bool {
        let entry = self.counters.entry(address).or_insert((0, current_time_ms));
        let (count, window_start) = *entry;

        if current_time_ms >= window_start + self.window_ms {
            // New window
            entry.0 = 1;
            entry.1 = current_time_ms;
            true
        } else if count < self.max_requests {
            entry.0 += 1;
            true
        } else {
            false
        }
    }

    /// Clear expired entries
    pub fn cleanup(&mut self, current_time_ms: u64) {
        self.counters
            .retain(|_, entry| current_time_ms < entry.1 + self.window_ms);
    }
}

/// Replay protection using nonce tracking and seen hash set
#[derive(Debug)]
pub struct ReplayProtector {
    seen_hashes: HashSet<TxHash>,
    max_seen: usize,
}

impl ReplayProtector {
    pub fn new(max_seen: usize) -> Self {
        Self {
            seen_hashes: HashSet::with_capacity(max_seen),
            max_seen,
        }
    }

    /// Check if a transaction is a replay. Returns true if first seen.
    pub fn check_and_record(&mut self, hash: TxHash) -> bool {
        let first_seen = self.seen_hashes.insert(hash);

        // Bound the set size to prevent memory DoS
        if self.seen_hashes.len() > self.max_seen {
            // Remove ~25% of entries when over limit
            let to_remove: Vec<_> = self.seen_hashes.iter().take(self.max_seen / 4).copied().collect();
            for h in &to_remove {
                self.seen_hashes.remove(h);
            }
        }

        first_seen
    }

    /// Remove a hash from the seen set (used for rollback on insertion failure).
    pub fn remove_hash(&mut self, hash: &TxHash) {
        self.seen_hashes.remove(hash);
    }
}

/// Mempool attack prevention state
#[derive(Debug)]
pub struct MempoolDefense {
    rate_limiter: RateLimiter,
    replay_protector: ReplayProtector,
    /// Running count of txs per address in current window
    tx_counts: HashMap<Address, u32>,
    /// Max txs per address per window
    max_txs_per_address: u32,
}

impl MempoolDefense {
    pub fn new(
        rate_limit: u32,
        rate_window_ms: u64,
        max_seen_hashes: usize,
        max_txs_per_address: u32,
    ) -> Self {
        Self {
            rate_limiter: RateLimiter::new(rate_limit, rate_window_ms),
            replay_protector: ReplayProtector::new(max_seen_hashes),
            tx_counts: HashMap::new(),
            max_txs_per_address,
        }
    }

    /// Check if a transaction should be accepted (anti-flood, anti-replay)
    pub fn validate_tx_submission(
        &mut self,
        sender: Address,
        hash: TxHash,
        current_time_ms: u64,
    ) -> Result<(), SecurityError> {
        // Rate limit check
        if !self.rate_limiter.allow(sender, current_time_ms) {
            return Err(SecurityError::RateLimited);
        }

        // Replay check
        if !self.replay_protector.check_and_record(hash) {
            return Err(SecurityError::ReplayDetected);
        }

        // Address saturation check
        let count = self.tx_counts.entry(sender).or_insert(0);
        if *count >= self.max_txs_per_address {
            return Err(SecurityError::AddressSaturation);
        }
        *count += 1;

        Ok(())
    }

    /// Reset address counter (called after block inclusion)
    pub fn on_tx_confirmed(&mut self, sender: Address) {
        if let Some(count) = self.tx_counts.get_mut(&sender) {
            *count = count.saturating_sub(1);
        }
    }

    /// Rollback defense state when mempool insertion fails after validation.
    /// Reverses tx_counts and replay protector so the tx can be re-submitted.
    pub fn rollback_submission(&mut self, sender: Address, hash: TxHash) {
        if let Some(count) = self.tx_counts.get_mut(&sender) {
            *count = count.saturating_sub(1);
        }
        self.replay_protector.remove_hash(&hash);
    }

    /// Cleanup expired rate limiter entries
    pub fn cleanup(&mut self, current_time_ms: u64) {
        self.rate_limiter.cleanup(current_time_ms);
    }
}

// ─── Shielded Pool Defense ─────────────────────────────────────────

/// Shielded pool per-block limits and nullifier tracking
#[derive(Debug)]
pub struct ShieldedDefense {
    /// Max shielded transactions per block
    pub max_per_block: usize,
    /// Known nullifiers (never expire)
    nullifiers: HashSet<[u8; 32]>,
    /// Current block shielded tx count
    current_block_count: usize,
}

impl ShieldedDefense {
    pub fn new(max_per_block: usize) -> Self {
        Self {
            max_per_block,
            nullifiers: HashSet::new(),
            current_block_count: 0,
        }
    }

    /// Start a new block
    pub fn begin_block(&mut self) {
        self.current_block_count = 0;
    }

    /// Validate a shielded transaction: check per-block limit and nullifier uniqueness
    pub fn validate_shielded_tx(
        &mut self,
        nullifiers: &[[u8; 32]],
    ) -> Result<(), SecurityError> {
        // Per-block limit
        if self.current_block_count + nullifiers.len() > self.max_per_block {
            return Err(SecurityError::ShieldedPerBlockLimit {
                count: self.current_block_count + nullifiers.len(),
                max: self.max_per_block,
            });
        }

        // Nullifier double-spend check
        for nf in nullifiers {
            if self.nullifiers.contains(nf) {
                return Err(SecurityError::NullifierDoubleSpend);
            }
        }

        // Record nullifiers
        for nf in nullifiers {
            self.nullifiers.insert(*nf);
        }
        self.current_block_count += nullifiers.len();

        Ok(())
    }

    /// Check if a nullifier has been seen (never expires)
    pub fn has_nullifier(&self, nullifier: &[u8; 32]) -> bool {
        self.nullifiers.contains(nullifier)
    }

    /// Get total nullifier count
    pub fn nullifier_count(&self) -> usize {
        self.nullifiers.len()
    }
}

// ─── P2P Defense ───────────────────────────────────────────────────

/// P2P rate limiting and large message defense
#[derive(Debug)]
pub struct P2PDefense {
    /// Per-peer message rate limiter
    peer_rates: HashMap<String, (u32, u64)>,
    /// Max messages per peer per window
    max_msgs_per_window: u32,
    /// Window duration in milliseconds
    window_ms: u64,
    /// Max message size in bytes (1 MB)
    pub max_message_size: usize,
}

impl P2PDefense {
    pub fn new(max_msgs_per_window: u32, window_ms: u64, max_message_size: usize) -> Self {
        Self {
            peer_rates: HashMap::new(),
            max_msgs_per_window,
            window_ms,
            max_message_size,
        }
    }

    /// Check if a P2P message should be accepted
    pub fn validate_message(
        &mut self,
        peer_id: String,
        message_size: usize,
        current_time_ms: u64,
    ) -> Result<(), SecurityError> {
        // Large message defense
        if message_size > self.max_message_size {
            return Err(SecurityError::MessageTooLarge {
                size: message_size,
                max: self.max_message_size,
            });
        }

        // Rate limit per peer
        let entry = self
            .peer_rates
            .entry(peer_id)
            .or_insert((0, current_time_ms));
        let (count, window_start) = *entry;

        if current_time_ms >= window_start + self.window_ms {
            entry.0 = 1;
            entry.1 = current_time_ms;
        } else if count >= self.max_msgs_per_window {
            return Err(SecurityError::PeerRateLimited);
        } else {
            entry.0 += 1;
        }

        Ok(())
    }

    /// Cleanup expired peer entries
    pub fn cleanup(&mut self, current_time_ms: u64) {
        self.peer_rates
            .retain(|_, (_, ws)| current_time_ms < *ws + self.window_ms);
    }
}

// ─── Consensus Defense ─────────────────────────────────────────────

/// Consensus attack detection and slashing triggers
#[derive(Debug)]
pub struct ConsensusDefense {
    /// Tracks blocks signed by each validator (for double-sign detection)
    blocks_per_round: HashMap<u64, HashMap<ValidatorId, [u8; 32]>>, // round -> (validator -> block_hash)
    /// Validators that have been slashed for double-signing
    slashed_validators: HashSet<ValidatorId>,
}

impl Default for ConsensusDefense {
    fn default() -> Self {
        Self::new()
    }
}

impl ConsensusDefense {
    pub fn new() -> Self {
        Self {
            blocks_per_round: HashMap::new(),
            slashed_validators: HashSet::new(),
        }
    }

    /// Record a block signature and detect double-signing.
    /// Returns Some(double_sign_evidence) if a validator signed two different blocks at the same round.
    pub fn record_block_signature(
        &mut self,
        validator_id: ValidatorId,
        round: u64,
        block_hash: [u8; 32],
    ) -> Option<DoubleSignEvidence> {
        if self.slashed_validators.contains(&validator_id) {
            return None; // Already slashed
        }

        let round_map = self.blocks_per_round.entry(round).or_default();

        if let Some(&existing_hash) = round_map.get(&validator_id) {
            if existing_hash != block_hash {
                // Double sign detected!
                let evidence = DoubleSignEvidence {
                    validator_id,
                    round,
                    block_hash_a: existing_hash,
                    block_hash_b: block_hash,
                };
                self.slashed_validators.insert(validator_id);
                return Some(evidence);
            }
        }

        round_map.insert(validator_id, block_hash);
        None
    }

    /// Check if a validator has been slashed
    pub fn is_slashed(&self, validator_id: ValidatorId) -> bool {
        self.slashed_validators.contains(&validator_id)
    }

    /// Get slashed validator count
    pub fn slashed_count(&self) -> usize {
        self.slashed_validators.len()
    }

    /// Cleanup old rounds (keep only recent)
    pub fn cleanup_old_rounds(&mut self, keep_rounds: u64, current_round: u64) {
        let cutoff = current_round.saturating_sub(keep_rounds);
        self.blocks_per_round.retain(|round, _| *round >= cutoff);
    }
}

/// Evidence of double-signing
#[derive(Debug, Clone)]
pub struct DoubleSignEvidence {
    pub validator_id: ValidatorId,
    pub round: u64,
    pub block_hash_a: [u8; 32],
    pub block_hash_b: [u8; 32],
}

// ─── MEV Protection ────────────────────────────────────────────────

/// Proposer-Builder Separation state
#[derive(Debug, Default)]
pub struct MevProtection {
    /// Whether PBS is enabled
    pub pbs_enabled: bool,
    /// Registered builders
    pub registered_builders: HashSet<Address>,
    /// Commit-reveal state: committed hashes awaiting reveal
    pub pending_commits: HashMap<[u8; 32], CommitEntry>,
}

#[derive(Debug, Clone)]
pub struct CommitEntry {
    pub committer: Address,
    pub committed_at: u64,
    /// Revealed transaction (None until revealed)
    pub revealed: Option<Vec<u8>>,
}

impl MevProtection {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a block builder
    pub fn register_builder(&mut self, builder: Address) {
        self.registered_builders.insert(builder);
    }

    /// Check if an address is a registered builder
    pub fn is_registered_builder(&self, builder: &Address) -> bool {
        self.registered_builders.contains(builder)
    }

    /// Commit a transaction hash (first phase of commit-reveal)
    pub fn commit_tx(
        &mut self,
        commitment: [u8; 32],
        committer: Address,
        current_time_ms: u64,
    ) -> Result<(), SecurityError> {
        if self.pending_commits.contains_key(&commitment) {
            return Err(SecurityError::DuplicateCommitment);
        }

        self.pending_commits.insert(
            commitment,
            CommitEntry {
                committer,
                committed_at: current_time_ms,
                revealed: None,
            },
        );
        Ok(())
    }

    /// Reveal a transaction (second phase of commit-reveal)
    pub fn reveal_tx(
        &mut self,
        commitment: [u8; 32],
        revealed_data: Vec<u8>,
    ) -> Result<Vec<u8>, SecurityError> {
        let entry = self
            .pending_commits
            .get_mut(&commitment)
            .ok_or(SecurityError::CommitmentNotFound)?;

        if entry.revealed.is_some() {
            return Err(SecurityError::AlreadyRevealed);
        }

        // Verify the commitment matches the revealed data
        let computed_commitment = keccak256_hash(&revealed_data);
        if computed_commitment != commitment {
            return Err(SecurityError::CommitmentMismatch);
        }

        entry.revealed = Some(revealed_data.clone());
        Ok(revealed_data)
    }

    /// Cleanup expired commitments (older than timeout_ms)
    pub fn cleanup_expired(&mut self, current_time_ms: u64, timeout_ms: u64) {
        self.pending_commits
            .retain(|_, entry| current_time_ms < entry.committed_at + timeout_ms);
    }
}

/// Simple keccak256 wrapper for commit-reveal verification
fn keccak256_hash(data: &[u8]) -> [u8; 32] {
    use call_crypto::keccak256;
    keccak256(data).into()
}

// ─── Errors ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("transaction too large: {size} > {max}")]
    TxTooLarge { size: usize, max: usize },
    #[error("too many instructions: {count} > {max}")]
    TooManyInstructions { count: usize, max: usize },
    #[error("batch transfer too large: {count} > {max}")]
    BatchTooLarge { count: usize, max: usize },
    #[error("too many txs in block: {count} > {max}")]
    TooManyTxsInBlock { count: usize, max: usize },
    #[error("block too large: {size} > {max}")]
    BlockTooLarge { size: usize, max: usize },
    #[error("rate limited")]
    RateLimited,
    #[error("replay detected")]
    ReplayDetected,
    #[error("address saturation limit reached")]
    AddressSaturation,
    #[error("shielded per-block limit exceeded: {count} > {max}")]
    ShieldedPerBlockLimit { count: usize, max: usize },
    #[error("nullifier double-spend detected")]
    NullifierDoubleSpend,
    #[error("message too large: {size} > {max}")]
    MessageTooLarge { size: usize, max: usize },
    #[error("peer rate limited")]
    PeerRateLimited,
    #[error("double-sign detected for validator {validator_id} at round {round}")]
    DoubleSign { validator_id: ValidatorId, round: u64 },
    #[error("duplicate commitment")]
    DuplicateCommitment,
    #[error("commitment not found")]
    CommitmentNotFound,
    #[error("already revealed")]
    AlreadyRevealed,
    #[error("commitment mismatch")]
    CommitmentMismatch,
}

// ─── Tests ─────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::TxHash;

    #[test]
    fn test_mempool_tx_flood_protection() {
        let mut defense = MempoolDefense::new(5, 1000, 10000, 100);

        // First 5 txs should pass within window
        for i in 0..5 {
            let result = defense.validate_tx_submission(
                Address::repeat_byte(1),
                TxHash::repeat_byte(i as u8),
                1000,
            );
            assert!(result.is_ok());
        }

        // 6th tx should be rate limited
        let result = defense.validate_tx_submission(
            Address::repeat_byte(1),
            TxHash::repeat_byte(5),
            1000,
        );
        assert!(matches!(result, Err(SecurityError::RateLimited)));

        // New window: tx should pass again
        let result = defense.validate_tx_submission(
            Address::repeat_byte(1),
            TxHash::repeat_byte(6),
            2001,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_mempool_address_saturation_protection() {
        let mut defense = MempoolDefense::new(1000, 1000, 10000, 5);

        // 5 txs from same address should pass
        for i in 0..5 {
            let result = defense.validate_tx_submission(
                Address::repeat_byte(1),
                TxHash::repeat_byte(i as u8),
                1000 + i as u64,
            );
            assert!(result.is_ok());
        }

        // 6th should hit saturation
        let result = defense.validate_tx_submission(
            Address::repeat_byte(1),
            TxHash::repeat_byte(5),
            1006,
        );
        assert!(matches!(result, Err(SecurityError::AddressSaturation)));

        // Different address should still work
        let result = defense.validate_tx_submission(
            Address::repeat_byte(2),
            TxHash::repeat_byte(6),
            1007,
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_mempool_replay_protection() {
        let mut defense = MempoolDefense::new(1000, 1000, 10000, 1000);

        let hash = TxHash::repeat_byte(42);

        // First submission should pass
        let result = defense.validate_tx_submission(Address::repeat_byte(1), hash, 1000);
        assert!(result.is_ok());

        // Replay should be detected
        let result = defense.validate_tx_submission(
            Address::repeat_byte(1),
            hash,
            1001,
        );
        assert!(matches!(result, Err(SecurityError::ReplayDetected)));
    }

    #[test]
    fn test_shielded_per_block_limit() {
        let mut defense = ShieldedDefense::new(5);

        defense.begin_block();

        // Valid: 2 nullifiers
        let nf1 = [1u8; 32];
        let nf2 = [2u8; 32];
        assert!(defense.validate_shielded_tx(&[nf1, nf2]).is_ok());

        // Valid: 2 more nullifiers (total 4)
        let nf3 = [3u8; 32];
        let nf4 = [4u8; 32];
        assert!(defense.validate_shielded_tx(&[nf3, nf4]).is_ok());

        // Exceeds limit: 2 more would make 6 > 5
        let nf5 = [5u8; 32];
        let nf6 = [6u8; 32];
        let err = defense.validate_shielded_tx(&[nf5, nf6]).unwrap_err();
        assert!(matches!(err, SecurityError::ShieldedPerBlockLimit { .. }));
    }

    #[test]
    fn test_shielded_nullifier_double_spend() {
        let mut defense = ShieldedDefense::new(100);

        defense.begin_block();

        let nf = [42u8; 32];

        // First use should pass
        assert!(defense.validate_shielded_tx(&[nf]).is_ok());

        // Second use (double-spend) should fail
        let err = defense.validate_shielded_tx(&[nf]).unwrap_err();
        assert!(matches!(err, SecurityError::NullifierDoubleSpend));

        // Nullifier persists across blocks
        defense.begin_block();
        let err = defense.validate_shielded_tx(&[nf]).unwrap_err();
        assert!(matches!(err, SecurityError::NullifierDoubleSpend));
    }

    #[test]
    fn test_p2p_rate_limiting() {
        let mut p2p = P2PDefense::new(3, 1000, 1024 * 1024);

        // First 3 messages should pass
        for i in 0..3 {
            assert!(p2p
                .validate_message("peer1".to_string(), 100, 1000 + i * 10)
                .is_ok());
        }

        // 4th should be rate limited
        let err = p2p
            .validate_message("peer1".to_string(), 100, 1030)
            .unwrap_err();
        assert!(matches!(err, SecurityError::PeerRateLimited));

        // Different peer should work
        assert!(p2p
            .validate_message("peer2".to_string(), 100, 1030)
            .is_ok());

        // New window for peer1
        assert!(p2p
            .validate_message("peer1".to_string(), 100, 2001)
            .is_ok());

        // Large message should be rejected
        let err = p2p
            .validate_message("peer3".to_string(), 2 * 1024 * 1024, 2002)
            .unwrap_err();
        assert!(matches!(err, SecurityError::MessageTooLarge { .. }));
    }

    #[test]
    fn test_consensus_double_sign_slash() {
        let mut defense = ConsensusDefense::new();

        let validator = 5u32;
        let round = 100u64;
        let block_a = [1u8; 32];
        let block_b = [2u8; 32];

        // First block should be fine
        assert!(defense
            .record_block_signature(validator, round, block_a)
            .is_none());

        // Same block again (re-signing) should be fine
        assert!(defense
            .record_block_signature(validator, round, block_a)
            .is_none());

        // Different block at same round = double sign!
        let evidence = defense
            .record_block_signature(validator, round, block_b)
            .expect("should detect double sign");
        assert_eq!(evidence.validator_id, validator);
        assert_eq!(evidence.round, round);
        assert_eq!(evidence.block_hash_a, block_a);
        assert_eq!(evidence.block_hash_b, block_b);

        // Validator should be marked as slashed
        assert!(defense.is_slashed(validator));
        assert_eq!(defense.slashed_count(), 1);

        // After slashing, further signs should be ignored
        assert!(defense
            .record_block_signature(validator, round + 1, [3u8; 32])
            .is_none());
    }
}
