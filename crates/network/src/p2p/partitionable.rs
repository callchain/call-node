//! Partitionable network implementation for network partition / Byzantine testing.
//!
//! Wraps `InMemoryNetwork` with per-node message filtering, partition groups,
//! configurable drop rates, and deterministic routing.  Used by E2E tests that
//! simulate network splits, message loss, and malicious peers.

use crate::limits::NetworkError;
use crate::p2p::trait_::Network;
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// A queued message with its scheduled delivery time (for latency simulation).
type TimedMessage = (String, u64, Vec<u8>, Instant);

/// Shared routing state for a partitioned network.
///
/// Every node in the simulation holds a clone of the same `PartitionRouter`.
/// The router maintains a separate receive buffer for each node and applies
/// partition / drop / delay rules at enqueue time.
pub struct PartitionRouter {
    /// Per-node receive buffers: node_id -> FIFO queue of (sender, channel, data, deliver_after)
    buffers: Mutex<HashMap<String, VecDeque<TimedMessage>>>,
    /// node_id -> partition_group_id.  Nodes in different groups cannot exchange
    /// messages (with the exception of `send_to` when the caller explicitly
    /// targets a cross-group peer – that is also blocked).
    partition_groups: Mutex<HashMap<String, String>>,
    /// Global drop rate as a fraction of 1_000_000 (e.g. 100_000 = 10 %).
    drop_rate: AtomicU64,
    /// Base latency in milliseconds applied to every delivered message.
    delay_ms: AtomicU64,
    /// Deterministic PRNG seed for drop decisions.  Incremented on every
    /// broadcast so tests are reproducible when run single-threaded.
    seed: AtomicU64,
}

impl PartitionRouter {
    pub fn new() -> Self {
        Self {
            buffers: Mutex::new(HashMap::new()),
            partition_groups: Mutex::new(HashMap::new()),
            drop_rate: AtomicU64::new(0),
            delay_ms: AtomicU64::new(0),
            seed: AtomicU64::new(1),
        }
    }

    /// Register a node so that it has its own receive buffer.
    pub fn register_node(&self, node_id: &str) {
        let mut buffers = self.buffers.lock().expect("lock poisoned");
        buffers.entry(node_id.to_string()).or_default();
    }

    /// Set the partition group for a node.
    pub fn set_partition_group(&self, node_id: &str, group: &str) {
        let mut groups = self.partition_groups.lock().expect("lock poisoned");
        groups.insert(node_id.to_string(), group.to_string());
    }

    /// Get the partition group for a node.
    pub fn get_partition_group(&self, node_id: &str) -> Option<String> {
        let groups = self.partition_groups.lock().expect("lock poisoned");
        groups.get(node_id).cloned()
    }

    /// Set global drop rate (0.0 .. 1.0).
    pub fn set_drop_rate(&self, rate: f64) {
        let scaled = (rate.clamp(0.0, 1.0) * 1_000_000.0) as u64;
        self.drop_rate.store(scaled, Ordering::Relaxed);
    }

    /// Set base latency (delay) in milliseconds applied to every delivered message.
    pub fn set_delay_ms(&self, ms: u64) {
        self.delay_ms.store(ms, Ordering::Relaxed);
    }

    /// Return true if the next message should be dropped.
    fn should_drop(&self) -> bool {
        let rate = self.drop_rate.load(Ordering::Relaxed);
        if rate == 0 {
            return false;
        }
        let s = self.seed.fetch_add(1, Ordering::Relaxed);
        // Simple LCG for deterministic “randomness” in tests.
        let next = s.wrapping_mul(1103515245).wrapping_add(12345);
        (next % 1_000_000) < rate
    }

    /// Compute the scheduled delivery time for a message given current delay settings.
    fn delivery_time(&self) -> Instant {
        let ms = self.delay_ms.load(Ordering::Relaxed);
        Instant::now() + Duration::from_millis(ms)
    }

    /// Deliver a message from `from_node` to `to_node` if they are in the same
    /// partition group and the drop dice allow it.
    fn deliver(&self, from_node: &str, to_node: &str, channel: u64, data: Vec<u8>) {
        if from_node != to_node {
            let groups = self.partition_groups.lock().expect("lock poisoned");
            let from_group = groups.get(from_node);
            let to_group = groups.get(to_node);
            if from_group != to_group {
                return; // partitioned
            }
        }
        if self.should_drop() {
            return;
        }
        let mut buffers = self.buffers.lock().expect("lock poisoned");
        if let Some(q) = buffers.get_mut(to_node) {
            q.push_back((from_node.to_string(), channel, data, self.delivery_time()));
        }
    }

    /// Broadcast `data` on `channel` from `from_node` to every registered node.
    pub fn broadcast(&self, from_node: &str, channel: u64, data: Vec<u8>) {
        let nodes: Vec<String> = {
            let buffers = self.buffers.lock().expect("lock poisoned");
            buffers.keys().cloned().collect()
        };
        for node in nodes {
            self.deliver(from_node, &node, channel, data.clone());
        }
    }

    /// Send `data` on `channel` from `from_node` to specific peers.
    pub fn send_to(&self, from_node: &str, channel: u64, peers: &[String], data: Vec<u8>) {
        for peer in peers {
            self.deliver(from_node, peer, channel, data.clone());
        }
    }

    /// Pop the oldest *deliverable* message for `node_id`, if any.
    /// Messages whose delay has not yet elapsed remain in the buffer.
    pub fn receive(&self, node_id: &str) -> Option<(String, u64, Vec<u8>)> {
        let now = Instant::now();
        let mut buffers = self.buffers.lock().expect("lock poisoned");
        let q = buffers.get_mut(node_id)?;
        if q.front().map(|m| m.3 <= now).unwrap_or(false) {
            let (s, c, d, _) = q.pop_front()?;
            Some((s, c, d))
        } else {
            None
        }
    }

    /// Number of messages waiting for `node_id` (including delayed ones).
    pub fn pending_count(&self, node_id: &str) -> usize {
        let buffers = self.buffers.lock().expect("lock poisoned");
        buffers.get(node_id).map(|q| q.len()).unwrap_or(0)
    }

    /// Number of *deliverable* messages (delay elapsed) for `node_id`.
    pub fn deliverable_count(&self, node_id: &str) -> usize {
        let now = Instant::now();
        let buffers = self.buffers.lock().expect("lock poisoned");
        buffers
            .get(node_id)
            .map(|q| q.iter().take_while(|m| m.3 <= now).count())
            .unwrap_or(0)
    }

    /// Drain all *deliverable* messages for `node_id`. Delayed messages remain.
    pub fn drain(&self, node_id: &str) -> Vec<(String, u64, Vec<u8>)> {
        let now = Instant::now();
        let mut buffers = self.buffers.lock().expect("lock poisoned");
        let q = match buffers.get_mut(node_id) {
            Some(q) => q,
            None => return Vec::new(),
        };
        // Split at the first delayed message
        let split_idx = q.iter().position(|m| m.3 > now).unwrap_or(q.len());
        q.drain(..split_idx).map(|(s, c, d, _)| (s, c, d)).collect()
    }

    /// Remove all *deliverable* messages from every buffer. Delayed messages remain.
    pub fn drain_all(&self) -> Vec<(String, u64, Vec<u8>)> {
        let now = Instant::now();
        let mut buffers = self.buffers.lock().expect("lock poisoned");
        let mut out = Vec::new();
        for q in buffers.values_mut() {
            let split_idx = q.iter().position(|m| m.3 > now).unwrap_or(q.len());
            out.extend(q.drain(..split_idx).map(|(s, c, d, _)| (s, c, d)));
        }
        out
    }
}

impl Default for PartitionRouter {
    fn default() -> Self {
        Self::new()
    }
}

/// Per-node handle that implements the `Network` trait over a `PartitionRouter`.
///
/// Created via `PartitionSimulator::add_node` (test harness) or directly when
/// you need fine-grained control.
pub struct PartitionableNetwork {
    pub node_id: String,
    router: Arc<PartitionRouter>,
    /// Fallback peers list (used for `peer_count` / `peer_ids`).
    peers: Mutex<Vec<String>>,
}

impl PartitionableNetwork {
    pub fn new(node_id: String, router: Arc<PartitionRouter>) -> Self {
        router.register_node(&node_id);
        Self {
            node_id,
            router,
            peers: Mutex::new(Vec::new()),
        }
    }

    /// Inject a peer into the local peer list (affects `peer_count` / `peer_ids`).
    pub fn add_peer(&self, peer_id: String) {
        let mut peers = self.peers.lock().expect("lock poisoned");
        if !peers.contains(&peer_id) {
            peers.push(peer_id);
        }
    }

    /// Remove a peer from the local peer list.
    pub fn remove_peer(&self, peer_id: &str) {
        let mut peers = self.peers.lock().expect("lock poisoned");
        peers.retain(|p| p != peer_id);
    }
}

#[async_trait::async_trait]
impl Network for PartitionableNetwork {
    async fn broadcast(&self, channel: u64, message: Vec<u8>) {
        let _ = self.try_broadcast(channel, message).await;
    }

    async fn try_broadcast(&self, channel: u64, message: Vec<u8>) -> Result<(), NetworkError> {
        self.router.broadcast(&self.node_id, channel, message);
        Ok(())
    }

    async fn send_to(&self, channel: u64, peers: Vec<String>, message: Vec<u8>) {
        self.router.send_to(&self.node_id, channel, &peers, message);
    }

    async fn receive(&self) -> Result<(String, u64, Vec<u8>), NetworkError> {
        self.router
            .receive(&self.node_id)
            .ok_or_else(|| NetworkError::NetworkError("no messages".into()))
    }

    fn peer_count(&self) -> usize {
        self.peers.lock().map(|p| p.len()).unwrap_or(0)
    }

    fn peer_ids(&self) -> Vec<String> {
        self.peers.lock().map(|p| p.clone()).unwrap_or_default()
    }

    async fn connect(&self, address: &str) -> Result<(), NetworkError> {
        self.add_peer(address.to_string());
        Ok(())
    }

    async fn disconnect(&self, peer_id: &str) -> Result<(), NetworkError> {
        self.remove_peer(peer_id);
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.peer_count() > 0
    }
}
