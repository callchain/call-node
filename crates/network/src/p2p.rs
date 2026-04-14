//! T7.1 — P2P Network Layer (per spec §8)
//!
//! Message types, network traits, and state sync interfaces.
//! Actual commonware-p2p runtime wiring deferred to P14 (Node App)
//! due to Rust toolchain requirement (needs 1.90+ for Duration::from_hours).

use alloy_rlp::{RlpDecodable, RlpEncodable};
use call_primitives::{BlockHash, TxHash};
use serde::{Deserialize, Serialize};

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

// ── Network Trait (abstraction over commonware-p2p) ──────────────────

/// Abstract network interface.
///
/// This trait defines the contract for P2P communication.
/// The actual implementation uses commonware-p2p at runtime (P14).
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
}

// ── CRC32 Helper ─────────────────────────────────────────────────────

fn crc32_fast(data: &[u8]) -> u32 {
    // Simple CRC32 using the standard polynomial (0xEDB88320)
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

// ── Re-export NetworkError ───────────────────────────────────────────

pub use crate::limits::NetworkError;

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
}
