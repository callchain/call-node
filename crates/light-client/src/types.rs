//! Core types for the Ethereum light client.

use alloy_primitives::{Address, B256};
use call_primitives::AssetId;
use serde::{Deserialize, Serialize};

/// Trusted genesis state for initializing the light client.
/// The light client starts from a known-good header (anchor point).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GenesisState {
    /// Block hash of the trusted anchor
    pub anchor_hash: B256,
    /// Block number of the trusted anchor
    pub anchor_block: u64,
    /// State root at the anchor (for consistency checks)
    pub state_root: B256,
}

/// Beacon chain configuration for consensus signature verification.
///
/// These parameters are chain-level constants needed to compute the BLS
/// signing domain for sync committee aggregate signatures. They are
/// provided at light client initialization (bootstrap) and remain fixed
/// unless a hard fork changes the `fork_version`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BeaconConfig {
    /// Current fork version (e.g. `[0, 0, 0, 1]` for Altair).
    /// Changes at scheduled hard forks.
    pub fork_version: [u8; 4],
    /// Genesis validators root (32 bytes), fixed at chain genesis.
    pub genesis_validators_root: B256,
}

/// RLP-encoded Ethereum block header with computed hash.
/// We use the full RLP bytes for verification and decode fields as needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EthHeader {
    /// RLP-encoded header bytes (for hash computation)
    pub rlp_bytes: Vec<u8>,
    /// Precomputed block hash = keccak256(rlp_bytes)
    pub block_hash: B256,
}

impl EthHeader {
    /// Create a header from RLP bytes and precompute its hash.
    pub fn from_rlp(rlp_bytes: Vec<u8>) -> Self {
        use alloy_primitives::keccak256;
        let block_hash = keccak256(&rlp_bytes);
        Self {
            rlp_bytes,
            block_hash,
        }
    }

    /// Get the parent hash from the RLP-encoded header.
    /// The parent hash is the first field in the Ethereum header.
    pub fn parent_hash(&self) -> Option<B256> {
        let item = decode_rlp_field(&self.rlp_bytes, 0)?;
        if item.len() == 32 {
            Some(B256::from_slice(item))
        } else {
            None
        }
    }

    /// Get the block number from the header.
    pub fn number(&self) -> Option<u64> {
        let item = decode_rlp_field(&self.rlp_bytes, 8)?; // block_number is field index 8
                                                          // Decode big-endian integer from bytes
        let mut bytes = [0u8; 8];
        let start = bytes.len().saturating_sub(item.len());
        bytes[start..].copy_from_slice(item);
        Some(u64::from_be_bytes(bytes))
    }

    /// Get the transactions root.
    pub fn transactions_root(&self) -> Option<B256> {
        let item = decode_rlp_field(&self.rlp_bytes, 4)?;
        if item.len() == 32 {
            Some(B256::from_slice(item))
        } else {
            None
        }
    }

    /// Get the receipts root.
    pub fn receipts_root(&self) -> Option<B256> {
        let item = decode_rlp_field(&self.rlp_bytes, 5)?;
        if item.len() == 32 {
            Some(B256::from_slice(item))
        } else {
            None
        }
    }

    /// Get the state root.
    pub fn state_root(&self) -> Option<B256> {
        let item = decode_rlp_field(&self.rlp_bytes, 3)?;
        if item.len() == 32 {
            Some(B256::from_slice(item))
        } else {
            None
        }
    }
}

/// Decode a single RLP item from the start of data.
/// Returns (item_payload, total_consumed).
fn decode_rlp_item(data: &[u8]) -> Option<(&[u8], usize)> {
    if data.is_empty() {
        return None;
    }
    let first = data[0];
    if first < 0x80 {
        Some((&data[..1], 1))
    } else if first < 0xB8 {
        let len = (first - 0x80) as usize;
        if 1 + len > data.len() {
            return None;
        }
        Some((&data[1..1 + len], 1 + len))
    } else if first < 0xC0 {
        let len_of_len = (first - 0xB7) as usize;
        if 1 + len_of_len > data.len() {
            return None;
        }
        let len = usize::from_be_bytes({
            let mut buf = [0u8; 8];
            buf[8 - len_of_len..].copy_from_slice(&data[1..1 + len_of_len]);
            buf
        });
        let total = 1 + len_of_len + len;
        if total > data.len() {
            return None;
        }
        Some((&data[1 + len_of_len..total], total))
    } else if first < 0xF8 {
        let list_len = (first - 0xC0) as usize;
        if 1 + list_len > data.len() {
            return None;
        }
        Some((&data[..1 + list_len], 1 + list_len))
    } else {
        // Long list
        let len_of_len = (first - 0xF7) as usize;
        if 1 + len_of_len > data.len() {
            return None;
        }
        let total_payload = usize::from_be_bytes({
            let mut buf = [0u8; 8];
            buf[8 - len_of_len..].copy_from_slice(&data[1..1 + len_of_len]);
            buf
        });
        let total = 1 + len_of_len + total_payload;
        if total > data.len() {
            return None;
        }
        Some((&data[..total], total))
    }
}

/// Decode field at index `idx` from an RLP-encoded Ethereum header.
/// Returns the raw payload bytes of the field.
fn decode_rlp_field(data: &[u8], idx: usize) -> Option<&[u8]> {
    if data.is_empty() {
        return None;
    }
    let first = data[0];
    // Calculate where the list payload starts
    let payload_start = if first < 0xF8 {
        1 // short list
    } else {
        let len_of_len = (first - 0xF7) as usize;
        1 + len_of_len // long list
    };
    if payload_start >= data.len() {
        return None;
    }
    let mut cursor = &data[payload_start..];
    for i in 0..=idx {
        let (item, consumed) = decode_rlp_item(cursor)?;
        if i == idx {
            return Some(item);
        }
        cursor = &cursor[consumed..];
    }
    None
}

/// MPT proof node for transaction or receipt inclusion.
/// A sequence of these nodes (RLP-encoded) proves that a tx/receipt
/// is in the block's transactions/receipts trie.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MptProofNode {
    /// RLP-encoded node bytes
    pub rlp_bytes: Vec<u8>,
}

impl MptProofNode {
    pub fn new(rlp_bytes: Vec<u8>) -> Self {
        Self { rlp_bytes }
    }
}

/// Transaction inclusion proof in a block.
/// Proves that a specific transaction is included in the block's
/// transactions Merkle-Patricia Trie.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TxInclusionProof {
    /// Proof nodes from root to leaf in the transactions trie
    pub nodes: Vec<MptProofNode>,
}

impl TxInclusionProof {
    pub fn new(nodes: Vec<MptProofNode>) -> Self {
        Self { nodes }
    }

    pub fn node_rlps(&self) -> Vec<Vec<u8>> {
        self.nodes.iter().map(|n| n.rlp_bytes.clone()).collect()
    }
}

/// Receipt proof in a block.
/// Proves that a specific receipt (with logs) is in the block's
/// receipts Merkle-Patricia Trie.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptProof {
    /// Receipt index within the block (used as the trie key, RLP-encoded)
    pub receipt_index: u64,
    /// Proof nodes from root to leaf in the receipts trie
    pub nodes: Vec<MptProofNode>,
}

impl ReceiptProof {
    pub fn new(receipt_index: u64, nodes: Vec<MptProofNode>) -> Self {
        Self {
            receipt_index,
            nodes,
        }
    }

    pub fn node_rlps(&self) -> Vec<Vec<u8>> {
        self.nodes.iter().map(|n| n.rlp_bytes.clone()).collect()
    }
}

/// Bridge deposit event parsed from an Ethereum receipt log.
/// This is the event emitted by the Call bridge contract on Ethereum.
#[derive(Debug, Clone)]
pub struct BridgeEvent {
    /// The source chain identifier
    pub source_chain: u64,
    /// Hash of the source transaction
    pub source_tx_hash: B256,
    /// Block number on source chain
    pub source_block: u64,
    /// Sender address on source chain
    pub sender: Vec<u8>,
    /// Recipient on Call chain
    pub recipient: Address,
    /// Asset ID being bridged
    pub asset_id: AssetId,
    /// Amount being bridged
    pub amount: u128,
}

/// Parsed Ethereum receipt log.
#[derive(Debug, Clone)]
pub struct ReceiptLog {
    /// Contract address that emitted the log
    pub address: Vec<u8>,
    /// Indexed topics (first = event signature)
    pub topics: Vec<B256>,
    /// Unindexed data
    pub data: Vec<u8>,
}

/// Light client verification error.
#[derive(Debug, thiserror::Error)]
pub enum LightClientError {
    #[error("invalid header: {0}")]
    InvalidHeader(String),
    #[error("parent hash mismatch at block {block}: expected {expected}, got {actual}")]
    ParentHashMismatch {
        block: u64,
        expected: B256,
        actual: B256,
    },
    #[error("header not found at block {0}")]
    HeaderNotFound(u64),
    #[error("header at block {block} not yet verified (latest: {latest})")]
    HeaderNotVerified { block: u64, latest: u64 },
    #[error("transaction not found in block")]
    TxNotFound,
    #[error("receipt not found in block")]
    ReceiptNotFound,
    #[error("bridge event not found in receipt logs")]
    BridgeEventNotFound,
    #[error("receipt log parsing failed: {0}")]
    LogParseError(String),
    #[error("MPT proof verification failed: {0}")]
    MptProofError(String),
    #[error("genesis not initialized")]
    NotInitialized,
    #[error("block number {0} is before anchor")]
    BeforeAnchor(u64),
    #[error("duplicate header at block {0}")]
    DuplicateHeader(u64),
    #[error("cannot reorg below finalized block {0}")]
    BeforeFinalized(u64),
    #[error("gap buffer is full")]
    BufferFull,
    #[error("block {0} not yet verified, cannot advance anchor")]
    AnchorNotVerified(u64),
    #[error("sync committee signature invalid: {0}")]
    SyncCommitteeSignatureInvalid(String),
    #[error("sync committee update failed: {0}")]
    SyncCommitteeUpdateFailed(String),
    #[error("sync committee already initialized")]
    AlreadyInitialized,
    #[error("insufficient sync committee participation: {got}/{required}")]
    InsufficientSyncParticipation { got: usize, required: usize },
}
