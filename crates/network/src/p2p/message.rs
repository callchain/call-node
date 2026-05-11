//! P2P Message Types (per spec §8.1, §9.1)

use alloy_rlp::{RlpDecodable, RlpEncodable};
use call_primitives::{BlockHash, PricePair, TxHash};
use serde::{Deserialize, Serialize};
use std::net::SocketAddr;

/// P2P network messages (RLP encoded per spec §9.1)
/// Note: enum uses serde for serialization; RLP derives apply to inner struct types
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum NetworkMessage {
    /// Transaction propagation (EvmTx)
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
    /// Signal sent by a validator after finalizing an epoch boundary block
    /// to coordinate epoch rotation across the subset.
    EpochBoundarySignal(EpochBoundarySignal),
    /// Internal request to restart the BFT engine (e.g. after sync crossed epoch)
    EngineRestartRequest(EngineRestartRequest),
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
        Self {
            data,
            hash,
            checksum,
        }
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

/// Signal sent by a validator after finalizing an epoch boundary block.
/// Broadcast to the subset so peers can track quorum readiness.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EpochBoundarySignal {
    /// The epoch boundary height that was finalized
    pub height: u64,
    /// Current epoch number
    pub epoch: u64,
    /// Sender's Ed25519 public key (32 bytes)
    pub sender_pubkey: [u8; 32],
}

/// Reason for requesting an engine restart.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum EngineRestartReason {
    /// Sync applied blocks that crossed an epoch boundary
    SyncCrossedEpoch,
    /// Validator set changed and requires re-evaluation
    ValidatorSetChange,
}

/// Internal request to restart the BFT engine.
/// Sent from the network/sync task to the BFT event loop.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EngineRestartRequest {
    /// Why the restart is needed
    pub reason: EngineRestartReason,
    /// Target epoch to restart into
    pub target_epoch: u64,
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

/// CRC32 helper used by TransactionMessage and wire protocol.
// ── Property-based tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_transaction_message_postcard_roundtrip(
            data in prop::collection::vec(any::<u8>(), 0..256),
            hash in prop::array::uniform32(any::<u8>()),
        ) {
            let msg = TransactionMessage::new(data, TxHash::from(hash));
            let encoded = postcard::to_allocvec(&msg).expect("serialize");
            let decoded: TransactionMessage = postcard::from_bytes(&encoded).expect("deserialize");
            prop_assert_eq!(msg.data, decoded.data);
            prop_assert_eq!(msg.hash, decoded.hash);
            prop_assert_eq!(msg.checksum, decoded.checksum);
        }

        #[test]
        fn prop_block_announcement_postcard_roundtrip(
            block_hash in prop::array::uniform32(any::<u8>()),
            height in any::<u64>(),
            proposer in any::<u32>(),
            timestamp_millis in any::<u64>(),
        ) {
            let msg = BlockAnnouncement {
                block_hash: BlockHash::from(block_hash),
                height,
                proposer,
                timestamp_millis,
            };
            let encoded = postcard::to_allocvec(&msg).expect("serialize");
            let decoded: BlockAnnouncement = postcard::from_bytes(&encoded).expect("deserialize");
            prop_assert_eq!(msg.block_hash, decoded.block_hash);
            prop_assert_eq!(msg.height, decoded.height);
            prop_assert_eq!(msg.proposer, decoded.proposer);
            prop_assert_eq!(msg.timestamp_millis, decoded.timestamp_millis);
        }

        #[test]
        fn prop_sync_request_postcard_roundtrip(
            start_height in any::<u64>(),
            count in any::<u64>(),
            full_state in any::<bool>(),
        ) {
            let msg = SyncRequest { start_height, count, full_state };
            let encoded = postcard::to_allocvec(&msg).expect("serialize");
            let decoded: SyncRequest = postcard::from_bytes(&encoded).expect("deserialize");
            prop_assert_eq!(msg.start_height, decoded.start_height);
            prop_assert_eq!(msg.count, decoded.count);
            prop_assert_eq!(msg.full_state, decoded.full_state);
        }

        #[test]
        fn prop_handshake_postcard_roundtrip(
            version in any::<u32>(),
            chain_id in any::<u64>(),
            best_height in any::<u64>(),
            best_hash in prop::array::uniform32(any::<u8>()),
            capabilities in any::<u32>(),
        ) {
            let msg = Handshake {
                version,
                chain_id,
                best_height,
                best_hash: BlockHash::from(best_hash),
                capabilities,
            };
            let encoded = postcard::to_allocvec(&msg).expect("serialize");
            let decoded: Handshake = postcard::from_bytes(&encoded).expect("deserialize");
            prop_assert_eq!(msg.version, decoded.version);
            prop_assert_eq!(msg.chain_id, decoded.chain_id);
            prop_assert_eq!(msg.best_height, decoded.best_height);
            prop_assert_eq!(msg.best_hash, decoded.best_hash);
            prop_assert_eq!(msg.capabilities, decoded.capabilities);
        }
    }
}

pub(crate) fn crc32_fast(data: &[u8]) -> u32 {
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
