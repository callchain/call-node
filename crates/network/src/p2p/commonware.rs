//! Real P2P network adapter backed by commonware-p2p.

use crate::gossip::GossipManager;
use crate::limits::NetworkError;
use crate::p2p::config::CommonwareConfig;
use crate::p2p::message::{NetworkMessage, PeerExchange};
use crate::p2p::trait_::Network;
use crate::p2p::wire::{decode_with_channel, encode_with_channel};
use commonware_codec::extensions::DecodeExt;
use commonware_cryptography::{ed25519, Signer};
use commonware_p2p::authenticated::lookup::{self as p2p_lookup, Config as P2PConfig};
use commonware_p2p::{
    Address, AddressableManager, Blocker, PeerSetUpdate, Provider, Receiver, Recipients, Sender,
};
use commonware_runtime::{IoBuf, Metrics, Quota, Runner, Spawner};
use commonware_utils::ordered::Map;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Validate a peer ID advertised in a PEX message.
/// Must be a valid hex-encoded Ed25519 public key (64 hex chars = 32 bytes).
fn validate_pex_peer_id(peer_id: &str) -> bool {
    if peer_id.len() != 64 {
        return false;
    }
    hex::decode(peer_id).is_ok()
}

/// Validate a socket address advertised in a PEX message.
/// Rejects loopback, multicast, unspecified, and link-local addresses.
/// Private IPs are rejected unless `allow_private` is true.
fn validate_pex_address(addr: SocketAddr, allow_private: bool) -> bool {
    let ip = addr.ip();
    if ip.is_multicast() || ip.is_unspecified() {
        return false;
    }
    if ip.is_loopback() && !allow_private {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => {
            if v4.is_link_local() {
                return false;
            }
            if !allow_private && v4.is_private() {
                return false;
            }
        }
        IpAddr::V6(v6) => {
            // Link-local IPv6: fe80::/10
            if (v6.segments()[0] & 0xffc0) == 0xfe80 {
                return false;
            }
        }
    }
    true
}

/// Real P2P network adapter backed by commonware-p2p.
///
/// Wraps commonware-p2p's authenticated lookup network and implements
/// the `Network` trait. Messages are prefixed with a channel byte for
/// multiplexing over a single commonware channel.
///
/// # Usage
/// ```ignore
/// let config = CommonwareConfig::local(SocketAddr::from(([0, 0, 0, 0], 51235)));
/// let network = CommonwareNetwork::new(&config, identity_key).await?;
///
/// // Use the network
/// network.broadcast(1, vec![1, 2, 3]).await;
/// let (peer_id, channel, data) = network.receive().await?;
/// ```
pub struct CommonwareNetwork {
    /// Sender for outgoing messages
    sender: tokio::sync::Mutex<
        p2p_lookup::Sender<ed25519::PublicKey, commonware_runtime::tokio::Context>,
    >,
    /// Receiver for incoming messages (wrapped in async mutex)
    receiver: tokio::sync::Mutex<p2p_lookup::Receiver<ed25519::PublicKey>>,
    /// Oracle for peer management
    oracle: tokio::sync::Mutex<p2p_lookup::Oracle<ed25519::PublicKey>>,
    /// Map of connected peers: hex(public_key) -> socket address
    /// Note: populated as peers are tracked; updated via subscribe
    peers: Arc<tokio::sync::RwLock<std::collections::BTreeMap<String, SocketAddr>>>,
    /// Our own peer ID (hex-encoded ed25519 public key)
    our_peer_id: String,
    /// Local listen address
    listen_addr: SocketAddr,
    /// Shutdown signal sender (sent to background thread on drop)
    shutdown_tx: Option<tokio::sync::oneshot::Sender<()>>,
    /// Background thread running the commonware runtime
    thread_handle: Option<std::thread::JoinHandle<()>>,
    /// Bootstrap peer addresses for reconnection
    bootstrap_peers: Arc<tokio::sync::RwLock<Vec<(String, SocketAddr)>>>,
    /// Minimum healthy peer count
    min_healthy_peers: u32,
    /// Gossip manager for rate limiting, dedup, and peer management
    gossip: Arc<tokio::sync::Mutex<GossipManager>>,
    /// Known peers address book: hex(peer_id) -> SocketAddr (includes bootstrap + discovered)
    known_peers: Arc<tokio::sync::RwLock<std::collections::BTreeMap<String, SocketAddr>>>,
    /// Last PEX received timestamp per peer (rate limiting)
    pex_last_received:
        Arc<tokio::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>>,
    /// PEX configuration fields
    enable_peer_exchange: bool,
    auto_connect_discovered: bool,
    max_known_peers: usize,
    max_pex_peers_per_msg: usize,
    /// Whether to allow private IP addresses (for devnet/testing)
    allow_private_ips: bool,
    /// TTL for PEX-discovered peers (seconds). 0 = no expiry.
    pex_peer_ttl_seconds: u64,
    /// PEX-discovered peers with timestamps (separate from trusted bootstrap peers)
    pex_entries: Arc<tokio::sync::RwLock<std::collections::HashMap<String, (SocketAddr, Instant)>>>,
}

struct InitComponents {
    sender: p2p_lookup::Sender<ed25519::PublicKey, commonware_runtime::tokio::Context>,
    receiver: p2p_lookup::Receiver<ed25519::PublicKey>,
    oracle: p2p_lookup::Oracle<ed25519::PublicKey>,
    peers: Arc<tokio::sync::RwLock<std::collections::BTreeMap<String, SocketAddr>>>,
    our_peer_id: String,
    listen_addr: SocketAddr,
}

impl CommonwareNetwork {
    /// Create and initialize a new commonware-p2p network.
    ///
    /// Spawns a background thread running the commonware runtime.
    /// Returns once the network is ready to send/receive messages.
    ///
    /// The `signer` is the Ed25519 keypair used for this node's identity.
    /// Use [`load_or_generate_identity_key`] for persistent identity.
    pub async fn new(
        config: &CommonwareConfig,
        signer: ed25519::PrivateKey,
    ) -> Result<Self, NetworkError> {
        use commonware_runtime::tokio::{Config as RuntimeConfig, Runner as TokioRunner};

        let listen_addr = config.listen_addr;
        let bootstrap_peers = Arc::new(tokio::sync::RwLock::new(config.bootstrap_peers.clone()));
        let known_peers = Arc::new(tokio::sync::RwLock::new(
            config
                .bootstrap_peers
                .iter()
                .cloned()
                .collect::<std::collections::BTreeMap<_, _>>(),
        ));

        // Channels for returning initialized components and shutdown signal
        let (tx, rx) = tokio::sync::oneshot::channel();
        let (shutdown_tx, shutdown_rx) = tokio::sync::oneshot::channel();

        let cfg = config.clone();
        let bootstrap_peers_clone = Arc::clone(&bootstrap_peers);
        let thread_handle = std::thread::spawn(move || {
            let runtime_cfg = RuntimeConfig::new();
            let runner = TokioRunner::new(runtime_cfg);
            runner.start(|context: commonware_runtime::tokio::Context| async move {
                // Save our peer ID before signer is moved into P2P config
                let our_peer_id = hex::encode(signer.public_key().as_ref());

                // Build P2P config with the provided signer
                let p2p_cfg = if cfg.allow_private_ips {
                    P2PConfig::local(
                        signer,
                        &cfg.namespace,
                        cfg.listen_addr,
                        cfg.max_message_size,
                    )
                } else {
                    P2PConfig::recommended(
                        signer,
                        &cfg.namespace,
                        cfg.listen_addr,
                        cfg.max_message_size,
                    )
                };

                // Create network
                let (mut network, mut oracle) =
                    p2p_lookup::Network::new(context.with_label("network"), p2p_cfg);

                // Register bootstrap peers using commonware_utils::ordered::Map
                if !cfg.bootstrap_peers.is_empty() {
                    let mut peer_entries: Vec<(ed25519::PublicKey, Address)> = Vec::new();
                    for (peer_id_hex, socket_addr) in &cfg.bootstrap_peers {
                        if let Ok(pk_bytes) = hex::decode(peer_id_hex) {
                            if let Ok(pk) = ed25519::PublicKey::decode(&*pk_bytes) {
                                peer_entries.push((pk, Address::Symmetric(*socket_addr)));
                            }
                        }
                    }
                    if !peer_entries.is_empty() {
                        let peer_map: Map<ed25519::PublicKey, Address> =
                            Map::from_iter_dedup(peer_entries);
                        oracle.track(0, peer_map).await;
                    }
                }

                // Register application channel
                let quota = Quota::per_second(NonZeroU32::new(1000).expect("invariant: 1000 > 0"));
                let (sender, receiver) = network.register(0, quota, 10_000);

                // Start the network (spawns background tasks)
                let _handle = network.start();

                // Collect initial peer info
                let peers = Arc::new(tokio::sync::RwLock::new(
                    cfg.bootstrap_peers
                        .iter()
                        .cloned()
                        .collect::<std::collections::BTreeMap<_, _>>(),
                ));

                // Subscribe to peer set changes — use actual addresses from the update
                let peers_clone = peers.clone();
                let mut subscription: tokio::sync::mpsc::UnboundedReceiver<
                    PeerSetUpdate<ed25519::PublicKey>,
                > = oracle.subscribe().await;
                let _subscribe_handle = context.clone().spawn(move |_ctx| async move {
                    while let Some(update) = subscription.recv().await {
                        let mut peers_guard = peers_clone.write().await;
                        peers_guard.clear();
                        let all = update.all.union();
                        for pk in all.into_iter() {
                            // Find the peer's address from the tracked bootstrap config
                            let pk_hex = hex::encode(pk.as_ref());
                            let addr = cfg
                                .bootstrap_peers
                                .iter()
                                .find(|(id, _)| *id == pk_hex)
                                .map(|(_, a)| *a)
                                .unwrap_or(listen_addr);
                            peers_guard.insert(pk_hex, addr);
                        }
                    }
                });

                // Periodic bootstrap peer reconnection — reconnect if bootstrap peers drop
                let bootstrap_reconnect = bootstrap_peers_clone.clone();
                let peers_for_reconnect = Arc::clone(&peers);
                let mut oracle_clone = oracle.clone();
                let _reconnect_handle = context.clone().spawn(move |_ctx| async move {
                    let mut interval = tokio::time::interval(Duration::from_secs(30));
                    loop {
                        interval.tick().await;
                        let connected_peers = {
                            let peers = peers_for_reconnect.read().await;
                            peers.clone()
                        };
                        for (peer_id, addr) in bootstrap_reconnect.read().await.iter() {
                            if !connected_peers.contains_key(peer_id) {
                                tracing::info!(peer_id, ?addr, "reconnecting to bootstrap peer");
                                if let Ok(pk_bytes) = hex::decode(peer_id) {
                                    if let Ok(pk) = ed25519::PublicKey::decode(&*pk_bytes) {
                                        let peer_map: Map<ed25519::PublicKey, Address> =
                                            Map::from_iter_dedup(vec![(
                                                pk,
                                                Address::Symmetric(*addr),
                                            )]);
                                        oracle_clone.track(0, peer_map).await;
                                    }
                                }
                            }
                        }
                    }
                });

                // Send components back to the calling thread
                let _ = tx.send(InitComponents {
                    sender,
                    receiver,
                    oracle,
                    peers,
                    our_peer_id,
                    listen_addr,
                });

                // Wait for shutdown signal
                let _ = shutdown_rx.await;
            });
        });

        // Wait for the network to be initialized
        let components = rx.await.map_err(|e| {
            NetworkError::NetworkError(format!("commonware initialization failed: {e}"))
        })?;

        Ok(Self {
            sender: tokio::sync::Mutex::new(components.sender),
            receiver: tokio::sync::Mutex::new(components.receiver),
            oracle: tokio::sync::Mutex::new(components.oracle),
            peers: components.peers,
            our_peer_id: components.our_peer_id,
            listen_addr: components.listen_addr,
            shutdown_tx: Some(shutdown_tx),
            thread_handle: Some(thread_handle),
            bootstrap_peers,
            min_healthy_peers: cfg.min_healthy_peers,
            gossip: Arc::new(tokio::sync::Mutex::new(GossipManager::new(config.limits))),
            known_peers,
            pex_last_received: Arc::new(tokio::sync::Mutex::new(std::collections::HashMap::new())),
            enable_peer_exchange: config.enable_peer_exchange,
            auto_connect_discovered: config.auto_connect_discovered,
            max_known_peers: config.max_known_peers,
            max_pex_peers_per_msg: config.max_pex_peers_per_msg,
            allow_private_ips: config.allow_private_ips,
            pex_peer_ttl_seconds: config.pex_peer_ttl_seconds,
            pex_entries: Arc::new(tokio::sync::RwLock::new(std::collections::HashMap::new())),
        })
    }

    /// Get our public key as a hex string (our peer ID)
    pub fn peer_id(&self) -> &str {
        &self.our_peer_id
    }

    /// Get the local listen address
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    /// Stop the network (signal the background thread to shut down)
    pub fn stop(&mut self) {
        if let Some(tx) = self.shutdown_tx.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.thread_handle.take() {
            let _ = handle.join();
        }
    }

    /// Send our known peers to all connected peers.
    /// Call this periodically (e.g., every `pex_interval_seconds`) from the node loop.
    pub async fn send_peer_exchange(&self) {
        if !self.enable_peer_exchange {
            return;
        }

        let peers_to_advertise = {
            let bootstrap = self.known_peers.read().await;
            let pex = self.pex_entries.read().await;
            let mut list: Vec<(String, SocketAddr)> = self
                .merge_peers(&bootstrap, &pex)
                .into_iter()
                .filter(|(id, _)| id != &self.our_peer_id)
                .collect();
            // Shuffle to avoid always advertising the same subset
            use rand::seq::SliceRandom;
            list.shuffle(&mut rand::thread_rng());
            list.truncate(self.max_pex_peers_per_msg);
            list
        };

        if peers_to_advertise.is_empty() {
            return;
        }

        let pex = PeerExchange::new(peers_to_advertise, self.listen_addr);
        let msg = NetworkMessage::PeerExchange(pex);
        match postcard::to_allocvec(&msg) {
            Ok(data) => {
                tracing::debug!(peers = %self.peer_count(), "broadcasting PEX");
                self.broadcast(0, data).await;
            }
            Err(e) => {
                tracing::warn!("failed to serialize PEX message: {e}");
            }
        }
    }

    /// Process an incoming PeerExchange message.
    /// Validates peer IDs and addresses before adding to the pending PEX entries.
    async fn process_peer_exchange(&self, pex: PeerExchange, from_peer: &str) {
        if !self.enable_peer_exchange {
            return;
        }

        // Rate limit: max 1 PEX per peer per 30 seconds
        {
            let mut guard = self.pex_last_received.lock().await;
            let now = Instant::now();
            if let Some(last) = guard.get(from_peer) {
                if now.duration_since(*last).as_secs() < 30 {
                    tracing::debug!(peer = %from_peer, "PEX rate limit hit");
                    return;
                }
            }
            guard.insert(from_peer.to_string(), now);
        }

        let mut new_peers: Vec<(String, SocketAddr)> = Vec::new();

        {
            let mut guard = self.pex_entries.write().await;

            // Validate and add advertised peers (NOT the sender's address —
            // that comes from the authenticated transport layer, not the PEX payload)
            for (peer_id, addr) in pex.peers {
                if peer_id == self.our_peer_id {
                    continue;
                }
                if !validate_pex_peer_id(&peer_id) {
                    tracing::debug!(peer = %peer_id, "PEX rejected: invalid peer_id format");
                    continue;
                }
                if !validate_pex_address(addr, self.allow_private_ips) {
                    tracing::debug!(%addr, "PEX rejected: invalid address");
                    continue;
                }
                if !guard.contains_key(&peer_id) {
                    guard.insert(peer_id.clone(), (addr, Instant::now()));
                    new_peers.push((peer_id, addr));
                }
            }

            // Trim if over capacity
            while guard.len() > self.max_known_peers {
                if let Some(oldest) = guard.keys().next().cloned() {
                    guard.remove(&oldest);
                } else {
                    break;
                }
            }
        }

        let pex_count = self.pex_entries.read().await.len();
        tracing::info!(
            count = new_peers.len(),
            total = pex_count,
            "added peers from PEX"
        );

        // Auto-connect if enabled and below max_peers
        if self.auto_connect_discovered {
            let current_peers = self.peer_count();
            let max_peers = self.gossip.lock().await.limits().max_peers as usize;
            for (peer_id, addr) in new_peers {
                if current_peers >= max_peers {
                    break;
                }
                let addr_str = format!("{}@{}", peer_id, addr);
                tracing::debug!(addr = %addr_str, "auto-connecting to discovered peer");
                if let Err(e) = self.connect(&addr_str).await {
                    tracing::debug!(addr = %addr_str, "auto-connect failed: {e}");
                }
            }
        }
    }

    /// Merge bootstrap peers with non-expired PEX-discovered peers.
    fn merge_peers(
        &self,
        bootstrap: &std::collections::BTreeMap<String, SocketAddr>,
        pex: &std::collections::HashMap<String, (SocketAddr, Instant)>,
    ) -> Vec<(String, SocketAddr)> {
        let ttl = if self.pex_peer_ttl_seconds > 0 {
            Some(Duration::from_secs(self.pex_peer_ttl_seconds))
        } else {
            None
        };
        let now = Instant::now();

        let mut result: Vec<(String, SocketAddr)> =
            bootstrap.iter().map(|(k, v)| (k.clone(), *v)).collect();

        for (peer_id, (addr, added)) in pex {
            if let Some(ttl) = ttl {
                if now.duration_since(*added) > ttl {
                    continue; // expired
                }
            }
            if !bootstrap.contains_key(peer_id) {
                result.push((peer_id.clone(), *addr));
            }
        }
        result
    }

    /// Get a snapshot of the known peers address book.
    /// Includes bootstrap peers and non-expired PEX-discovered peers.
    pub async fn known_peers(&self) -> Vec<(String, SocketAddr)> {
        let bootstrap = self.known_peers.read().await;
        let pex = self.pex_entries.read().await;
        self.merge_peers(&bootstrap, &pex)
    }
}

#[async_trait::async_trait]
impl Network for CommonwareNetwork {
    async fn broadcast(&self, channel: u64, message: Vec<u8>) {
        let _ = self.try_broadcast(channel, message).await;
    }

    async fn try_broadcast(&self, channel: u64, message: Vec<u8>) -> Result<(), NetworkError> {
        let mut sender = self.sender.lock().await;
        let data = encode_with_channel(channel, &message);
        let buf = IoBuf::copy_from_slice(&data);
        sender
            .send(Recipients::All, buf, false)
            .await
            .map(|_| ())
            .map_err(|e| NetworkError::NetworkError(format!("broadcast failed: {e}")))
    }

    async fn send_to(&self, channel: u64, peers: Vec<String>, message: Vec<u8>) {
        if peers.is_empty() {
            return;
        }

        let mut sender = self.sender.lock().await;

        // Parse peer IDs into public keys
        let pub_keys: Vec<ed25519::PublicKey> = peers
            .iter()
            .filter_map(|peer_id| hex::decode(peer_id).ok())
            .filter_map(|bytes| ed25519::PublicKey::decode(&*bytes).ok())
            .collect();

        if pub_keys.is_empty() {
            return;
        }

        let recipients = if pub_keys.len() == 1 {
            Recipients::One(pub_keys.into_iter().next().expect("invariant: exactly one pubkey"))
        } else {
            Recipients::Some(pub_keys)
        };

        // IMPORTANT: prepend the channel byte (matches `broadcast`) so the
        // receiver can decode and dispatch on the correct channel. Without
        // this, SyncResponse / SyncRequest / OraclePriceSubmission payloads
        // were arriving with a garbage leading byte and being dropped.
        let data = encode_with_channel(channel, &message);
        let buf = IoBuf::copy_from_slice(&data);
        let _ = sender.send(recipients, buf, false).await;
    }

    async fn receive(&self) -> Result<(String, u64, Vec<u8>), NetworkError> {
        loop {
            let mut receiver = self.receiver.lock().await;
            let (public_key, io_buf) = receiver
                .recv()
                .await
                .map_err(|e| NetworkError::NetworkError(format!("receive failed: {e}")))?;

            let peer_id = hex::encode(public_key.as_ref());
            let data: &[u8] = io_buf.as_ref();

            let (channel, payload) = decode_with_channel(data)
                .ok_or_else(|| NetworkError::NetworkError("empty message received".into()))?;

            // Apply gossip rate limiting per peer only to transaction propagation
            // (channel 1). Exempt block announcements, sync, oracle, and upgrade
            // channels so consensus-critical traffic is never dropped.
            if channel == 1 {
                let mut gossip = self.gossip.lock().await;
                if let Some(peer_state) = gossip.peers.get_mut(&peer_id) {
                    if let Err(e) = peer_state.record_message() {
                        tracing::warn!(peer_id = %peer_id, "gossip rate limit hit: {e}");
                        continue;
                    }
                } else {
                    // Auto-register peer seen for the first time
                    let _ = gossip.add_peer(peer_id.clone());
                    if let Some(peer_state) = gossip.peers.get_mut(&peer_id) {
                        let _ = peer_state.record_message();
                    }
                }
            }

            // Transparently handle PeerExchange messages — loop again so callers never see them
            if let Ok(NetworkMessage::PeerExchange(pex)) = postcard::from_bytes(payload) {
                self.process_peer_exchange(pex, &peer_id).await;
                continue;
            }

            return Ok((peer_id, channel, payload.to_vec()));
        }
    }

    fn peer_count(&self) -> usize {
        // Read from the peers RwLock, which is updated by the peer set subscription
        if let Ok(guard) = self.peers.try_read() {
            guard.len()
        } else {
            0
        }
    }

    fn peer_ids(&self) -> Vec<String> {
        if let Ok(guard) = self.peers.try_read() {
            guard.keys().cloned().collect()
        } else {
            Vec::new()
        }
    }

    async fn connect(&self, address: &str) -> Result<(), NetworkError> {
        // Accept "peer_id@host:port" or just "host:port"
        let (peer_id_hex, addr) = if let Some((id, host)) = address.split_once('@') {
            let a: SocketAddr = host.parse().map_err(|e| {
                NetworkError::NetworkError(format!("invalid address '{host}': {e}"))
            })?;
            (id.to_string(), a)
        } else {
            // Address-only: try to find the peer in bootstrap_peers
            let addr: SocketAddr = address.parse().map_err(|e| {
                NetworkError::NetworkError(format!("invalid address '{address}': {e}"))
            })?;
            // Find matching bootstrap peer by address
            let peers = self.bootstrap_peers.read().await;
            let found = peers.iter().find(|(_, a)| *a == addr).cloned();
            drop(peers);
            if let Some((pid, _)) = found {
                (pid, addr)
            } else {
                return Err(NetworkError::NetworkError(format!(
                    "address '{address}' not in bootstrap peers — use 'peer_id@host:port' format"
                )));
            }
        };

        let pk_bytes = hex::decode(&peer_id_hex)
            .map_err(|e| NetworkError::NetworkError(format!("invalid peer_id: {e}")))?;
        let public_key =
            ed25519::PublicKey::decode(&*pk_bytes).map_err(|_| NetworkError::PeerNotFound {
                peer_id: peer_id_hex.clone(),
            })?;

        let peer_map: Map<ed25519::PublicKey, Address> =
            Map::from_iter_dedup(vec![(public_key, Address::Symmetric(addr))]);
        self.oracle.lock().await.track(0, peer_map).await;

        // Record in peers map for bookkeeping
        self.peers.write().await.insert(peer_id_hex.clone(), addr);

        // Also add to known_peers address book
        self.known_peers
            .write()
            .await
            .insert(peer_id_hex.clone(), addr);

        // Register peer with gossip manager for rate limiting
        let _ = self.gossip.lock().await.add_peer(peer_id_hex);

        Ok(())
    }

    async fn disconnect(&self, peer_id: &str) -> Result<(), NetworkError> {
        let mut oracle = self.oracle.lock().await;

        // Decode peer_id as public key and block
        let pk_bytes = hex::decode(peer_id)
            .map_err(|e| NetworkError::NetworkError(format!("invalid peer_id: {e}")))?;
        let public_key =
            ed25519::PublicKey::decode(&*pk_bytes).map_err(|_| NetworkError::PeerNotFound {
                peer_id: peer_id.to_string(),
            })?;

        oracle.block(public_key).await;
        self.peers.write().await.remove(peer_id);
        self.gossip.lock().await.remove_peer(peer_id);
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        let peer_count = self.peer_count();
        (peer_count as u32) >= self.min_healthy_peers
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_pex_peer_id_valid() {
        let valid = "aabbccdd".repeat(8); // 64 hex chars
        assert!(validate_pex_peer_id(&valid));
    }

    #[test]
    fn test_validate_pex_peer_id_too_short() {
        assert!(!validate_pex_peer_id("aabbccdd"));
    }

    #[test]
    fn test_validate_pex_peer_id_too_long() {
        assert!(!validate_pex_peer_id(&"aa".repeat(40))); // 80 chars
    }

    #[test]
    fn test_validate_pex_peer_id_invalid_hex() {
        assert!(!validate_pex_peer_id(&"gggg".repeat(16))); // 64 chars but invalid hex
    }

    #[test]
    fn test_validate_pex_address_public_ip() {
        let addr: SocketAddr = "8.8.8.8:1234".parse().unwrap();
        assert!(validate_pex_address(addr, false));
        assert!(validate_pex_address(addr, true));
    }

    #[test]
    fn test_validate_pex_address_loopback_rejected_in_production() {
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert!(!validate_pex_address(addr, false));
    }

    #[test]
    fn test_validate_pex_address_loopback_allowed_in_devnet() {
        let addr: SocketAddr = "127.0.0.1:1234".parse().unwrap();
        assert!(validate_pex_address(addr, true));
    }

    #[test]
    fn test_validate_pex_address_private_ip_rejected_in_production() {
        let addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();
        assert!(!validate_pex_address(addr, false));
    }

    #[test]
    fn test_validate_pex_address_private_ip_allowed_in_devnet() {
        let addr: SocketAddr = "192.168.1.1:1234".parse().unwrap();
        assert!(validate_pex_address(addr, true));
    }

    #[test]
    fn test_validate_pex_address_multicast_rejected() {
        let addr: SocketAddr = "224.0.0.1:1234".parse().unwrap();
        assert!(!validate_pex_address(addr, false));
        assert!(!validate_pex_address(addr, true));
    }

    #[test]
    fn test_validate_pex_address_unspecified_rejected() {
        let addr: SocketAddr = "0.0.0.0:1234".parse().unwrap();
        assert!(!validate_pex_address(addr, false));
        assert!(!validate_pex_address(addr, true));
    }

    #[test]
    fn test_validate_pex_address_link_local_rejected() {
        let addr: SocketAddr = "169.254.1.1:1234".parse().unwrap();
        assert!(!validate_pex_address(addr, false));
        assert!(!validate_pex_address(addr, true));
    }

    #[test]
    fn test_validate_pex_address_ipv6_link_local_rejected() {
        let addr: SocketAddr = "[fe80::1]:1234".parse().unwrap();
        assert!(!validate_pex_address(addr, false));
        assert!(!validate_pex_address(addr, true));
    }

    #[test]
    fn test_validate_pex_address_ipv6_public_allowed() {
        let addr: SocketAddr = "[2001:db8::1]:1234".parse().unwrap();
        assert!(validate_pex_address(addr, false));
        assert!(validate_pex_address(addr, true));
    }
}
