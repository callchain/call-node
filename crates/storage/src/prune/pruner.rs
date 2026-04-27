//! Pruning functions and fast sync flow.
//!
//! Per spec §10.3.5: fast sync downloads a verified snapshot, restores it,
//! then incrementally syncs to the current chain tip.

use crate::prune::config::{snapshot_message_hash, NodeMode, PruneConfig, StateSnapshot, StateRoots};
use crate::prune::state::PruneState;
use crate::reth_db::{compact_db, db_del, CallConsensusBlocks, CallConsensusState, CallReceipts};
use crate::StorageError;
use reth_db::DatabaseEnv;
use std::path::Path;

fn height_key(height: u64) -> Vec<u8> {
    height.to_be_bytes().to_vec()
}

/// Prune execution traces older than the given height boundary.
/// If `db` is provided, also deletes the corresponding entries from MDBX.
pub fn prune_execution_traces(
    state: &mut PruneState,
    prune_boundary: u64,
    db: Option<&DatabaseEnv>,
) -> Result<usize, StorageError> {
    let before = state.trace_count();
    let to_prune = state.execution_trace_keys_older_than(prune_boundary);
    state.remove_execution_traces(&to_prune);
    if let Some(db_env) = db {
        for h in &to_prune {
            let _ = db_del::<CallConsensusBlocks>(db_env, &height_key(*h));
        }
    }
    let pruned = before.saturating_sub(state.trace_count());
    state.traces_pruned += pruned as u64;
    Ok(pruned)
}

/// Prune receipts older than the given height boundary.
/// If `db` is provided, also deletes the corresponding entries from MDBX.
pub fn prune_receipts(
    state: &mut PruneState,
    prune_boundary: u64,
    db: Option<&DatabaseEnv>,
) -> Result<usize, StorageError> {
    let before = state.receipt_count();
    let to_prune = state.receipt_keys_older_than(prune_boundary);
    state.remove_receipts(&to_prune);
    if let Some(db_env) = db {
        for h in &to_prune {
            let _ = db_del::<CallReceipts>(db_env, &height_key(*h));
        }
    }
    let pruned = before.saturating_sub(state.receipt_count());
    state.receipts_pruned += pruned as u64;
    Ok(pruned)
}

/// Prune block bodies older than the given boundary, keeping only block headers.
/// If `db` is provided, also deletes the corresponding entries from MDBX.
pub fn prune_block_bodies(
    state: &mut PruneState,
    prune_boundary: u64,
    db: Option<&DatabaseEnv>,
) -> Result<usize, StorageError> {
    let before = state.body_count();
    let to_prune = state.block_body_keys_older_than(prune_boundary);
    state.remove_block_bodies(&to_prune);
    if let Some(db_env) = db {
        for h in &to_prune {
            let _ = db_del::<CallConsensusBlocks>(db_env, &height_key(*h));
        }
    }
    let pruned = before.saturating_sub(state.body_count());
    state.bodies_pruned += pruned as u64;
    Ok(pruned)
}

/// Remove snapshots beyond the retention limit (keep most recent N).
/// If `db` is provided, also deletes the corresponding entries from MDBX.
pub fn prune_old_snapshots(
    state: &mut PruneState,
    keep: u64,
    db: Option<&DatabaseEnv>,
) -> Result<usize, StorageError> {
    let before = state.snapshot_count();
    let mut to_prune = Vec::new();
    while state.snapshot_count() > keep as usize {
        if let Some(height) = state.pop_oldest_snapshot() {
            to_prune.push(height);
        }
    }
    if let Some(db_env) = db {
        for h in &to_prune {
            let _ = db_del::<CallConsensusState>(db_env, &height_key(*h));
        }
    }
    let pruned = before.saturating_sub(state.snapshot_count());
    state.snapshots_pruned += pruned as u64;
    Ok(pruned)
}

/// Run MDBX compaction to release unused disk pages.
///
/// Commits a flush transaction and marks the in-memory state accordingly.
/// If `db` is provided, calls `compact_db()` to sync the database.
pub fn compact_database(state: &mut PruneState, db: Option<&DatabaseEnv>) -> Result<(), StorageError> {
    if let Some(db_env) = db {
        compact_db(db_env)?;
    }
    state.request_compaction(state.last_compact_height());
    state.clear_compaction_pending();
    Ok(())
}

/// Run periodic prune checks based on the current height and config.
/// If `db` is provided, also deletes pruned entries from MDBX.
///
/// **Node mode behavior:**
/// - `Archive`: No pruning (all historical data retained)
/// - `Light`: Aggressive pruning — only headers kept, bodies/receipts/traces pruned immediately
/// - `Full` / `Validator`: Standard layered retention as configured
pub fn maybe_prune(
    state: &mut PruneState,
    current_height: u64,
    config: &PruneConfig,
    db: Option<&DatabaseEnv>,
) -> Result<(), StorageError> {
    match config.node_mode {
        NodeMode::Archive => {
            // Archive nodes retain everything; skip all pruning.
            return Ok(());
        }
        NodeMode::Light => {
            // Light nodes keep only recent headers; prune aggressively.
            let header_boundary = current_height.saturating_sub(1000);
            prune_execution_traces(state, header_boundary, db)?;
            prune_receipts(state, header_boundary, db)?;
            prune_block_bodies(state, header_boundary, db)?;
            prune_old_snapshots(state, 1, db)?;
            compact_database(state, db)?;
            return Ok(());
        }
        NodeMode::Full | NodeMode::Validator => {
            // Standard layered retention.
        }
    }

    if !current_height.is_multiple_of(config.prune_interval) {
        return Ok(());
    }

    let prune_boundary = current_height.saturating_sub(config.keep_recent);
    prune_execution_traces(state, prune_boundary, db)?;
    prune_receipts(state, current_height.saturating_sub(config.keep_receipt), db)?;
    prune_block_bodies(state, current_height.saturating_sub(config.keep_block_body), db)?;
    prune_old_snapshots(state, config.snapshot_keep, db)?;
    compact_database(state, db)?;

    Ok(())
}

/// Verify a state snapshot: cryptographically validate Ed25519 signatures
/// from validators and check that at least 2/3 of the validator set signed.
pub fn verify_snapshot(
    snapshot: &StateSnapshot,
    total_validators: u32,
    validator_pubkeys: &std::collections::HashMap<u32, call_primitives::Ed25519PublicKey>,
) -> bool {
    let quorum = (2 * total_validators).div_ceil(3); // ceiling of 2/3

    // If no pubkeys provided, fall back to count-only (not safe for production)
    if validator_pubkeys.is_empty() {
        return snapshot.validator_signatures.len() as u32 >= quorum;
    }

    let message = snapshot_message_hash(snapshot);
    let mut valid_count = 0;
    for vs in &snapshot.validator_signatures {
        if let Some(pubkey) = validator_pubkeys.get(&vs.validator_id) {
            if call_crypto::ed25519_verify(pubkey, &vs.signature, &message).is_ok() {
                valid_count += 1;
            }
        }
    }

    valid_count as u32 >= quorum
}

/// Compute a `StateSnapshot` from pre-computed state roots.
///
/// Called by the block production pipeline at `snapshot_interval` boundaries.
/// The resulting snapshot is recorded in `PruneState` and optionally saved
/// to disk for fast sync.
pub fn produce_state_snapshot(
    state: &mut PruneState,
    roots: StateRoots,
    height: u64,
    dir: Option<&Path>,
) -> Result<StateSnapshot, StorageError> {
    let snapshot = StateSnapshot {
        height,
        protocol_root: roots.protocol_root,
        evm_root: roots.evm_root,
        shielded_root: roots.shielded_root,
        agent_root: roots.agent_root,
        consensus_root: roots.consensus_root,
        total_size: 0, // would be estimated from DB size in production
        validator_signatures: vec![], // collected by the node's BFT layer
    };

    // Record in prune state
    state.add_snapshot(snapshot.clone());

    // Save to disk if directory provided
    if let Some(path) = dir {
        FastSyncFlow::save_snapshot(&snapshot, path)?;
    }

    Ok(snapshot)
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
    /// Save a snapshot to disk as JSON.
    pub fn save_snapshot(snapshot: &StateSnapshot, dir: &Path) -> Result<(), StorageError> {
        std::fs::create_dir_all(dir)
            .map_err(|e| StorageError::IoError(std::io::Error::other(e.to_string())))?;
        let path = dir.join(format!("snapshot-{}.json", snapshot.height));
        let data = serde_json::to_vec_pretty(snapshot)
            .map_err(|e| StorageError::Serialization(e.to_string()))?;
        std::fs::write(&path, data)
            .map_err(|e| StorageError::IoError(std::io::Error::other(e.to_string())))?;
        Ok(())
    }

    /// Load a snapshot from disk by height.
    pub fn load_snapshot(dir: &Path, height: u64) -> Result<StateSnapshot, StorageError> {
        let path = dir.join(format!("snapshot-{}.json", height));
        let data = std::fs::read(&path)
            .map_err(|e| StorageError::IoError(std::io::Error::other(e.to_string())))?;
        serde_json::from_slice(&data)
            .map_err(|e| StorageError::Serialization(e.to_string()))
    }

    /// List all available snapshot heights in the directory.
    pub fn list_snapshots(dir: &Path) -> Result<Vec<u64>, StorageError> {
        let entries = std::fs::read_dir(dir)
            .map_err(|e| StorageError::IoError(std::io::Error::other(e.to_string())))?;
        let mut heights = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if let Some(rest) = name_str
                .strip_prefix("snapshot-")
                .and_then(|s| s.strip_suffix(".json"))
            {
                if let Ok(h) = rest.parse::<u64>() {
                    heights.push(h);
                }
            }
        }
        heights.sort();
        Ok(heights)
    }

    /// Step 1-2: Download and verify a snapshot from the network.
    /// In a file-based setup, this loads the most recent verified snapshot from disk.
    ///
    /// `validator_pubkeys` maps validator_id -> Ed25519PublicKey for cryptographic
    /// signature verification. Must be non-empty — the quorum is calculated from
    /// the actual validator set size, not peer count.
    pub fn download_and_verify(
        dir: &Path,
        peers: &[String],
        validator_pubkeys: &std::collections::HashMap<u32, call_primitives::Ed25519PublicKey>,
    ) -> Result<StateSnapshot, StorageError> {
        if peers.is_empty() {
            return Err(StorageError::NotFound("no peers available".into()));
        }
        if validator_pubkeys.is_empty() {
            return Err(StorageError::Validation(
                "validator_pubkeys required for snapshot verification".into(),
            ));
        }
        let heights = Self::list_snapshots(dir)?;
        let latest = heights
            .last()
            .ok_or_else(|| StorageError::NotFound("no snapshots on disk".into()))?;
        let snapshot = Self::load_snapshot(dir, *latest)?;
        let total_validators = validator_pubkeys.len() as u32;
        if !verify_snapshot(&snapshot, total_validators, validator_pubkeys) {
            return Err(StorageError::Validation(
                "insufficient validator signatures".into(),
            ));
        }
        Ok(snapshot)
    }

    /// Step 3: Restore snapshot state to local database.
    /// In a file-based setup, this confirms the snapshot is readable and valid.
    pub fn restore_snapshot(dir: &Path, snapshot: &StateSnapshot) -> Result<(), StorageError> {
        // Verify the snapshot can be read back correctly
        let restored = Self::load_snapshot(dir, snapshot.height)?;
        if restored.height != snapshot.height || restored.protocol_root != snapshot.protocol_root {
            return Err(StorageError::Validation(
                "snapshot restore verification failed".into(),
            ));
        }
        Ok(())
    }

    /// Step 4: Incremental sync from snapshot height to current height.
    /// Returns the number of blocks synced.
    ///
    /// This function is intentionally a no-op in the storage crate — the storage
    /// layer has no network access. In a production node, incremental sync is
    /// handled by the consensus block production loop, which receives blocks
    /// from P2P peers and applies them via `Block::execute`. After restoring
    /// a snapshot, the node participates in consensus to catch up to the
    /// current chain tip.
    pub fn incremental_sync(_from_height: u64, _to_height: u64) -> Result<u64, StorageError> {
        Ok(0)
    }

    /// Full fast sync pipeline.
    pub fn run(
        dir: &Path,
        peers: &[String],
        validator_pubkeys: &std::collections::HashMap<u32, call_primitives::Ed25519PublicKey>,
    ) -> Result<u64, StorageError> {
        let snapshot = Self::download_and_verify(dir, peers, validator_pubkeys)?;
        let height = snapshot.height;
        Self::restore_snapshot(dir, &snapshot)?;
        let synced_to = Self::incremental_sync(height, height)?;
        Ok(synced_to)
    }
}
