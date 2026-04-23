//! T7.1 — P2P Network Layer (per spec §8)
//!
//! Message types, network traits, and state sync interfaces.

use alloy_rlp::{RlpDecodable, RlpEncodable};
use call_primitives::{BlockHash, PricePair, TxHash};
use commonware_codec::extensions::DecodeExt;
use commonware_p2p::{Address, AddressableManager, Blocker, PeerSetUpdate, Provider, Receiver, Recipients, Sender};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use commonware_cryptography::ed25519;
use commonware_cryptography::Signer;
use commonware_p2p::authenticated::lookup::{self as p2p_lookup, Config as P2PConfig};
use commonware_runtime::{IoBuf, Metrics, Quota, Runner, Spawner};
use commonware_utils::ordered::Map;
use rand::RngCore;

use crate::gossip::GossipManager;
use crate::limits::NetworkLimits;

// ── P2P Message Types (per spec §8.1, §9.1) ──────────────────────────

/// P2P network messages (RLP encoded per spec §9.1)
/// Note: enum uses serde for serialization; RLP derives apply to inner struct types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// Transaction propagation (ProtocolTransaction or EvmTx)
    Transaction(TransactionMessage),
    /// Block announcement
    BlockAnnouncement(BlockAnnouncement),
    /// Request for blocks/state sync
    SyncRequest(SyncRequest),
    /// Response to sync request
    SyncResponse(SyncResponse),
    /// Handshake for new peer connection
    Handshake(Handshake),
    /// Oracle price request broadcast by proposer
    OraclePriceRequest(OraclePriceRequest),
    /// Oracle price submission response from validator
    OraclePriceSubmission(OraclePriceSubmission),
    /// Protocol upgrade announcement
    UpgradeAnnouncement(UpgradeAnnouncement),
    /// Peer exchange — list of known peers for network discovery
    PeerExchange(PeerExchange),
}

/// Transaction message for gossipsub propagation
#[derive(Debug, Clone, Serialize, Deserialize, RlpEncodable, RlpDecodable)]
pub struct TransactionMessage {
    /// Raw RLP-encoded transaction data
    pub data: Vec<u8>,
    /// Transaction hash for quick identification
    pub hash: TxHash,
    /// Message checksum (CRC32 of data)
    pub checksum: u32,
}

impl TransactionMessage {
    pub fn new(data: Vec<u8>, hash: TxHash) -> Self {
        let checksum = crc32_fast(&data);
        Self { data, hash, checksum }
    }

    /// Verify data integrity
    pub fn verify_checksum(&self) -> bool {
        self.checksum == crc32_fast(&self.data)
    }
}

/// Block announcement for gossipsub
#[derive(Debug, Clone, Serialize, Deserialize, RlpEncodable, RlpDecodable)]
pub struct BlockAnnouncement {
    /// Block hash
    pub block_hash: BlockHash,
    /// Block height
    pub height: u64,
    /// Proposer validator ID
    pub proposer: u32,
    /// Timestamp milliseconds
    pub timestamp_millis: u64,
}

/// State sync request (request-response pattern per spec §8.1)
#[derive(Debug, Clone, Serialize, Deserialize, RlpEncodable, RlpDecodable)]
pub struct SyncRequest {
    /// Start block height (inclusive)
    pub start_height: u64,
    /// Number of blocks requested
    pub count: u64,
    /// Whether full state is requested (vs just headers)
    pub full_state: bool,
}

/// State sync response
#[derive(Debug, Clone, Serialize, Deserialize, RlpEncodable, RlpDecodable)]
pub struct SyncResponse {
    /// Start block height
    pub start_height: u64,
    /// RLP-encoded blocks
    pub blocks: Vec<Vec<u8>>,
    /// State root at end of this batch
    pub state_root: BlockHash,
}

/// Peer handshake message
#[derive(Debug, Clone, Serialize, Deserialize, RlpEncodable, RlpDecodable)]
pub struct Handshake {
    /// Protocol version
    pub version: u32,
    /// Chain ID
    pub chain_id: u64,
    /// Best block height
    pub best_height: u64,
    /// Best block hash
    pub best_hash: BlockHash,
    /// Peer network capabilities
    pub capabilities: u32,
}

/// Oracle price request — broadcast by the proposer at oracle period boundaries.
/// Validators respond with signed price submissions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OraclePriceRequest {
    /// Price pairs to fetch prices for
    pub pairs: Vec<PricePair>,
    /// Block height at which the oracle period is advancing
    pub block: u64,
    /// Validator ID of the requesting proposer
    pub requester_id: u32,
}

/// Upgrade announcement — broadcast by any node when a protocol upgrade is scheduled.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UpgradeAnnouncement {
    /// Protocol version to activate
    pub version: call_primitives::ProtocolVersion,
    /// Block height at which the upgrade activates
    pub activation_height: u64,
    /// Governance proposal ID that triggered this upgrade, if any
    pub proposal_id: Option<u64>,
}

/// Peer exchange message — exchanged between connected peers to discover new nodes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PeerExchange {
    /// Known peers advertised by the sender: (peer_id_hex, socket_address)
    pub peers: Vec<(String, SocketAddr)>,
    /// Sender's own listen address (helps with NAT traversal / address advertisement)
    pub sender_addr: SocketAddr,
}

impl PeerExchange {
    /// Create a new peer exchange with the given peer list and sender address.
    pub fn new(peers: Vec<(String, SocketAddr)>, sender_addr: SocketAddr) -> Self {
        Self { peers, sender_addr }
    }

    /// Limit the number of advertised peers to avoid oversized messages.
    pub fn truncate(&mut self, max: usize) {
        self.peers.truncate(max);
    }
}

/// Oracle price submission — signed price data from a validator
/// in response to an OraclePriceRequest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OraclePriceSubmission {
    /// Validator ID submitting the price
    pub validator_id: u32,
    /// Price pair for this price
    pub pair: PricePair,
    /// Price value
    pub price: u128,
    /// Block number
    pub block_number: u64,
    /// Timestamp in seconds
    pub timestamp: u64,
    /// Ed25519 signature of the submission
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
    /// Data sources (e.g., "binance", "coinbase")
    pub sources: Vec<String>,
}

// ── Network Trait (abstraction over commonware-p2p) ──────────────────

/// Abstract network interface.
///
/// This trait defines the contract for P2P communication.
/// At runtime, `CommonwareNetwork` provides a real commonware-p2p implementation.
#[async_trait::async_trait]
pub trait Network: Send + Sync + 'static {
    /// Send a message to all connected peers
    async fn broadcast(&self, channel: u64, message: Vec<u8>);

    /// Send a message to specific peers
    async fn send_to(&self, peers: Vec<String>, message: Vec<u8>);

    /// Receive messages from the network
    async fn receive(&self) -> Result<(String, u64, Vec<u8>), NetworkError>;

    /// Get the number of connected peers
    fn peer_count(&self) -> usize;

    /// Get the list of connected peer IDs
    fn peer_ids(&self) -> Vec<String>;

    /// Connect to a peer
    async fn connect(&self, address: &str) -> Result<(), NetworkError>;

    /// Disconnect from a peer
    async fn disconnect(&self, peer_id: &str) -> Result<(), NetworkError>;

    /// Check if the network is healthy
    fn is_healthy(&self) -> bool;
}

// ── Network Event ────────────────────────────────────────────────────

/// Events emitted by the P2P network layer
#[derive(Debug, Clone)]
pub enum NetworkEvent {
    /// New peer connected
    PeerConnected { peer_id: String },
    /// Peer disconnected
    PeerDisconnected { peer_id: String },
    /// Transaction received
    TransactionReceived {
        peer_id: String,
        hash: TxHash,
        data: Vec<u8>,
    },
    /// Block announcement received
    BlockAnnouncementReceived {
        peer_id: String,
        announcement: BlockAnnouncement,
    },
    /// Sync request received
    SyncRequestReceived {
        peer_id: String,
        request: SyncRequest,
    },
    /// Oracle price request received from proposer
    OraclePriceRequestReceived {
        peer_id: String,
        request: OraclePriceRequest,
    },
    /// Oracle price submission received
    OraclePriceSubmissionReceived {
        peer_id: String,
        submission: OraclePriceSubmission,
    },
    /// Protocol upgrade announcement received
    UpgradeAnnouncementReceived {
        peer_id: String,
        announcement: UpgradeAnnouncement,
    },
}

// ── CRC32 Helper ─────────────────────────────────────────────────────

fn crc32_fast(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xFFFF_FFFF;
    for &byte in data {
        crc ^= byte as u32;
        for _ in 0..8 {
            if crc & 1 != 0 {
                crc = (crc >> 1) ^ 0xEDB8_8320;
            } else {
                crc >>= 1;
            }
        }
    }
    !crc
}

// ── Network Error ────────────────────────────────────────────────────

pub use crate::limits::NetworkError;

// ── Wire Protocol ────────────────────────────────────────────────────

/// First byte of each message encodes the channel ID for multiplexing.
const CHANNEL_PREFIX_LEN: usize = 1;

fn encode_with_channel(channel: u64, payload: &[u8]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(CHANNEL_PREFIX_LEN + payload.len());
    buf.push(channel as u8);
    buf.extend_from_slice(payload);
    buf
}

fn decode_with_channel(data: &[u8]) -> Option<(u64, &[u8])> {
    if data.is_empty() {
        return None;
    }
    Some((data[0] as u64, &data[1..]))
}

// ── Commonware Network Config ────────────────────────────────────────

/// Configuration for the commonware-p2p network.
#[derive(Clone)]
pub struct CommonwareConfig {
    /// Listen address
    pub listen_addr: SocketAddr,
    /// Peer public keys to connect to (peer_id hex -> socket address)
    pub bootstrap_peers: Vec<(String, SocketAddr)>,
    /// Maximum message size in bytes
    pub max_message_size: u32,
    /// Whether to allow private IP connections (for devnet/testing)
    pub allow_private_ips: bool,
    /// Namespace for signing (prevents replay attacks across networks)
    pub namespace: Vec<u8>,
    /// Minimum number of peers required for the network to be considered healthy.
    /// Set to 0 for "any peer" health; validators should set this to 1+.
    pub min_healthy_peers: u32,
    /// Network-level rate limiting and gossip configuration
    pub limits: NetworkLimits,
    /// Enable peer exchange (PEX) for dynamic peer discovery.
    /// When enabled, nodes exchange lists of known peers with connected peers.
    pub enable_peer_exchange: bool,
    /// Interval between periodic PEX broadcasts, in seconds.
    pub pex_interval_seconds: u64,
    /// Automatically connect to peers discovered via PEX.
    /// Set to false in permissioned networks; true for permissionless discovery.
    pub auto_connect_discovered: bool,
    /// Maximum number of known peers to retain in the address book.
    pub max_known_peers: usize,
    /// Maximum number of peers to advertise in a single PEX message.
    pub max_pex_peers_per_msg: usize,
}

impl Default for CommonwareConfig {
    fn default() -> Self {
        Self {
            listen_addr: "0.0.0.0:51235".parse().unwrap(),
            bootstrap_peers: Vec::new(),
            max_message_size: 10 * 1024 * 1024, // 10 MB
            allow_private_ips: false,
            namespace: b"callchain".to_vec(),
            min_healthy_peers: 0,
            limits: NetworkLimits::default(),
            enable_peer_exchange: true,
            pex_interval_seconds: 60,
            auto_connect_discovered: false,
            max_known_peers: 1000,
            max_pex_peers_per_msg: 50,
        }
    }
}

impl CommonwareConfig {
    /// Create a config suitable for local testing (allows private IPs).
    pub fn local(listen_addr: SocketAddr) -> Self {
        Self {
            listen_addr,
            bootstrap_peers: Vec::new(),
            max_message_size: 10 * 1024 * 1024,
            allow_private_ips: true,
            namespace: b"callchain-local".to_vec(),
            min_healthy_peers: 0,
            limits: NetworkLimits::default(),
            enable_peer_exchange: true,
            pex_interval_seconds: 60,
            auto_connect_discovered: false,
            max_known_peers: 1000,
            max_pex_peers_per_msg: 50,
        }
    }
}

// ── Identity Key Management ──────────────────────────────────────────

/// Load or generate a persistent Ed25519 identity key.
///
/// Priority:
/// 1. Use the provided `key_hex` if Some (hex-encoded 32 or 64 byte key)
/// 2. Load from `{data_dir}/node.key` if it exists
/// 3. Generate a random key and persist to `{data_dir}/node.key`
pub fn load_or_generate_identity_key(
    data_dir: &std::path::Path,
    key_hex: Option<&str>,
) -> Result<ed25519::PrivateKey, String> {
    let key_path = data_dir.join("node.key");

    if let Some(hex_key) = key_hex {
        let hex_clean = hex_key.trim_start_matches("0x");
        let seed_hex = if hex_clean.len() == 128 {
            &hex_clean[..64]
        } else if hex_clean.len() == 64 {
            hex_clean
        } else {
            return Err(format!(
                "identity_key must be 64 or 128 hex chars, got {}",
                hex_clean.len()
            ));
        };
        let bytes = hex::decode(seed_hex).map_err(|e| format!("invalid identity_key hex: {e}"))?;
        let key = ed25519::PrivateKey::decode(&bytes[..])
            .map_err(|e| format!("invalid ed25519 key: {e}"))?;
        tracing::info!(peer_id = %hex::encode(key.public_key().as_ref()), "using configured identity key");
        return Ok(key);
    }

    if key_path.exists() {
        let content = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("failed to read node.key: {e}"))?;
        let hex_clean = content.trim();
        let bytes = hex::decode(hex_clean).map_err(|e| format!("invalid node.key: {e}"))?;
        let key = ed25519::PrivateKey::decode(&bytes[..])
            .map_err(|e| format!("invalid ed25519 key in node.key: {e}"))?;
        tracing::info!(peer_id = %hex::encode(key.public_key().as_ref()), path = ?key_path, "loaded persistent identity key");
        return Ok(key);
    }

    // Generate random key and persist
    let mut seed = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut seed);
    let key = ed25519::PrivateKey::decode(&seed[..])
        .map_err(|e| format!("failed to decode random ed25519 key: {e}"))?;
    let hex_key = hex::encode(seed);
    // Write atomically: write to tmp, then rename
    let tmp_path = key_path.with_extension("key.tmp");
    std::fs::write(&tmp_path, &hex_key)
        .map_err(|e| format!("failed to write node.key: {e}"))?;
    std::fs::rename(&tmp_path, &key_path)
        .map_err(|e| format!("failed to persist node.key: {e}"))?;
    tracing::info!(peer_id = %hex::encode(key.public_key().as_ref()), path = ?key_path, "generated and persisted new identity key");
    Ok(key)
}

// ── Commonware Network (real P2P via commonware-p2p) ─────────────────

/// Real P2P network adapter backed by commonware-p2p.
///
/// Wraps commonware-p2p's authenticated lookup network and implements
/// the `Network` trait. Messages are prefixed with a channel byte for
/// multiplexing over a single commonware channel.
///
/// # Usage
/// ```ignore
/// let config = CommonwareConfig::local("0.0.0.0:51235".parse().unwrap());
/// let network = CommonwareNetwork::new(&config, identity_key).await?;
///
/// // Use the network
/// network.broadcast(1, vec![1, 2, 3]).await;
/// let (peer_id, channel, data) = network.receive().await?;
/// ```
pub struct CommonwareNetwork {
    /// Sender for outgoing messages
    sender: tokio::sync::Mutex<p2p_lookup::Sender<ed25519::PublicKey, commonware_runtime::tokio::Context>>,
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
    pex_last_received: Arc<tokio::sync::Mutex<std::collections::HashMap<String, std::time::Instant>>>,
    /// PEX configuration fields
    enable_peer_exchange: bool,
    auto_connect_discovered: bool,
    max_known_peers: usize,
    max_pex_peers_per_msg: usize,
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
            config.bootstrap_peers.iter().cloned().collect::<std::collections::BTreeMap<_, _>>()
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
                    P2PConfig::local(signer, &cfg.namespace, cfg.listen_addr, cfg.max_message_size)
                } else {
                    P2PConfig::recommended(signer, &cfg.namespace, cfg.listen_addr, cfg.max_message_size)
                };

                // Create network
                let (mut network, mut oracle) = p2p_lookup::Network::new(
                    context.with_label("network"),
                    p2p_cfg,
                );

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
                let quota = Quota::per_second(NonZeroU32::new(1000).unwrap());
                let (sender, receiver) = network.register(0, quota, 10_000);

                // Start the network (spawns background tasks)
                let _handle = network.start();

                // Collect initial peer info
                let peers = Arc::new(tokio::sync::RwLock::new(
                    cfg.bootstrap_peers.iter().cloned().collect::<std::collections::BTreeMap<_, _>>(),
                ));

                // Subscribe to peer set changes — use actual addresses from the update
                let peers_clone = peers.clone();
                let mut subscription: tokio::sync::mpsc::UnboundedReceiver<PeerSetUpdate<ed25519::PublicKey>> = oracle.subscribe().await;
                let _subscribe_handle = context.clone().spawn(move |_ctx| async move {
                    while let Some(update) = subscription.recv().await {
                        let mut peers_guard = peers_clone.write().await;
                        peers_guard.clear();
                        let all = update.all.union();
                        for pk in all.into_iter() {
                            // Find the peer's address from the tracked bootstrap config
                            let pk_hex = hex::encode(pk.as_ref());
                            let addr = cfg.bootstrap_peers.iter()
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
                                            Map::from_iter_dedup(vec![(pk, Address::Symmetric(*addr))]);
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

    // ── Peer Exchange (PEX) ───────────────────────────────────────────

    /// Send our known peers to all connected peers.
    /// Call this periodically (e.g., every `pex_interval_seconds`) from the node loop.
    pub async fn send_peer_exchange(&self) {
        if !self.enable_peer_exchange {
            return;
        }

        let peers_to_advertise = {
            let guard = self.known_peers.read().await;
            let mut list: Vec<(String, SocketAddr)> = guard
                .iter()
                .filter(|(id, _)| **id != self.our_peer_id)
                .map(|(id, addr)| (id.clone(), *addr))
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
        match bincode::serialize(&msg) {
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
    /// Adds new peers to the known_peers address book and optionally auto-connects.
    async fn process_peer_exchange(&self, pex: PeerExchange, from_peer: &str) {
        if !self.enable_peer_exchange {
            return;
        }

        // Rate limit: max 1 PEX per peer per 30 seconds
        {
            let mut guard = self.pex_last_received.lock().await;
            let now = std::time::Instant::now();
            if let Some(last) = guard.get(from_peer) {
                if now.duration_since(*last).as_secs() < 30 {
                    tracing::debug!(peer = %from_peer, "PEX rate limit hit");
                    return;
                }
            }
            guard.insert(from_peer.to_string(), now);
        }

        // Also add the sender's advertised address (they know best)
        let mut new_peers: Vec<(String, SocketAddr)> = Vec::new();

        {
            let mut guard = self.known_peers.write().await;

            // Add sender's own address
            let sender_id = from_peer.to_string();
            if !guard.contains_key(&sender_id) && sender_id != self.our_peer_id {
                guard.insert(sender_id.clone(), pex.sender_addr);
                new_peers.push((sender_id, pex.sender_addr));
            }

            // Add advertised peers
            for (peer_id, addr) in pex.peers {
                if peer_id == self.our_peer_id {
                    continue;
                }
                if !guard.contains_key(&peer_id) {
                    guard.insert(peer_id.clone(), addr);
                    new_peers.push((peer_id, addr));
                }
            }

            // Trim if over capacity (oldest entries first — BTreeMap preserves order)
            while guard.len() > self.max_known_peers {
                if let Some(oldest) = guard.keys().next().cloned() {
                    guard.remove(&oldest);
                } else {
                    break;
                }
            }
        }

        let known_count = self.known_peers.read().await.len();
        tracing::info!(
            count = new_peers.len(),
            total = known_count,
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

    /// Get a snapshot of the known peers address book.
    pub async fn known_peers(&self) -> Vec<(String, SocketAddr)> {
        let guard = self.known_peers.read().await;
        guard.iter().map(|(k, v)| (k.clone(), *v)).collect()
    }
}

struct InitComponents {
    sender: p2p_lookup::Sender<ed25519::PublicKey, commonware_runtime::tokio::Context>,
    receiver: p2p_lookup::Receiver<ed25519::PublicKey>,
    oracle: p2p_lookup::Oracle<ed25519::PublicKey>,
    peers: Arc<tokio::sync::RwLock<std::collections::BTreeMap<String, SocketAddr>>>,
    our_peer_id: String,
    listen_addr: SocketAddr,
}

#[async_trait::async_trait]
impl Network for CommonwareNetwork {
    async fn broadcast(&self, channel: u64, message: Vec<u8>) {
        let mut sender = self.sender.lock().await;
        let data = encode_with_channel(channel, &message);
        let buf = IoBuf::copy_from_slice(&data);
        let _ = sender.send(Recipients::All, buf, false).await;
    }

    async fn send_to(&self, peers: Vec<String>, message: Vec<u8>) {
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
            Recipients::One(pub_keys.into_iter().next().unwrap())
        } else {
            Recipients::Some(pub_keys)
        };

        let buf = IoBuf::copy_from_slice(&message);
        let _ = sender.send(recipients, buf, false).await;
    }

    async fn receive(&self) -> Result<(String, u64, Vec<u8>), NetworkError> {
        loop {
            let mut receiver = self.receiver.lock().await;
            let (public_key, io_buf) = receiver.recv().await.map_err(|e| {
                NetworkError::NetworkError(format!("receive failed: {e}"))
            })?;

            let peer_id = hex::encode(public_key.as_ref());
            let data: &[u8] = io_buf.as_ref();

            let (channel, payload) = decode_with_channel(data).ok_or_else(|| {
                NetworkError::NetworkError("empty message received".into())
            })?;

            // Apply gossip rate limiting per peer
            // Auto-register unknown peers (discovered via oracle, not explicit connect)
            {
                let mut gossip = self.gossip.lock().await;
                if gossip.peers.contains_key(&peer_id) {
                    let peer_state = gossip.peers.get_mut(&peer_id).unwrap();
                    if let Err(e) = peer_state.record_message() {
                        tracing::warn!(peer_id = %peer_id, "gossip rate limit hit: {e}");
                        return Err(e);
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
            if let Ok(NetworkMessage::PeerExchange(pex)) = bincode::deserialize(payload) {
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
                return Err(NetworkError::NetworkError(
                    format!("address '{address}' not in bootstrap peers — use 'peer_id@host:port' format"),
                ));
            }
        };

        let pk_bytes = hex::decode(&peer_id_hex).map_err(|e| {
            NetworkError::NetworkError(format!("invalid peer_id: {e}"))
        })?;
        let public_key = ed25519::PublicKey::decode(&*pk_bytes).map_err(|_| {
            NetworkError::PeerNotFound { peer_id: peer_id_hex.clone() }
        })?;

        let peer_map: Map<ed25519::PublicKey, Address> =
            Map::from_iter_dedup(vec![(public_key, Address::Symmetric(addr))]);
        self.oracle.lock().await.track(0, peer_map).await;

        // Record in peers map for bookkeeping
        self.peers.write().await.insert(peer_id_hex.clone(), addr);

        // Also add to known_peers address book
        self.known_peers.write().await.insert(peer_id_hex.clone(), addr);

        // Register peer with gossip manager for rate limiting
        let _ = self.gossip.lock().await.add_peer(peer_id_hex);

        Ok(())
    }

    async fn disconnect(&self, peer_id: &str) -> Result<(), NetworkError> {
        let mut oracle = self.oracle.lock().await;

        // Decode peer_id as public key and block
        let pk_bytes = hex::decode(peer_id).map_err(|e| {
            NetworkError::NetworkError(format!("invalid peer_id: {e}"))
        })?;
        let public_key = ed25519::PublicKey::decode(&*pk_bytes).map_err(|_| {
            NetworkError::PeerNotFound { peer_id: peer_id.to_string() }
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

// ── In-Memory Network (for local testing) ────────────────────────────

/// In-memory network implementation for local testing.
///
/// Simulates a P2P network with in-memory message buffering.
/// Useful for testing without actual network connectivity.
pub struct InMemoryNetwork {
    connected_peers: std::sync::Mutex<Vec<String>>,
    message_buffer: std::sync::Mutex<Vec<(String, u64, Vec<u8>)>>,
}

impl InMemoryNetwork {
    pub fn new() -> Self {
        Self {
            connected_peers: std::sync::Mutex::new(Vec::new()),
            message_buffer: std::sync::Mutex::new(Vec::new()),
        }
    }

    /// Simulate receiving a message (pushes to buffer for testing)
    pub fn simulate_receive(&self, peer_id: String, channel: u64, message: Vec<u8>) {
        if let Ok(mut buffer) = self.message_buffer.lock() {
            buffer.push((peer_id, channel, message));
        }
    }
}

impl Default for InMemoryNetwork {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl Network for InMemoryNetwork {
    async fn broadcast(&self, channel: u64, message: Vec<u8>) {
        if let Ok(mut buffer) = self.message_buffer.lock() {
            buffer.push(("broadcast".into(), channel, message));
        }
    }

    async fn send_to(&self, peers: Vec<String>, message: Vec<u8>) {
        if let Ok(mut buffer) = self.message_buffer.lock() {
            for peer in &peers {
                buffer.push((peer.clone(), 0, message.clone()));
            }
        }
    }

    async fn receive(&self) -> Result<(String, u64, Vec<u8>), NetworkError> {
        let mut buffer = self.message_buffer.lock().map_err(|_| NetworkError::NetworkError("lock poisoned".into()))?;
        buffer.pop().ok_or_else(|| NetworkError::NetworkError("no messages".into()))
    }

    fn peer_count(&self) -> usize {
        self.connected_peers.lock().map(|p| p.len()).unwrap_or(0)
    }

    fn peer_ids(&self) -> Vec<String> {
        self.connected_peers.lock().map(|p| p.clone()).unwrap_or_default()
    }

    async fn connect(&self, address: &str) -> Result<(), NetworkError> {
        if let Ok(mut peers) = self.connected_peers.lock() {
            if !peers.contains(&address.to_string()) {
                peers.push(address.to_string());
            }
        }
        Ok(())
    }

    async fn disconnect(&self, peer_id: &str) -> Result<(), NetworkError> {
        if let Ok(mut peers) = self.connected_peers.lock() {
            peers.retain(|p| p != peer_id);
        }
        Ok(())
    }

    fn is_healthy(&self) -> bool {
        self.connected_peers.lock().map(|p| !p.is_empty()).unwrap_or(false)
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::limits::NetworkLimits;
    use call_primitives::TxHash;

    fn test_hash(n: u8) -> TxHash {
        TxHash::repeat_byte(n)
    }

    #[test]
    fn test_transaction_message_checksum() {
        let data = vec![1, 2, 3, 4, 5];
        let hash = test_hash(1);
        let msg = TransactionMessage::new(data.clone(), hash);

        assert!(msg.verify_checksum());
        assert_eq!(msg.hash, hash);
        assert_eq!(msg.data, data);
    }

    #[test]
    fn test_transaction_message_corrupted_data() {
        let data = vec![1, 2, 3, 4, 5];
        let hash = test_hash(1);
        let mut msg = TransactionMessage::new(data, hash);

        // Corrupt data
        msg.data[0] = 0xFF;
        assert!(!msg.verify_checksum());
    }

    #[test]
    fn test_block_announcement() {
        let ann = BlockAnnouncement {
            block_hash: test_hash(0xAB),
            height: 100,
            proposer: 42,
            timestamp_millis: 1_000_000,
        };

        assert_eq!(ann.height, 100);
        assert_eq!(ann.proposer, 42);
    }

    #[test]
    fn test_sync_request_response() {
        let req = SyncRequest {
            start_height: 10,
            count: 5,
            full_state: true,
        };

        assert_eq!(req.start_height, 10);
        assert_eq!(req.count, 5);
        assert!(req.full_state);
    }

    #[test]
    fn test_handshake() {
        let hs = Handshake {
            version: 1,
            chain_id: 1,
            best_height: 1000,
            best_hash: test_hash(0xFF),
            capabilities: 0b111, // tx + block + sync
        };

        assert_eq!(hs.version, 1);
        assert_eq!(hs.chain_id, 1);
    }

    #[test]
    fn test_oracle_price_request() {
        let req = OraclePriceRequest {
            pairs: vec![PricePair::new(1, 0), PricePair::new(2, 0), PricePair::new(3, 0)],
            block: 1000,
            requester_id: 5,
        };
        assert_eq!(req.pairs.len(), 3);
        assert_eq!(req.block, 1000);
        assert_eq!(req.requester_id, 5);

        // Test serialization
        let msg = NetworkMessage::OraclePriceRequest(req.clone());
        let serialized = bincode::serialize(&msg).unwrap();
        let deserialized: NetworkMessage = bincode::deserialize(&serialized).unwrap();
        assert!(matches!(deserialized, NetworkMessage::OraclePriceRequest(r) if r.block == 1000));
    }

    #[test]
    fn test_oracle_price_submission() {
        let sub = OraclePriceSubmission {
            validator_id: 2,
            pair: PricePair::new(1, 0),
            price: 2_000_000,
            block_number: 1000,
            timestamp: 1_000_000,
            signature: [0u8; 64],
            sources: vec!["binance".into()],
        };
        assert_eq!(sub.price, 2_000_000);
        assert_eq!(sub.sources.len(), 1);

        // Test serialization
        let msg = NetworkMessage::OraclePriceSubmission(sub.clone());
        let serialized = bincode::serialize(&msg).unwrap();
        let deserialized: NetworkMessage = bincode::deserialize(&serialized).unwrap();
        assert!(matches!(deserialized, NetworkMessage::OraclePriceSubmission(s) if s.price == 2_000_000));
    }

    #[test]
    fn test_network_event_oracle_variants() {
        let evt = NetworkEvent::OraclePriceRequestReceived {
            peer_id: "peer_1".into(),
            request: OraclePriceRequest {
                pairs: vec![PricePair::new(1, 0)],
                block: 1000,
                requester_id: 0,
            },
        };
        assert!(matches!(evt, NetworkEvent::OraclePriceRequestReceived { .. }));

        let evt2 = NetworkEvent::OraclePriceSubmissionReceived {
            peer_id: "peer_2".into(),
            submission: OraclePriceSubmission {
                validator_id: 1,
                pair: PricePair::new(1, 0),
                price: 100,
                block_number: 1000,
                timestamp: 1000,
                signature: [0u8; 64],
                sources: vec![],
            },
        };
        assert!(matches!(evt2, NetworkEvent::OraclePriceSubmissionReceived { .. }));
    }

    #[test]
    fn test_network_event_variants() {
        let evt1 = NetworkEvent::PeerConnected {
            peer_id: "peer_1".into(),
        };
        assert!(matches!(evt1, NetworkEvent::PeerConnected { .. }));

        let evt2 = NetworkEvent::TransactionReceived {
            peer_id: "peer_1".into(),
            hash: test_hash(1),
            data: vec![1, 2, 3],
        };
        assert!(matches!(evt2, NetworkEvent::TransactionReceived { .. }));

        let evt3 = NetworkEvent::BlockAnnouncementReceived {
            peer_id: "peer_1".into(),
            announcement: BlockAnnouncement {
                block_hash: test_hash(0xAB),
                height: 100,
                proposer: 1,
                timestamp_millis: 1000,
            },
        };
        assert!(matches!(
            evt3,
            NetworkEvent::BlockAnnouncementReceived { .. }
        ));
    }

    #[test]
    fn test_crc32_deterministic() {
        let data = b"hello world";
        let crc1 = crc32_fast(data);
        let crc2 = crc32_fast(data);
        assert_eq!(crc1, crc2);
    }

    #[test]
    fn test_crc32_different_data() {
        let crc1 = crc32_fast(b"hello");
        let crc2 = crc32_fast(b"world");
        assert_ne!(crc1, crc2);
    }

    #[test]
    fn test_network_limits_integration() {
        let limits = NetworkLimits::default();
        assert_eq!(limits.max_peers, 50);
        assert_eq!(limits.max_messages_per_second, 100);
    }

    #[tokio::test]
    async fn test_in_memory_network_connect_disconnect() {
        let network = InMemoryNetwork::new();
        assert_eq!(network.peer_count(), 0);
        assert!(!network.is_healthy());

        network.connect("peer_1").await.unwrap();
        assert_eq!(network.peer_count(), 1);
        assert!(network.is_healthy());

        network.disconnect("peer_1").await.unwrap();
        assert_eq!(network.peer_count(), 0);
        assert!(!network.is_healthy());
    }

    #[tokio::test]
    async fn test_in_memory_network_broadcast() {
        let network = InMemoryNetwork::new();
        network.connect("peer_1").await.unwrap();
        network.broadcast(1, vec![1, 2, 3]).await;

        let (peer_id, channel, data) = network.receive().await.unwrap();
        assert_eq!(peer_id, "broadcast");
        assert_eq!(channel, 1);
        assert_eq!(data, vec![1, 2, 3]);
    }

    #[tokio::test]
    async fn test_in_memory_network_receive_empty() {
        let network = InMemoryNetwork::new();
        let result = network.receive().await;
        assert!(result.is_err());
    }

    #[test]
    fn test_wire_protocol_channel_encoding() {
        let channel = 42u64;
        let payload = vec![1, 2, 3, 4, 5];
        let encoded = encode_with_channel(channel, &payload);

        assert_eq!(encoded[0], 42);
        assert_eq!(&encoded[1..], &payload);

        let (decoded_channel, decoded_payload) = decode_with_channel(&encoded).unwrap();
        assert_eq!(decoded_channel, channel);
        assert_eq!(decoded_payload, payload.as_slice());
    }

    #[test]
    fn test_wire_protocol_empty_message() {
        assert!(decode_with_channel(&[]).is_none());
    }

    #[test]
    fn test_commonware_config_defaults() {
        let cfg = CommonwareConfig::default();
        assert!(!cfg.allow_private_ips);
        assert_eq!(cfg.namespace, b"callchain");
    }

    #[test]
    fn test_commonware_config_local() {
        let addr = "127.0.0.1:51235".parse().unwrap();
        let cfg = CommonwareConfig::local(addr);
        assert!(cfg.allow_private_ips);
        assert_eq!(cfg.listen_addr, addr);
    }

    #[test]
    fn test_peer_exchange_serialization() {
        let pex = PeerExchange::new(
            vec![
                ("abcd".to_string(), "127.0.0.1:5001".parse().unwrap()),
                ("efgh".to_string(), "127.0.0.1:5002".parse().unwrap()),
            ],
            "127.0.0.1:5000".parse().unwrap(),
        );
        let msg = NetworkMessage::PeerExchange(pex);
        let serialized = bincode::serialize(&msg).unwrap();
        let deserialized: NetworkMessage = bincode::deserialize(&serialized).unwrap();
        match deserialized {
            NetworkMessage::PeerExchange(px) => {
                assert_eq!(px.peers.len(), 2);
                assert_eq!(px.sender_addr.to_string(), "127.0.0.1:5000");
            }
            _ => panic!("expected PeerExchange variant"),
        }
    }

    #[test]
    fn test_peer_exchange_truncate() {
        let mut pex = PeerExchange::new(
            (0..100)
                .map(|i| (format!("peer_{i}"), format!("127.0.0.1:{i}").parse().unwrap()))
                .collect(),
            "127.0.0.1:5000".parse().unwrap(),
        );
        pex.truncate(10);
        assert_eq!(pex.peers.len(), 10);
    }

    #[test]
    fn test_commonware_config_pex_defaults() {
        let cfg = CommonwareConfig::default();
        assert!(cfg.enable_peer_exchange);
        assert_eq!(cfg.pex_interval_seconds, 60);
        assert!(!cfg.auto_connect_discovered);
        assert_eq!(cfg.max_known_peers, 1000);
        assert_eq!(cfg.max_pex_peers_per_msg, 50);
    }
}
