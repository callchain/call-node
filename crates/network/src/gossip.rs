//! T7.1 — Gossipsub Transaction Propagation (per spec §8.2, §17.2)
//!
//! Transaction priority ordering, LRU deduplication cache, and gossip channels.

use call_primitives::TxHash;
use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use crate::limits::{NetworkError, NetworkLimits};

// ── Channel IDs (per spec §8.1) ───────────────────────────────────────

/// P2P communication channels
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u64)]
pub enum ChannelId {
    /// High-priority channel for consensus messages
    Consensus = 0,
    /// High-priority channel for protocol transactions
    ProtocolTx = 1,
    /// Standard priority channel for EVM transactions
    EvmTx = 2,
    /// State sync request-response channel
    StateSync = 3,
}

impl ChannelId {
    /// All available channels
    pub fn all() -> &'static [Self] {
        &[Self::Consensus, Self::ProtocolTx, Self::EvmTx, Self::StateSync]
    }
}

// ── Transaction Priority (per spec §8.2) ─────────────────────────────

/// Priority levels for gossipsub propagation (per spec §8.2)
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
#[repr(u8)]
pub enum TxPriority {
    /// Protocol transactions: high priority (payments first)
    High = 0,
    /// EVM transactions: standard priority
    Standard = 1,
}

/// A transaction ready for gossipsub propagation
#[derive(Debug, Clone)]
pub struct PropagatedTx {
    /// Raw transaction bytes (RLP encoded)
    pub data: Vec<u8>,
    /// Transaction hash for deduplication
    pub hash: TxHash,
    /// Priority level for gossipsub
    pub priority: TxPriority,
    /// Timestamp when this tx was first seen
    pub seen_at: Instant,
}

impl PropagatedTx {
    pub fn new(data: Vec<u8>, hash: TxHash, priority: TxPriority) -> Self {
        Self {
            data,
            hash,
            priority,
            seen_at: Instant::now(),
        }
    }
}

// ── LRU Deduplication Cache ──────────────────────────────────────────

/// LRU cache for known transaction hashes (deduplication)
pub struct KnownTxsCache {
    /// Hash -> timestamp of when the tx was first seen
    entries: HashMap<TxHash, Instant>,
    /// Eviction order (oldest first)
    order: VecDeque<TxHash>,
    /// Maximum cache size
    capacity: usize,
}

impl KnownTxsCache {
    /// Create a new cache with the given capacity
    pub fn new(capacity: usize) -> Self {
        Self {
            entries: HashMap::with_capacity(capacity),
            order: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Create from network limits config
    pub fn from_limits(limits: &NetworkLimits) -> Self {
        Self::new(limits.known_txs_cache_size as usize)
    }

    /// Check if a transaction is known
    pub fn contains(&self, hash: &TxHash) -> bool {
        self.entries.contains_key(hash)
    }

    /// Insert a transaction hash. Returns true if it was new (not previously known).
    pub fn insert(&mut self, hash: TxHash) -> bool {
        if self.entries.contains_key(&hash) {
            return false;
        }

        // Evict oldest entry if at capacity
        if self.entries.len() >= self.capacity {
            if let Some(oldest) = self.order.pop_front() {
                self.entries.remove(&oldest);
            }
        }

        let now = Instant::now();
        self.entries.insert(hash, now);
        self.order.push_back(hash);
        true
    }

    /// Remove a transaction from the cache
    pub fn remove(&mut self, hash: &TxHash) -> bool {
        if self.entries.remove(hash).is_some() {
            self.order.retain(|h| h != hash);
            true
        } else {
            false
        }
    }

    /// Current cache size
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache is empty
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

// ── Rate Limiter (per spec §13.5.4) ──────────────────────────────────

/// Token bucket rate limiter per peer
pub struct RateLimiter {
    /// Maximum tokens (messages) allowed
    max_tokens: u32,
    /// Refill rate: tokens per second
    refill_rate: f64,
    /// Current available tokens
    tokens: f64,
    /// Last refill timestamp
    last_refill: Instant,
}

impl RateLimiter {
    /// Create a new rate limiter
    pub fn new(max_messages_per_second: u32) -> Self {
        Self {
            max_tokens: max_messages_per_second,
            refill_rate: max_messages_per_second as f64,
            tokens: max_messages_per_second as f64,
            last_refill: Instant::now(),
        }
    }

    /// Try to consume a token. Returns true if allowed.
    pub fn try_consume(&mut self) -> bool {
        self.refill();
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    /// Refill tokens based on elapsed time
    fn refill(&mut self) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.last_refill = now;
        self.tokens = (self.tokens + elapsed * self.refill_rate)
            .min(self.max_tokens as f64);
    }

    /// Reset the rate limiter
    pub fn reset(&mut self) {
        self.tokens = self.max_tokens as f64;
        self.last_refill = Instant::now();
    }
}

// ── Peer State ────────────────────────────────────────────────────────

/// State for a single connected peer
pub struct PeerState {
    /// Peer identifier
    pub peer_id: String,
    /// Rate limiter for this peer
    pub rate_limiter: RateLimiter,
    /// Time when this peer was connected
    pub connected_at: Instant,
    /// Total messages received from this peer
    pub messages_received: u64,
    /// Whether this peer is currently banned
    pub banned: bool,
    /// Reason for ban (if banned)
    pub ban_reason: Option<String>,
}

impl PeerState {
    pub fn new(peer_id: String, limits: &NetworkLimits) -> Self {
        Self {
            peer_id,
            rate_limiter: RateLimiter::new(limits.max_messages_per_second),
            connected_at: Instant::now(),
            messages_received: 0,
            banned: false,
            ban_reason: None,
        }
    }

    /// Ban this peer
    pub fn ban(&mut self, reason: String) {
        self.banned = true;
        self.ban_reason = Some(reason);
    }

    /// Unban this peer
    pub fn unban(&mut self) {
        self.banned = false;
        self.ban_reason = None;
        self.rate_limiter.reset();
    }

    /// Record an incoming message, checking rate limit
    pub fn record_message(&mut self) -> Result<(), NetworkError> {
        if self.banned {
            return Err(NetworkError::PeerBanned {
                reason: self
                    .ban_reason
                    .clone()
                    .unwrap_or_else(|| "unknown".into()),
            });
        }
        if !self.rate_limiter.try_consume() {
            return Err(NetworkError::RateLimitExceeded);
        }
        self.messages_received += 1;
        Ok(())
    }
}

// ── Gossip Manager ───────────────────────────────────────────────────

/// Manages transaction propagation via gossipsub channels
pub struct GossipManager {
    /// Known transaction hashes (dedup cache)
    pub known_txs: KnownTxsCache,
    /// Per-peer rate limiters
    peers: HashMap<String, PeerState>,
    /// Network limits
    limits: NetworkLimits,
    /// Pending transactions to propagate (ordered by priority)
    pending: Vec<PropagatedTx>,
}

impl GossipManager {
    /// Create a new gossip manager
    pub fn new(limits: NetworkLimits) -> Self {
        Self {
            known_txs: KnownTxsCache::from_limits(&limits),
            peers: HashMap::new(),
            limits,
            pending: Vec::new(),
        }
    }

    /// Register a new peer
    pub fn add_peer(&mut self, peer_id: String) -> Result<(), NetworkError> {
        if self.peers.len() >= self.limits.max_peers as usize {
            return Err(NetworkError::PeerLimitReached {
                current: self.peers.len(),
                max: self.limits.max_peers as usize,
            });
        }
        self.peers
            .insert(peer_id.clone(), PeerState::new(peer_id, &self.limits));
        Ok(())
    }

    /// Remove a peer
    pub fn remove_peer(&mut self, peer_id: &str) {
        self.peers.remove(peer_id);
    }

    /// Process an incoming transaction from a peer.
    /// Returns the transaction if it's new and valid.
    pub fn process_incoming_tx(
        &mut self,
        peer_id: &str,
        data: Vec<u8>,
        hash: TxHash,
        priority: TxPriority,
    ) -> Result<Option<PropagatedTx>, NetworkError> {
        // Check peer exists and is not banned
        let peer = self
            .peers
            .get_mut(peer_id)
            .ok_or_else(|| NetworkError::PeerNotFound {
                peer_id: peer_id.to_string(),
            })?;
        peer.record_message()?;

        // Check message size
        if data.len() > self.limits.max_message_size as usize {
            return Err(NetworkError::MessageTooLarge {
                size: data.len(),
                max: self.limits.max_message_size as usize,
            });
        }

        // Deduplication check
        if !self.known_txs.insert(hash) {
            return Ok(None); // Already known
        }

        let tx = PropagatedTx::new(data, hash, priority);
        Ok(Some(tx))
    }

    /// Queue a transaction for propagation
    pub fn queue_for_propagation(&mut self, tx: PropagatedTx) {
        self.pending.push(tx);
    }

    /// Get pending transactions sorted by priority (high first)
    pub fn drain_pending(&mut self) -> Vec<PropagatedTx> {
        // Sort by priority (high < standard in ordinal, so high comes first)
        self.pending.sort_by_key(|tx| tx.priority);
        std::mem::take(&mut self.pending)
    }

    /// Get the number of connected peers
    pub fn peer_count(&self) -> usize {
        self.peers.len()
    }

    /// Get known tx count
    pub fn known_tx_count(&self) -> usize {
        self.known_txs.len()
    }

    /// Get network limits
    pub fn limits(&self) -> &NetworkLimits {
        &self.limits
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::TxHash;
    use std::time::Duration;

    fn test_hash(n: u8) -> TxHash {
        TxHash::repeat_byte(n)
    }

    #[test]
    fn test_channel_ids() {
        assert_eq!(ChannelId::Consensus as u64, 0);
        assert_eq!(ChannelId::ProtocolTx as u64, 1);
        assert_eq!(ChannelId::EvmTx as u64, 2);
        assert_eq!(ChannelId::StateSync as u64, 3);
        assert_eq!(ChannelId::all().len(), 4);
    }

    #[test]
    fn test_tx_priority_ordering() {
        assert!(TxPriority::High < TxPriority::Standard);
    }

    #[test]
    fn test_known_txs_dedup() {
        let mut cache = KnownTxsCache::new(100);
        let hash = test_hash(1);

        // First insert should be new
        assert!(cache.insert(hash));
        assert_eq!(cache.len(), 1);

        // Second insert should be duplicate
        assert!(!cache.insert(hash));
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn test_known_txs_cache_eviction() {
        let mut cache = KnownTxsCache::new(3);

        // Fill to capacity
        assert!(cache.insert(test_hash(1)));
        assert!(cache.insert(test_hash(2)));
        assert!(cache.insert(test_hash(3)));
        assert_eq!(cache.len(), 3);

        // Insert one more — should evict oldest
        assert!(cache.insert(test_hash(4)));
        assert_eq!(cache.len(), 3);
        assert!(!cache.contains(&test_hash(1))); // evicted
        assert!(cache.contains(&test_hash(4)));
    }

    #[test]
    fn test_rate_limiter_allow() {
        let mut limiter = RateLimiter::new(10);
        // Should allow first 10 messages
        for _ in 0..10 {
            assert!(limiter.try_consume());
        }
        // 11th should fail
        assert!(!limiter.try_consume());
    }

    #[test]
    fn test_rate_limiter_refill() {
        let mut limiter = RateLimiter::new(10);
        // Exhaust tokens
        for _ in 0..10 {
            limiter.try_consume();
        }
        assert!(!limiter.try_consume());

        // Wait for refill (at 10/sec, should refill some in 200ms)
        std::thread::sleep(Duration::from_millis(200));
        assert!(limiter.try_consume());
    }

    #[test]
    fn test_peer_state_ban() {
        let limits = NetworkLimits::default();
        let mut peer = PeerState::new("peer_1".into(), &limits);

        // Record some messages
        for _ in 0..5 {
            peer.record_message().unwrap();
        }
        assert_eq!(peer.messages_received, 5);

        // Ban peer
        peer.ban("spam".into());
        assert!(peer.record_message().is_err());

        // Unban
        peer.unban();
        assert!(peer.record_message().is_ok());
    }

    #[test]
    fn test_gossip_manager_peer_limit() {
        let limits = NetworkLimits::new(3, 100, 1024, 100, 3600);
        let mut manager = GossipManager::new(limits);

        assert!(manager.add_peer("p1".into()).is_ok());
        assert!(manager.add_peer("p2".into()).is_ok());
        assert!(manager.add_peer("p3".into()).is_ok());
        assert!(manager.add_peer("p4".into()).is_err()); // limit reached
    }

    #[test]
    fn test_gossip_manager_process_incoming_tx() {
        let limits = NetworkLimits::default();
        let mut manager = GossipManager::new(limits);
        manager.add_peer("peer_1".into()).unwrap();

        let tx = manager
            .process_incoming_tx(
                "peer_1",
                vec![0u8; 100],
                test_hash(1),
                TxPriority::High,
            )
            .unwrap();
        assert!(tx.is_some());

        // Duplicate should be filtered
        let tx2 = manager
            .process_incoming_tx(
                "peer_1",
                vec![0u8; 100],
                test_hash(1),
                TxPriority::High,
            )
            .unwrap();
        assert!(tx2.is_none());
    }

    #[test]
    fn test_gossip_manager_max_message_size() {
        let limits = NetworkLimits::new(10, 100, 1024, 1000, 3600); // 1 KB max
        let mut manager = GossipManager::new(limits);
        manager.add_peer("peer_1".into()).unwrap();

        let result = manager.process_incoming_tx(
            "peer_1",
            vec![0u8; 2048], // 2 KB — exceeds limit
            test_hash(1),
            TxPriority::High,
        );
        assert!(matches!(result, Err(NetworkError::MessageTooLarge { .. })));
    }

    #[test]
    fn test_gossip_manager_propagation_priority() {
        let limits = NetworkLimits::default();
        let mut manager = GossipManager::new(limits);

        // Queue transactions with mixed priorities
        manager.queue_for_propagation(PropagatedTx::new(
            vec![1],
            test_hash(3),
            TxPriority::Standard,
        ));
        manager.queue_for_propagation(PropagatedTx::new(
            vec![2],
            test_hash(1),
            TxPriority::High,
        ));
        manager.queue_for_propagation(PropagatedTx::new(
            vec![3],
            test_hash(4),
            TxPriority::Standard,
        ));
        manager.queue_for_propagation(PropagatedTx::new(
            vec![4],
            test_hash(2),
            TxPriority::High,
        ));

        // Drain should return in priority order (High before Standard)
        let drained = manager.drain_pending();
        assert_eq!(drained.len(), 4);
        // First two should be High priority
        assert_eq!(drained[0].priority, TxPriority::High);
        assert_eq!(drained[1].priority, TxPriority::High);
        // Last two should be Standard priority
        assert_eq!(drained[2].priority, TxPriority::Standard);
        assert_eq!(drained[3].priority, TxPriority::Standard);
    }

    #[test]
    fn test_gossip_manager_peer_removal() {
        let limits = NetworkLimits::default();
        let mut manager = GossipManager::new(limits);
        manager.add_peer("p1".into()).unwrap();
        manager.add_peer("p2".into()).unwrap();
        assert_eq!(manager.peer_count(), 2);

        manager.remove_peer("p1");
        assert_eq!(manager.peer_count(), 1);

        // Sending from removed peer should fail
        let result = manager.process_incoming_tx(
            "p1",
            vec![1],
            test_hash(1),
            TxPriority::High,
        );
        assert!(matches!(result, Err(NetworkError::PeerNotFound { .. })));
    }
}
