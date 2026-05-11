//! Commonware network configuration and identity key management.

use crate::limits::NetworkLimits;
use commonware_codec::extensions::DecodeExt;
use commonware_cryptography::{ed25519, Signer};
use rand::RngCore;
use std::net::SocketAddr;

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
    /// TTL for PEX-discovered peers in seconds. 0 = no expiry.
    pub pex_peer_ttl_seconds: u64,
}

impl Default for CommonwareConfig {
    fn default() -> Self {
        Self {
            listen_addr: SocketAddr::from(([0, 0, 0, 0], 51235)),
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
            pex_peer_ttl_seconds: 600,
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
            pex_peer_ttl_seconds: 600,
        }
    }
}

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
    std::fs::write(&tmp_path, &hex_key).map_err(|e| format!("failed to write node.key: {e}"))?;
    std::fs::rename(&tmp_path, &key_path)
        .map_err(|e| format!("failed to persist node.key: {e}"))?;
    tracing::info!(peer_id = %hex::encode(key.public_key().as_ref()), path = ?key_path, "generated and persisted new identity key");
    Ok(key)
}
