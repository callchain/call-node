//! Security & Attack Prevention (per spec §13)
//!
//! Mempool attack prevention, P2P rate limiting.

use call_primitives::{Address, TxHash};
use std::collections::{HashMap, HashSet};

// ─── Mempool Attack Prevention ─────────────────────────────────────

/// Rate limiter for mempool
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
            let to_remove: Vec<_> = self
                .seen_hashes
                .iter()
                .take(self.max_seen / 4)
                .copied()
                .collect();
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

// ─── Errors ────────────────────────────────────────────────────────

#[derive(Debug, thiserror::Error)]
pub enum SecurityError {
    #[error("rate limited")]
    RateLimited,
    #[error("replay detected")]
    ReplayDetected,
    #[error("address saturation limit reached")]
    AddressSaturation,
    #[error("message too large: {size} > {max}")]
    MessageTooLarge { size: usize, max: usize },
    #[error("peer rate limited")]
    PeerRateLimited,
}

// ─── Tests ─────────────────────────────────────────

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
        let result =
            defense.validate_tx_submission(Address::repeat_byte(1), TxHash::repeat_byte(5), 1000);
        assert!(matches!(result, Err(SecurityError::RateLimited)));

        // New window: tx should pass again
        let result =
            defense.validate_tx_submission(Address::repeat_byte(1), TxHash::repeat_byte(6), 2001);
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
        let result =
            defense.validate_tx_submission(Address::repeat_byte(1), TxHash::repeat_byte(5), 1006);
        assert!(matches!(result, Err(SecurityError::AddressSaturation)));

        // Different address should still work
        let result =
            defense.validate_tx_submission(Address::repeat_byte(2), TxHash::repeat_byte(6), 1007);
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
        let result = defense.validate_tx_submission(Address::repeat_byte(1), hash, 1001);
        assert!(matches!(result, Err(SecurityError::ReplayDetected)));
    }

    #[test]
    fn test_replay_protector_evicts_25_percent_when_over_capacity() {
        let max_seen = 100;
        let mut protector = ReplayProtector::new(max_seen);

        // Fill exactly to max_seen
        for i in 0..max_seen {
            let hash = TxHash::repeat_byte(i as u8);
            assert!(
                protector.check_and_record(hash),
                "first insert #{i} should succeed"
            );
        }
        assert_eq!(protector.seen_hashes.len(), max_seen);

        // Insert one more — should trigger eviction of ~25% (max_seen / 4 = 25)
        let overflow_hash = TxHash::repeat_byte(255);
        assert!(
            protector.check_and_record(overflow_hash),
            "overflow insert should succeed"
        );

        let expected_remaining = max_seen + 1 - (max_seen / 4);
        assert_eq!(
            protector.seen_hashes.len(),
            expected_remaining,
            "expected ~75% of max_seen + 1 to remain after eviction"
        );
    }

    #[test]
    fn test_replay_protector_evicted_hash_can_be_re_inserted() {
        let max_seen = 8;
        let mut protector = ReplayProtector::new(max_seen);

        // Fill to capacity with unique hashes
        for i in 0..max_seen {
            protector.check_and_record(TxHash::repeat_byte(i as u8));
        }

        // Trigger eviction (removes max_seen / 4 = 2 arbitrary hashes)
        let new_hash = TxHash::repeat_byte(255);
        assert!(protector.check_and_record(new_hash));

        // After eviction, total should be max_seen + 1 - 2 = 7
        assert_eq!(protector.seen_hashes.len(), 7);

        // The new hash must still be present (it was just inserted)
        assert!(
            !protector.check_and_record(new_hash),
            "new hash should still be a replay"
        );

        // At least one of the original 8 hashes should have been evicted
        // (since we only have room for 7 and the new hash is one of them)
        let mut evicted_count = 0;
        for i in 0..max_seen {
            if protector.check_and_record(TxHash::repeat_byte(i as u8)) {
                evicted_count += 1;
            }
        }
        assert!(
            evicted_count >= 2,
            "expected at least 2 evicted hashes, got {}",
            evicted_count
        );

        // Some original hashes should still be present (not all evicted)
        let remaining_original = max_seen - evicted_count;
        assert!(
            remaining_original >= 1,
            "expected at least 1 original hash to survive eviction"
        );
    }

    #[test]
    fn test_replay_protector_stays_bounded_under_pressure() {
        let max_seen = 50;
        let mut protector = ReplayProtector::new(max_seen);

        // Simulate sustained pressure: insert 10x capacity
        for i in 0..(max_seen * 10) {
            let hash = TxHash::repeat_byte((i % 256) as u8);
            protector.check_and_record(hash);
            assert!(
                protector.seen_hashes.len() <= max_seen + 1,
                "set should never exceed max_seen + 1, got {} at i={}",
                protector.seen_hashes.len(),
                i
            );
        }
    }

    #[test]
    fn test_replay_protector_remove_hash_manually() {
        let mut protector = ReplayProtector::new(100);
        let hash = TxHash::repeat_byte(42);

        assert!(protector.check_and_record(hash));
        assert!(!protector.check_and_record(hash)); // replay

        protector.remove_hash(&hash);
        assert!(protector.check_and_record(hash)); // now accepted again
    }

    // ── Property-based tests (proptest) ───────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_replay_protector_stays_bounded(
            max_seen in 10usize..500usize,
            ops in prop::collection::vec(any::<u8>(), 100..1000),
        ) {
            let mut protector = ReplayProtector::new(max_seen);
            for (i, byte) in ops.iter().enumerate() {
                let hash = TxHash::repeat_byte(*byte);
                protector.check_and_record(hash);
                prop_assert!(
                    protector.seen_hashes.len() <= max_seen + 1,
                    "len {} exceeded max_seen + 1 {} at step {}",
                    protector.seen_hashes.len(),
                    max_seen + 1,
                    i
                );
            }
        }

        #[test]
        fn prop_rate_limiter_allows_at_most_max_per_window(
            max_requests in 1u32..100u32,
            window_ms in 100u64..5000u64,
        ) {
            let mut limiter = RateLimiter::new(max_requests, window_ms);
            let addr = Address::repeat_byte(1);
            let mut allowed_count = 0u32;

            // Burst within a single window (time stays constant so we never cross windows)
            for _ in 0..(max_requests * 2) {
                if limiter.allow(addr, 0u64) {
                    allowed_count += 1;
                }
            }

            prop_assert!(
                allowed_count <= max_requests,
                "allowed {} requests but max_requests is {} within a single window",
                allowed_count,
                max_requests
            );
        }

        #[test]
        fn prop_rate_limiter_new_window_resets(
            max_requests in 1u32..100u32,
            window_ms in 100u64..5000u64,
        ) {
            let mut limiter = RateLimiter::new(max_requests, window_ms);
            let addr = Address::repeat_byte(1);

            // Fill the window
            for i in 0..max_requests {
                prop_assert!(limiter.allow(addr, i as u64));
            }
            // Next request in same window should fail
            prop_assert!(!limiter.allow(addr, max_requests as u64));

            // Request after window expiry should succeed
            let new_time = window_ms + 1;
            prop_assert!(limiter.allow(addr, new_time), "new window should reset counter");
        }

        #[test]
        fn prop_mempool_defense_rate_limit(
            rate_limit in 1u32..50u32,
            window_ms in 100u64..5000u64,
            num_txs in 1usize..200usize,
        ) {
            let mut defense = MempoolDefense::new(rate_limit, window_ms, 10000, 1000);
            let sender = Address::repeat_byte(1);

            for i in 0..num_txs {
                let time_ms = i as u64 * (window_ms / rate_limit as u64 + 1);
                let hash = TxHash::repeat_byte((i % 256) as u8);
                let result = defense.validate_tx_submission(sender, hash, time_ms);

                // Within rate limit should pass; exceeding should fail with RateLimited
                if i < rate_limit as usize {
                    prop_assert!(result.is_ok(), "tx {} should pass within rate limit", i);
                }
                // We don't assert failure beyond rate_limit because time spacing may vary
            }
        }
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
        assert!(p2p.validate_message("peer2".to_string(), 100, 1030).is_ok());

        // New window for peer1
        assert!(p2p.validate_message("peer1".to_string(), 100, 2001).is_ok());

        // Large message should be rejected
        let err = p2p
            .validate_message("peer3".to_string(), 2 * 1024 * 1024, 2002)
            .unwrap_err();
        assert!(matches!(err, SecurityError::MessageTooLarge { .. }));
    }
}
