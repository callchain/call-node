//! State persistence helpers — load/save all on-chain state to reth-db.

use std::sync::{Arc, RwLock};

use call_consensus::{SimplexConsensus, ForkManager, PersistedConsensusState};
use call_protocol::{
    FeeParams, ProtocolReceipt,
};
use call_evm::EvmState;
use call_primitives::TxHash;
use call_rpc::RpcState;
use call_storage::{
    StorageError,
    db_put, db_batch_put, db_clear, db_iter_all, db_get, db_del,
    CallEvmAccounts,
    CallConsensusState,
    CallReceipts, CallReceiptsByBlock, CallForkState, CallCheckpoint,
    CallFeeParams,
};
use reth_db::DatabaseEnv;

// ── State Persistence ─────────────────────────────────────────────────

/// All on-chain state loaded from reth-db in one struct.
/// Replaces the previous 13-element tuple so callers use named fields.
pub(crate) struct LoadedState {
    pub evm_state: EvmState,
    pub fee_params: FeeParams,
}

/// Load all state types from the reth-db database.
pub(crate) fn load_state_from_db(db_env: &Arc<DatabaseEnv>) -> LoadedState {
    // Load EVM accounts
    let evm_state = match load_evm_accounts_inner(db_env) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load evm accounts");
            EvmState::new()
        }
    };

    // Load fee params
    let fee_params = match load_fee_params(db_env) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load fee params");
            FeeParams::default()
        }
    };

    LoadedState {
        evm_state,
        fee_params,
    }
}

/// Persist all state types to the reth-db database.
/// Uses a checkpoint marker to detect incomplete writes on crash recovery.
pub(crate) fn persist_state_to_db(
    db_env: &Arc<DatabaseEnv>,
    state: &Arc<RpcState>,
    consensus: &Arc<RwLock<SimplexConsensus>>,
) -> Result<(), String> {
    // 1. Write pending checkpoint marker
    let checkpoint_hash = {
        let c = consensus.read().unwrap();
        c.last_block_hash().0
    };
    write_checkpoint_pending(db_env, checkpoint_hash)
        .map_err(|e| format!("write checkpoint: {e}"))?;

    // Persist EVM state
    {
        let evm = state.evm_state.read().unwrap();
        save_evm_accounts_inner(db_env, &evm)
            .map_err(|e| format!("save evm: {e}"))?;
    }

    // Persist fee params
    {
        let fee_params = state.fee_params.read().unwrap();
        save_fee_params(db_env, &fee_params)
            .map_err(|e| format!("save fee params: {e}"))?;
    }

    // Persist consensus state
    {
        let c = consensus.read().unwrap();
        save_consensus_state_inner(db_env, &c)
            .map_err(|e| format!("save consensus: {e}"))?;
    }

    // Persist receipts
    {
        let receipts = state.receipts.read().unwrap();
        save_receipts(db_env, &receipts)
            .map_err(|e| format!("save receipts: {e}"))?;
    }

    // Persist fork state
    {
        let fork_manager = state.fork_manager.read().unwrap();
        save_fork_state(db_env, &fork_manager)
            .map_err(|e| format!("save fork state: {e}"))?;
    }

    // 3. Clear checkpoint marker — state is now consistent
    clear_checkpoint(db_env)
        .map_err(|e| format!("clear checkpoint: {e}"))?;

    Ok(())
}

/// Load EVM accounts from DB
pub(crate) fn load_evm_accounts_inner(db: &DatabaseEnv) -> Result<EvmState, String> {
    let data = db_iter_all::<CallEvmAccounts>(db).map_err(|e: StorageError| e.to_string())?;
    let mut state = EvmState::new();
    for (k, v) in data {
        let addr: alloy_primitives::Address = serde_json::from_slice(&k).map_err(|e: serde_json::Error| e.to_string())?;
        let account: call_evm::EvmAccount = serde_json::from_slice(&v).map_err(|e: serde_json::Error| e.to_string())?;
        let existing = state.get_account_mut(&addr);
        *existing = account;
    }
    Ok(state)
}

/// Save EVM accounts to DB
pub(crate) fn save_evm_accounts_inner(db: &DatabaseEnv, state: &EvmState) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .get_all_accounts()
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    db_clear::<CallEvmAccounts>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallEvmAccounts>(db, entries).map_err(|e: StorageError| e.to_string())?;
    Ok(())
}

/// Save fee params to the database.
pub(crate) fn save_fee_params(db: &DatabaseEnv, fee_params: &FeeParams) -> Result<(), String> {
    let data = serde_json::to_vec(fee_params).map_err(|e| format!("serialize fee params: {e}"))?;
    db_put::<CallFeeParams>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load fee params from the database.
pub(crate) fn load_fee_params(db: &DatabaseEnv) -> Result<FeeParams, String> {
    match db_get::<CallFeeParams>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize fee params: {e}")),
        None => Ok(FeeParams::default()),
    }
}

// ── Receipt persistence ───────────────────────────────────────────────

pub(crate) fn save_receipts(db: &DatabaseEnv, receipts: &std::collections::HashMap<TxHash, ProtocolReceipt>) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = receipts
        .iter()
        .map(|(k, v)| {
            let key: Vec<u8> = k.as_slice().to_vec();
            let value: Vec<u8> = serde_json::to_vec(v).unwrap();
            (key, value)
        })
        .collect();
    db_clear::<CallReceipts>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallReceipts>(db, entries).map_err(|e: StorageError| e.to_string())?;

    // Build block_number -> tx_hashes index
    let mut by_block: std::collections::HashMap<u64, Vec<TxHash>> = std::collections::HashMap::new();
    for (tx_hash, receipt) in receipts {
        by_block.entry(receipt.block_number).or_default().push(*tx_hash);
    }
    let index_entries: Vec<(Vec<u8>, Vec<u8>)> = by_block
        .iter()
        .map(|(block, hashes)| {
            (block.to_be_bytes().to_vec(), serde_json::to_vec(hashes).unwrap())
        })
        .collect();
    db_clear::<CallReceiptsByBlock>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallReceiptsByBlock>(db, index_entries).map_err(|e: StorageError| e.to_string())
}

pub(crate) fn load_receipts(db: &DatabaseEnv) -> Result<std::collections::HashMap<TxHash, ProtocolReceipt>, String> {
    let data = db_iter_all::<CallReceipts>(db).map_err(|e: StorageError| e.to_string())?;
    let mut receipts = std::collections::HashMap::new();
    for (k, v) in data {
        let key = call_primitives::TxHash::from_slice(&k);
        let receipt: ProtocolReceipt = serde_json::from_slice(&v).map_err(|e| format!("deserialize receipt: {e}"))?;
        receipts.insert(key, receipt);
    }
    Ok(receipts)
}

// ── Checkpoint / WAL persistence ──────────────────────────────────────

/// Write a checkpoint marker to signal that a state write is in progress.
/// If the node crashes while this marker exists, state may be inconsistent.
pub(crate) fn write_checkpoint_pending(db: &DatabaseEnv, state_hash: [u8; 32]) -> Result<(), String> {
    db_put::<CallCheckpoint>(db, b"pending".to_vec(), state_hash.to_vec())
        .map_err(|e: StorageError| e.to_string())
}

/// Clear the checkpoint marker after a successful state write.
pub(crate) fn clear_checkpoint(db: &DatabaseEnv) -> Result<(), String> {
    db_del::<CallCheckpoint>(db, b"pending")
        .map_err(|e: StorageError| e.to_string())
}

/// Check if a pending checkpoint marker exists (indicates potential crash).
pub(crate) fn check_recovery_needed(db: &DatabaseEnv) -> Result<bool, String> {
    match db_get::<CallCheckpoint>(db, b"pending").map_err(|e: StorageError| e.to_string())? {
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

// ── Fork state persistence ────────────────────────────────────────────

pub(crate) fn save_fork_state(db: &DatabaseEnv, fork_manager: &ForkManager) -> Result<(), String> {
    let data = serde_json::to_vec(fork_manager).map_err(|e| format!("serialize fork state: {e}"))?;
    db_put::<CallForkState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

pub(crate) fn load_fork_state(db: &DatabaseEnv) -> Result<Option<ForkManager>, String> {
    match db_get::<CallForkState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize fork state: {e}")).map(Some),
        None => Ok(None),
    }
}

/// Save consensus state to the database.
pub(crate) fn save_consensus_state_inner(db: &DatabaseEnv, consensus: &SimplexConsensus) -> Result<(), String> {
    let state = consensus.persist_state();
    let data = bincode::serialize(&state).map_err(|e| format!("serialize consensus: {e}"))?;
    db_put::<CallConsensusState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load consensus state from the database.
pub(crate) fn load_consensus_state_inner(db: &DatabaseEnv, evm_state: &EvmState) -> Result<SimplexConsensus, String> {
    match db_get::<CallConsensusState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => {
            let state: PersistedConsensusState = serde_json::from_slice(&data)
                .map_err(|e| format!("deserialize consensus: {e}"))?;
            Ok(SimplexConsensus::restore_from_persisted(state, evm_state))
        }
        None => Err("no consensus state in db".to_string()),
    }
}

// ── Incremental State Persistence ────────────────────────────────────
//
// Instead of clearing and rewriting entire tables every 100 blocks,
// write only changed entries after each block. Full rebuild runs
// every 1000 blocks as a safety net.

/// Incrementally persist state after a block.
/// Unlike `persist_state_to_db` which clears and rewrites all tables,
/// this appends/overwrites only changed entries.
pub(crate) fn persist_state_incremental(
    db_env: &Arc<DatabaseEnv>,
    state: &Arc<RpcState>,
    consensus: &Arc<RwLock<SimplexConsensus>>,
) -> Result<(), String> {
    // Persist EVM state (overwrite existing entries, no clear)
    {
        let evm = state.evm_state.read().map_err(|_| "evm lock poisoned".to_string())?;
        save_evm_accounts_no_clear(db_env, &evm)?;
    }

    // Persist consensus state
    {
        let c = consensus.read().map_err(|_| "consensus lock poisoned".to_string())?;
        save_consensus_state_inner(db_env, &c)
            .map_err(|e| format!("save consensus: {e}"))?;
    }

    // Persist receipts (overwrite)
    {
        let receipts = state.receipts.read().map_err(|_| "receipt lock poisoned".to_string())?;
        save_receipts(db_env, &receipts)
            .map_err(|e| format!("save receipts: {e}"))?;
    }

    // Persist fork state (overwrite)
    {
        let fork_manager = state.fork_manager.read().map_err(|_| "fork lock poisoned".to_string())?;
        save_fork_state(db_env, &fork_manager)
            .map_err(|e| format!("save fork state: {e}"))?;
    }

    Ok(())
}

/// Save EVM accounts without clearing the table first.
pub(crate) fn save_evm_accounts_no_clear(db: &DatabaseEnv, state: &EvmState) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .get_all_accounts()
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    for (k, v) in entries {
        db_put::<CallEvmAccounts>(db, k, v).map_err(|e: StorageError| e.to_string())?;
    }
    Ok(())
}

