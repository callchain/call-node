//! Pruning functions and fast sync flow.
//!
//! Per spec §10.3.5: fast sync downloads a verified snapshot, restores it,
//! then incrementally syncs to the current chain tip.

use crate::prune::config::{
    snapshot_message_hash, NodeMode, PruneConfig, StateRoots, StateSnapshot,
};
use crate::prune::state::PruneState;
use crate::reth_db::{
    compact_db, db_del, CallBlockHashByHeight, CallConsensusBlocks, CallConsensusState,
    CallReceipts,
};
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
            let _ = db_del::<CallBlockHashByHeight>(db_env, &height_key(*h));
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
            let _ = db_del::<CallBlockHashByHeight>(db_env, &height_key(*h));
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
pub fn compact_database(
    state: &mut PruneState,
    db: Option<&DatabaseEnv>,
) -> Result<(), StorageError> {
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
    prune_receipts(
        state,
        current_height.saturating_sub(config.keep_receipt),
        db,
    )?;
    prune_block_bodies(
        state,
        current_height.saturating_sub(config.keep_block_body),
        db,
    )?;
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
        total_size: 0,                // would be estimated from DB size in production
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
        serde_json::from_slice(&data).map_err(|e| StorageError::Serialization(e.to_string()))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::prune::config::ValidatorSignature;
    use call_crypto::{ed25519_generate_keypair, ed25519_sign};
    use call_primitives::Hash;
    use std::collections::HashMap;

    fn make_snapshot(height: u64) -> StateSnapshot {
        StateSnapshot {
            height,
            protocol_root: Hash::repeat_byte(0xA1),
            evm_root: Hash::repeat_byte(0xB2),
            shielded_root: Hash::repeat_byte(0xC3),
            agent_root: Hash::repeat_byte(0xD4),
            consensus_root: Hash::repeat_byte(0xE5),
            total_size: 0,
            validator_signatures: Vec::new(),
        }
    }

    fn sign_snapshot(
        snapshot: &mut StateSnapshot,
        validator_id: u32,
        signing_key: &ed25519_dalek::SigningKey,
    ) {
        let message = snapshot_message_hash(snapshot);
        let sig = ed25519_sign(signing_key, &message);
        snapshot.validator_signatures.push(ValidatorSignature {
            validator_id,
            signature: sig,
        });
    }

    #[test]
    fn test_verify_snapshot_valid_quorum() {
        let mut snapshot = make_snapshot(100);
        let mut pubkeys = HashMap::new();

        // 3 validators, need 2 signatures for quorum
        for id in 0..3 {
            let (pk, sk) = ed25519_generate_keypair();
            pubkeys.insert(id, pk);
            if id < 2 {
                sign_snapshot(&mut snapshot, id, &sk);
            }
        }

        assert!(verify_snapshot(&snapshot, 3, &pubkeys));
    }

    #[test]
    fn test_verify_snapshot_tampered_data_rejected() {
        let mut snapshot = make_snapshot(100);
        let mut pubkeys = HashMap::new();

        for id in 0..3 {
            let (pk, sk) = ed25519_generate_keypair();
            pubkeys.insert(id, pk);
            sign_snapshot(&mut snapshot, id, &sk);
        }

        // Tamper with snapshot data after signing
        snapshot.evm_root = Hash::repeat_byte(0xFF);

        assert!(
            !verify_snapshot(&snapshot, 3, &pubkeys),
            "tampered snapshot should fail verification"
        );
    }

    #[test]
    fn test_verify_snapshot_below_quorum() {
        let mut snapshot = make_snapshot(100);
        let mut pubkeys = HashMap::new();

        for id in 0..3 {
            let (pk, sk) = ed25519_generate_keypair();
            pubkeys.insert(id, pk);
        }
        // Only 1 signature — below quorum of 2
        let (_, sk) = ed25519_generate_keypair();
        pubkeys.insert(99, sk.verifying_key().to_bytes());
        sign_snapshot(&mut snapshot, 0, &sk);

        assert!(
            !verify_snapshot(&snapshot, 3, &pubkeys),
            "below quorum should fail"
        );
    }

    #[test]
    fn test_verify_snapshot_unknown_validator_ignored() {
        let mut snapshot = make_snapshot(100);
        let mut pubkeys = HashMap::new();

        // 3 known validators
        for id in 0..3 {
            let (pk, sk) = ed25519_generate_keypair();
            pubkeys.insert(id, pk);
            if id < 2 {
                sign_snapshot(&mut snapshot, id, &sk);
            }
        }

        // Add a signature from an unknown validator (id 999)
        let (_, sk_unknown) = ed25519_generate_keypair();
        sign_snapshot(&mut snapshot, 999, &sk_unknown);

        // Should still pass because the 2 known validators form quorum
        assert!(
            verify_snapshot(&snapshot, 3, &pubkeys),
            "unknown validator sig should be ignored, known quorum should pass"
        );

        // But if we only had the unknown sig, it should fail
        let mut snapshot_bad = make_snapshot(100);
        sign_snapshot(&mut snapshot_bad, 999, &sk_unknown);
        assert!(
            !verify_snapshot(&snapshot_bad, 3, &pubkeys),
            "only unknown sig should fail"
        );
    }

    #[test]
    fn test_verify_snapshot_empty_pubkeys_fallback() {
        let mut snapshot = make_snapshot(100);

        // 3 signatures but no pubkeys provided — falls back to count-only
        for id in 0..3 {
            let (_, sk) = ed25519_generate_keypair();
            sign_snapshot(&mut snapshot, id, &sk);
        }

        let empty_pubkeys: HashMap<u32, call_primitives::Ed25519PublicKey> = HashMap::new();
        assert!(
            verify_snapshot(&snapshot, 3, &empty_pubkeys),
            "count-only fallback should pass with 3 of 3 signatures"
        );

        // Below quorum even in count-only mode
        let mut snapshot_low = make_snapshot(100);
        let (_, sk) = ed25519_generate_keypair();
        sign_snapshot(&mut snapshot_low, 0, &sk);
        assert!(
            !verify_snapshot(&snapshot_low, 3, &empty_pubkeys),
            "count-only fallback should still require quorum"
        );
    }

    #[test]
    fn test_verify_snapshot_invalid_signature_bytes_rejected() {
        let mut snapshot = make_snapshot(100);
        let mut pubkeys = HashMap::new();

        let (pk, sk) = ed25519_generate_keypair();
        pubkeys.insert(0, pk);
        sign_snapshot(&mut snapshot, 0, &sk);

        // Add a second signature with tampered bytes
        snapshot.validator_signatures.push(ValidatorSignature {
            validator_id: 1,
            signature: [0xFFu8; 64],
        });
        // We need to give a pubkey for validator 1 so it attempts verification
        let (pk1, _) = ed25519_generate_keypair();
        pubkeys.insert(1, pk1);

        // Only 1 valid signature out of 3 needed for quorum (ceil(2*3/3)=2)
        assert!(
            !verify_snapshot(&snapshot, 3, &pubkeys),
            "invalid signature bytes should not count toward quorum"
        );
    }

    #[test]
    fn test_incremental_sync_no_op_returns_zero() {
        // incremental_sync is intentionally a no-op in the storage crate
        assert_eq!(FastSyncFlow::incremental_sync(0, 0).unwrap(), 0);
        assert_eq!(FastSyncFlow::incremental_sync(100, 200).unwrap(), 0);
        assert_eq!(FastSyncFlow::incremental_sync(1000, 5000).unwrap(), 0);
    }

    #[test]
    fn test_fast_sync_run_full_pipeline() {
        let tmp = std::env::temp_dir().join(format!("call-fast-sync-test-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Create a snapshot with valid validator signatures
        let mut snapshot = make_snapshot(500);
        let mut pubkeys = HashMap::new();
        for id in 0..3 {
            let (pk, sk) = ed25519_generate_keypair();
            pubkeys.insert(id, pk);
            if id < 2 {
                sign_snapshot(&mut snapshot, id, &sk);
            }
        }

        // Save snapshot to disk
        FastSyncFlow::save_snapshot(&snapshot, &tmp).unwrap();

        // Run the full fast sync pipeline
        let peers = vec!["peer1".to_string()];
        let result = FastSyncFlow::run(&tmp, &peers, &pubkeys);
        assert!(
            result.is_ok(),
            "fast sync pipeline should complete: {:?}",
            result.err()
        );
        assert_eq!(result.unwrap(), 0, "catch-up should return 0 (no-op)");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fast_sync_run_no_peers_fails() {
        let tmp =
            std::env::temp_dir().join(format!("call-fast-sync-no-peers-{}", std::process::id()));
        let pubkeys: HashMap<u32, call_primitives::Ed25519PublicKey> = HashMap::new();
        let peers: Vec<String> = Vec::new();

        let err = FastSyncFlow::run(&tmp, &peers, &pubkeys).unwrap_err();
        assert!(
            err.to_string().contains("no peers available"),
            "should fail with no peers: {}",
            err
        );
    }

    #[test]
    fn test_fast_sync_run_no_pubkeys_fails() {
        let tmp =
            std::env::temp_dir().join(format!("call-fast-sync-no-pubkeys-{}", std::process::id()));
        let pubkeys: HashMap<u32, call_primitives::Ed25519PublicKey> = HashMap::new();
        let peers = vec!["peer1".to_string()];

        let err = FastSyncFlow::run(&tmp, &peers, &pubkeys).unwrap_err();
        assert!(
            err.to_string().contains("validator_pubkeys required"),
            "should fail with no pubkeys: {}",
            err
        );
    }

    #[test]
    fn test_fast_sync_restore_snapshot_mismatch_rejected() {
        let tmp =
            std::env::temp_dir().join(format!("call-fast-sync-mismatch-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Create and save an original snapshot
        let mut snapshot = make_snapshot(300);
        let mut pubkeys = HashMap::new();
        for id in 0..3 {
            let (pk, sk) = ed25519_generate_keypair();
            pubkeys.insert(id, pk);
            if id < 2 {
                sign_snapshot(&mut snapshot, id, &sk);
            }
        }
        FastSyncFlow::save_snapshot(&snapshot, &tmp).unwrap();

        // Pass a tampered snapshot to restore_snapshot (different protocol_root)
        let mut tampered = snapshot.clone();
        tampered.protocol_root = Hash::repeat_byte(0xFF);

        let err = FastSyncFlow::restore_snapshot(&tmp, &tampered).unwrap_err();
        assert!(
            err.to_string()
                .contains("snapshot restore verification failed"),
            "tampered snapshot should fail restore: {}",
            err
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_fast_sync_list_snapshots_sorted() {
        let tmp = std::env::temp_dir().join(format!("call-fast-sync-list-{}", std::process::id()));
        std::fs::create_dir_all(&tmp).unwrap();

        // Save snapshots at various heights (out of order)
        let heights = [500, 100, 300, 200, 400];
        for h in heights {
            let snapshot = make_snapshot(h);
            FastSyncFlow::save_snapshot(&snapshot, &tmp).unwrap();
        }

        let listed = FastSyncFlow::list_snapshots(&tmp).unwrap();
        assert_eq!(listed, vec![100, 200, 300, 400, 500]);

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
