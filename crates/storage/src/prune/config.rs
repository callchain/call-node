//! Prune configuration, node modes, and state snapshot types.
//!
//! Per spec §10.3: layered prune strategy with configurable retention periods.

use call_crypto::keccak256;
use call_primitives::Hash;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Prune configuration for layered data retention.
///
/// | Parameter          | Default      | Description                            |
/// |--------------------|-------------|----------------------------------------|
/// | snapshot_interval  | 100,000     | Blocks between state snapshots         |
/// | snapshot_keep      | 3           | Number of recent snapshots to keep     |
/// | prune_interval     | 10,000      | Blocks between prune checks            |
/// | keep_recent        | 50,000      | Recent blocks with full state          |
/// | keep_block_body    | 100,000     | Blocks with full transaction details   |
/// | keep_receipt       | 1,000,000   | Blocks with receipt/log data           |
#[derive(Debug, Clone)]
pub struct PruneConfig {
    /// Generate a full state snapshot every N blocks (~7 hours at 250ms/block)
    pub snapshot_interval: u64,
    /// Retain the most recent N snapshots
    pub snapshot_keep: u64,
    /// Run prune checks every N blocks
    pub prune_interval: u64,
    /// Retain full state for the most recent N blocks
    pub keep_recent: u64,
    /// Retain block body (tx details) for N blocks
    pub keep_block_body: u64,
    /// Retain receipts/logs for N blocks
    pub keep_receipt: u64,
    /// Node operating mode
    pub node_mode: NodeMode,
}

impl Default for PruneConfig {
    fn default() -> Self {
        Self {
            snapshot_interval: 100_000,
            snapshot_keep: 3,
            prune_interval: 10_000,
            keep_recent: 50_000,
            keep_block_body: 100_000,
            keep_receipt: 1_000_000,
            node_mode: NodeMode::Full,
        }
    }
}

/// Node operating mode, controlling data retention and consensus participation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NodeMode {
    /// Validator: full state + recent 100K blocks, participates in consensus
    Validator,
    /// Full node: current state + pruned history (default)
    #[default]
    Full,
    /// Light node: block headers only, state on-demand
    Light,
    /// Archive node: all historical data preserved
    Archive,
}

/// State snapshot for fast sync and checkpoint verification.
///
/// Contains the Merkle roots of all state sub-tries at a given block height,
/// signed by 2/3 of the validator set.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StateSnapshot {
    /// Block height at which the snapshot was taken
    pub height: u64,
    /// Protocol layer balance trie root
    pub protocol_root: Hash,
    /// EVM state trie root
    pub evm_root: Hash,
    /// Shielded pool Merkle root
    pub shielded_root: Hash,
    /// Agent state trie root
    pub agent_root: Hash,
    /// Validator set hash
    pub consensus_root: Hash,
    /// Estimated snapshot size in bytes
    pub total_size: u64,
    /// Validator signatures proving 2/3 consensus on this snapshot
    pub validator_signatures: Vec<ValidatorSignature>,
}

/// A validator's Ed25519 signature on a state snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidatorSignature {
    pub validator_id: u32,
    #[serde(with = "serde_bytes")]
    pub signature: [u8; 64],
}

mod serde_bytes {
    use serde::{Deserialize, Deserializer, Serializer};
    pub(super) fn serialize<S>(sig: &[u8; 64], serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_bytes(sig.as_slice())
    }
    pub(super) fn deserialize<'de, D>(deserializer: D) -> Result<[u8; 64], D::Error>
    where
        D: Deserializer<'de>,
    {
        let bytes = Vec::<u8>::deserialize(deserializer)?;
        let len = bytes.len();
        let arr: [u8; 64] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom(format!("expected 64 bytes, got {len}")))?;
        Ok(arr)
    }
}

/// Compute the canonical message hash that validators sign for a snapshot.
pub fn snapshot_message_hash(snapshot: &StateSnapshot) -> [u8; 32] {
    let mut buf = Vec::new();
    buf.extend_from_slice(&snapshot.height.to_be_bytes());
    buf.extend_from_slice(snapshot.protocol_root.as_slice());
    buf.extend_from_slice(snapshot.evm_root.as_slice());
    buf.extend_from_slice(snapshot.shielded_root.as_slice());
    buf.extend_from_slice(snapshot.agent_root.as_slice());
    buf.extend_from_slice(snapshot.consensus_root.as_slice());
    buf.extend_from_slice(&snapshot.total_size.to_be_bytes());
    call_crypto::keccak256(&buf).0
}

/// Pre-computed state roots from all sub-systems at a given block height.
///
/// Each root is computed by the respective subsystem and passed in here
/// to produce a canonical `StateSnapshot`.
#[derive(Debug, Clone)]
pub struct StateRoots {
    /// Protocol layer balance trie root
    pub protocol_root: Hash,
    /// EVM state trie root
    pub evm_root: Hash,
    /// Shielded pool Merkle root
    pub shielded_root: Hash,
    /// Agent state trie root
    pub agent_root: Hash,
    /// Validator set hash
    pub consensus_root: Hash,
}

/// Hash a protocol balance map into a single root.
pub fn compute_protocol_root(
    balances: &HashMap<
        (call_primitives::AssetId, call_primitives::Address),
        call_primitives::Balance,
    >,
    allowances: &HashMap<
        (
            call_primitives::AssetId,
            call_primitives::Address,
            call_primitives::Address,
        ),
        call_primitives::Balance,
    >,
) -> Hash {
    let mut buf = Vec::new();
    let mut entries: Vec<_> = balances.iter().collect();
    entries.sort_by_key(|(k, _)| *k);
    for ((asset_id, addr), balance) in entries {
        buf.extend_from_slice(&asset_id.to_be_bytes());
        buf.extend_from_slice(addr.as_slice());
        buf.extend_from_slice(&balance.to_be_bytes());
    }
    let mut entries: Vec<_> = allowances.iter().collect();
    entries.sort_by_key(|(k, _)| *k);
    for ((asset_id, owner, spender), amount) in entries {
        buf.extend_from_slice(&asset_id.to_be_bytes());
        buf.extend_from_slice(owner.as_slice());
        buf.extend_from_slice(spender.as_slice());
        buf.extend_from_slice(&amount.to_be_bytes());
    }
    keccak256(&buf)
}

/// Hash the agent registry into a single root.
pub fn compute_agent_root(agents: &HashMap<u64, (call_primitives::Address, String, u64)>) -> Hash {
    let mut buf = Vec::new();
    let mut entries: Vec<_> = agents.iter().collect();
    entries.sort_by_key(|(id, _)| *id);
    for (id, (owner, name, registered_at)) in entries {
        buf.extend_from_slice(&id.to_be_bytes());
        buf.extend_from_slice(owner.as_slice());
        buf.extend_from_slice(name.as_bytes());
        buf.extend_from_slice(&registered_at.to_be_bytes());
    }
    keccak256(&buf)
}

/// Hash the validator set into a single root.
pub fn compute_consensus_root(validators: &HashMap<u32, (call_primitives::Address, u128)>) -> Hash {
    let mut buf = Vec::new();
    let mut entries: Vec<_> = validators.iter().collect();
    entries.sort_by_key(|(id, _)| *id);
    for (id, (addr, stake)) in entries {
        buf.extend_from_slice(&id.to_be_bytes());
        buf.extend_from_slice(addr.as_slice());
        buf.extend_from_slice(&stake.to_be_bytes());
    }
    keccak256(&buf)
}
