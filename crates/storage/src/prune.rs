//! Prune configuration, node modes, state snapshots, and fast sync flow.
//!
//! Per spec §10.3: layered prune strategy with configurable retention periods.

use call_primitives::Hash;
use crate::StorageError;

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
#[derive(Debug, Clone)]
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

/// A validator's signature on a state snapshot.
#[derive(Debug, Clone)]
pub struct ValidatorSignature {
    pub validator_id: u32,
    pub signature: [u8; 65],
}

/// Prune execution traces older than the given height boundary.
pub fn prune_execution_traces(_prune_boundary: u64) -> Result<(), StorageError> {
    // TODO: implement when protocol layer is in place
    Ok(())
}

/// Prune receipts older than the given height boundary.
pub fn prune_receipts(_prune_boundary: u64) -> Result<(), StorageError> {
    // TODO: implement when receipt tables are wired up
    Ok(())
}

/// Prune block bodies (tx details) older than the given boundary,
/// keeping only block headers.
pub fn prune_block_bodies(_prune_boundary: u64) -> Result<(), StorageError> {
    // TODO: implement when block tables are wired up
    Ok(())
}

/// Remove snapshots beyond the retention limit (keep most recent N).
pub fn prune_old_snapshots(_keep: u64) -> Result<(), StorageError> {
    // TODO: implement when snapshot storage is wired up
    Ok(())
}

/// Compact the database to release physical disk space.
pub fn compact_database() -> Result<(), StorageError> {
    // TODO: implement when DB backend is wired up
    Ok(())
}

/// Run periodic prune checks based on the current height and config.
pub fn maybe_prune(current_height: u64, config: &PruneConfig) -> Result<(), StorageError> {
    if current_height % config.prune_interval != 0 {
        return Ok(());
    }

    let prune_boundary = current_height.saturating_sub(config.keep_recent);
    prune_execution_traces(prune_boundary)?;
    prune_receipts(current_height.saturating_sub(config.keep_receipt))?;
    prune_block_bodies(current_height.saturating_sub(config.keep_block_body))?;
    prune_old_snapshots(config.snapshot_keep)?;
    compact_database()?;

    Ok(())
}

/// Verify a state snapshot: check that at least 2/3 of validators signed.
pub fn verify_snapshot(snapshot: &StateSnapshot, total_validators: u32) -> bool {
    let quorum = (2 * total_validators + 2) / 3; // ceiling of 2/3
    snapshot.validator_signatures.len() as u32 >= quorum
}

/// Fast sync flow per spec §10.3.5:
/// 1. Download recent snapshot from network
/// 2. Verify 2/3 validator signatures + Merkle root consistency
/// 3. Restore snapshot state to local DB
/// 4. Incremental sync from snapshot height
/// 5. Participate in consensus
///
/// Target: < 5 minutes total sync time.
pub struct FastSyncFlow;

impl FastSyncFlow {
    /// Step 1-2: Download and verify a snapshot from the network.
    pub fn download_and_verify(_peers: &[String]) -> Result<StateSnapshot, StorageError> {
        // TODO: implement P2P snapshot download and verification
        Err(StorageError::NotFound("no snapshot available".into()))
    }

    /// Step 3: Restore snapshot state to local database.
    pub fn restore_snapshot(_snapshot: &StateSnapshot) -> Result<(), StorageError> {
        // TODO: implement state restoration
        Ok(())
    }

    /// Step 4: Incremental sync from snapshot height to current height.
    pub fn incremental_sync(_from_height: u64) -> Result<u64, StorageError> {
        // TODO: implement incremental block sync
        Ok(0)
    }

    /// Full fast sync pipeline.
    pub fn run(peers: &[String]) -> Result<u64, StorageError> {
        let snapshot = Self::download_and_verify(peers)?;
        let height = snapshot.height;
        Self::restore_snapshot(&snapshot)?;
        let synced_to = Self::incremental_sync(height)?;
        Ok(synced_to)
    }
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prune_config_defaults() {
        let config = PruneConfig::default();
        assert_eq!(config.snapshot_interval, 100_000);
        assert_eq!(config.snapshot_keep, 3);
        assert_eq!(config.prune_interval, 10_000);
        assert_eq!(config.keep_recent, 50_000);
        assert_eq!(config.keep_block_body, 100_000);
        assert_eq!(config.keep_receipt, 1_000_000);
        assert_eq!(config.node_mode, NodeMode::Full);
    }

    #[test]
    fn test_node_mode_variants() {
        // Verify all variants exist and are distinct
        assert_ne!(NodeMode::Validator, NodeMode::Full);
        assert_ne!(NodeMode::Full, NodeMode::Light);
        assert_ne!(NodeMode::Light, NodeMode::Archive);
    }

    #[test]
    fn test_snapshot_verification() {
        let total_validators = 216u32;
        let quorum = (2 * total_validators + 2) / 3; // = 144

        // Snapshot with enough signatures
        let snapshot_ok = StateSnapshot {
            height: 1000,
            protocol_root: Hash::ZERO,
            evm_root: Hash::ZERO,
            shielded_root: Hash::ZERO,
            agent_root: Hash::ZERO,
            consensus_root: Hash::ZERO,
            total_size: 0,
            validator_signatures: (0..quorum)
                .map(|i| ValidatorSignature {
                    validator_id: i,
                    signature: [0u8; 65],
                })
                .collect(),
        };
        assert!(verify_snapshot(&snapshot_ok, total_validators));

        // Snapshot with too few signatures
        let snapshot_bad = StateSnapshot {
            height: 1000,
            protocol_root: Hash::ZERO,
            evm_root: Hash::ZERO,
            shielded_root: Hash::ZERO,
            agent_root: Hash::ZERO,
            consensus_root: Hash::ZERO,
            total_size: 0,
            validator_signatures: (0..quorum - 1)
                .map(|i| ValidatorSignature {
                    validator_id: i,
                    signature: [0u8; 65],
                })
                .collect(),
        };
        assert!(!verify_snapshot(&snapshot_bad, total_validators));
    }

    #[test]
    fn test_maybe_prune_skips_when_not_interval() {
        let config = PruneConfig::default();
        // Height 5000 is not a multiple of prune_interval (10000)
        assert!(maybe_prune(5000, &config).is_ok());
    }

    #[test]
    fn test_maybe_prune_runs_at_interval() {
        let config = PruneConfig::default();
        // Height 10000 is a multiple of prune_interval
        assert!(maybe_prune(10_000, &config).is_ok());
    }

    #[test]
    fn test_fast_sync_returns_error_when_no_peers() {
        // Should return NotFound when no peers available
        let result = FastSyncFlow::download_and_verify(&[]);
        assert!(result.is_err());
    }
}
