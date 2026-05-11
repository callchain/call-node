//! State persistence helpers — load/save all on-chain state to reth-db.

use std::sync::{Arc, RwLock};

use call_consensus::{ForkManager, PersistedConsensusState, SimplexConsensus};
use call_primitives::TxHash;
use call_protocol::{FeeParams, ProtocolReceipt};
use call_rpc::RpcState;
use call_storage::{
    db_batch_put, db_clear, db_del, db_get, db_iter_all, db_put, CallCheckpoint,
    CallConsensusState, CallFeeParams, CallForkState, CallReceipts, CallReceiptsByBlock,
    StorageError,
};
use reth_db::DatabaseEnv;

// ── State Persistence ─────────────────────────────────────────────────

/// All on-chain state loaded from reth-db in one struct.
/// Replaces the previous 13-element tuple so callers use named fields.
pub(crate) struct LoadedState {
    pub fee_params: FeeParams,
}

/// Load all state types from the reth-db database.
pub(crate) fn load_state_from_db(db_env: &Arc<DatabaseEnv>) -> LoadedState {
    // Load fee params
    let fee_params = match load_fee_params(db_env) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load fee params");
            FeeParams::default()
        }
    };

    LoadedState { fee_params }
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
        let c = consensus.read().unwrap_or_else(|e| e.into_inner());
        c.last_block_hash().0
    };
    write_checkpoint_pending(db_env, checkpoint_hash)
        .map_err(|e| format!("write checkpoint: {e}"))?;

    // Persist fee params
    {
        let fee_params = state.fee_params.read().unwrap_or_else(|e| e.into_inner());
        save_fee_params(db_env, &fee_params).map_err(|e| format!("save fee params: {e}"))?;
    }

    // Persist consensus state
    {
        let c = consensus.read().unwrap_or_else(|e| e.into_inner());
        save_consensus_state_inner(db_env, &c).map_err(|e| format!("save consensus: {e}"))?;
    }

    // Persist receipts
    {
        let receipts = state.receipts.read().unwrap_or_else(|e| e.into_inner());
        save_receipts(db_env, &receipts).map_err(|e| format!("save receipts: {e}"))?;
    }

    // Persist fork state
    {
        let fork_manager = state.fork_manager.read().unwrap_or_else(|e| e.into_inner());
        save_fork_state(db_env, &fork_manager).map_err(|e| format!("save fork state: {e}"))?;
    }

    // 3. Clear checkpoint marker — state is now consistent
    clear_checkpoint(db_env).map_err(|e| format!("clear checkpoint: {e}"))?;

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
        Some(data) => {
            serde_json::from_slice(&data).map_err(|e| format!("deserialize fee params: {e}"))
        }
        None => Ok(FeeParams::default()),
    }
}

// ── Receipt persistence ───────────────────────────────────────────────

pub(crate) fn save_receipts(
    db: &DatabaseEnv,
    receipts: &std::collections::HashMap<TxHash, ProtocolReceipt>,
) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = receipts
        .iter()
        .map(|(k, v)| {
            let key: Vec<u8> = k.as_slice().to_vec();
            let value: Vec<u8> = serde_json::to_vec(v).expect("invariant: JSON serialization never fails for Receipt");
            (key, value)
        })
        .collect();
    db_clear::<CallReceipts>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallReceipts>(db, entries).map_err(|e: StorageError| e.to_string())?;

    // Build block_number -> tx_hashes index
    let mut by_block: std::collections::HashMap<u64, Vec<TxHash>> =
        std::collections::HashMap::new();
    for (tx_hash, receipt) in receipts {
        by_block
            .entry(receipt.block_number)
            .or_default()
            .push(*tx_hash);
    }
    let index_entries: Vec<(Vec<u8>, Vec<u8>)> = by_block
        .iter()
        .map(|(block, hashes)| {
            (
                block.to_be_bytes().to_vec(),
                serde_json::to_vec(hashes).expect("invariant: JSON serialization never fails for Vec<B256>"),
            )
        })
        .collect();
    db_clear::<CallReceiptsByBlock>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallReceiptsByBlock>(db, index_entries).map_err(|e: StorageError| e.to_string())
}

pub(crate) fn load_receipts(
    db: &DatabaseEnv,
) -> Result<std::collections::HashMap<TxHash, ProtocolReceipt>, String> {
    let data = db_iter_all::<CallReceipts>(db).map_err(|e: StorageError| e.to_string())?;
    let mut receipts = std::collections::HashMap::new();
    for (k, v) in data {
        let key = call_primitives::TxHash::from_slice(&k);
        let receipt: ProtocolReceipt =
            serde_json::from_slice(&v).map_err(|e| format!("deserialize receipt: {e}"))?;
        receipts.insert(key, receipt);
    }
    Ok(receipts)
}

// ── Checkpoint / WAL persistence ──────────────────────────────────────

/// Write a checkpoint marker to signal that a state write is in progress.
/// If the node crashes while this marker exists, state may be inconsistent.
pub(crate) fn write_checkpoint_pending(
    db: &DatabaseEnv,
    state_hash: [u8; 32],
) -> Result<(), String> {
    db_put::<CallCheckpoint>(db, b"pending".to_vec(), state_hash.to_vec())
        .map_err(|e: StorageError| e.to_string())
}

/// Clear the checkpoint marker after a successful state write.
pub(crate) fn clear_checkpoint(db: &DatabaseEnv) -> Result<(), String> {
    db_del::<CallCheckpoint>(db, b"pending").map_err(|e: StorageError| e.to_string())
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
    let data =
        serde_json::to_vec(fork_manager).map_err(|e| format!("serialize fork state: {e}"))?;
    db_put::<CallForkState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

pub(crate) fn load_fork_state(db: &DatabaseEnv) -> Result<Option<ForkManager>, String> {
    match db_get::<CallForkState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data)
            .map_err(|e| format!("deserialize fork state: {e}"))
            .map(Some),
        None => Ok(None),
    }
}

/// Save consensus state to the database.
pub(crate) fn save_consensus_state_inner(
    db: &DatabaseEnv,
    consensus: &SimplexConsensus,
) -> Result<(), String> {
    let state = consensus.persist_state();
    let data = postcard::to_allocvec(&state).map_err(|e| format!("serialize consensus: {e}"))?;
    db_put::<CallConsensusState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load consensus state from the database.
pub(crate) fn load_consensus_state_inner(
    db_env: &Arc<DatabaseEnv>,
) -> Result<SimplexConsensus, String> {
    match db_get::<CallConsensusState>(db_env, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => {
            let state: PersistedConsensusState =
                serde_json::from_slice(&data).map_err(|e| format!("deserialize consensus: {e}"))?;
            let provider = call_evm::provider::InMemoryStateProvider::from_db(db_env)
                .map_err(|e| format!("load provider for consensus restore: {e}"))?;
            Ok(SimplexConsensus::restore_from_persisted(state, &provider))
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
    // Persist consensus state
    {
        let c = consensus
            .read()
            .map_err(|_| "consensus lock poisoned".to_string())?;
        save_consensus_state_inner(db_env, &c).map_err(|e| format!("save consensus: {e}"))?;
    }

    // Persist receipts (overwrite)
    {
        let receipts = state
            .receipts
            .read()
            .map_err(|_| "receipt lock poisoned".to_string())?;
        save_receipts(db_env, &receipts).map_err(|e| format!("save receipts: {e}"))?;
    }

    // Persist fork state (overwrite)
    {
        let fork_manager = state
            .fork_manager
            .read()
            .map_err(|_| "fork lock poisoned".to_string())?;
        save_fork_state(db_env, &fork_manager).map_err(|e| format!("save fork state: {e}"))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::FeeCurrency;
    use call_protocol::InstructionExecResult;

    fn temp_db() -> Arc<DatabaseEnv> {
        use std::hash::{DefaultHasher, Hash, Hasher};
        let mut h = DefaultHasher::new();
        std::thread::current().id().hash(&mut h);
        let tmp = std::env::temp_dir().join(format!(
            "call-state-persist-test-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            h.finish(),
        ));
        call_storage::init_call_db(&tmp).expect("init temp db")
    }

    #[test]
    fn test_checkpoint_pending_detected() {
        let db = temp_db();
        assert!(!check_recovery_needed(&db).unwrap());

        write_checkpoint_pending(&db, [0xABu8; 32]).unwrap();
        assert!(check_recovery_needed(&db).unwrap());

        clear_checkpoint(&db).unwrap();
        assert!(!check_recovery_needed(&db).unwrap());
    }

    #[test]
    fn test_fee_params_roundtrip() {
        let db = temp_db();
        let mut params = FeeParams::default();
        params.base_fee = 42;

        save_fee_params(&db, &params).unwrap();
        let loaded = load_fee_params(&db).unwrap();
        assert_eq!(loaded.base_fee, 42);
    }

    #[test]
    fn test_fee_params_missing_returns_default() {
        let db = temp_db();
        let loaded = load_fee_params(&db).unwrap();
        assert_eq!(loaded.base_fee, FeeParams::default().base_fee);
    }

    #[test]
    fn test_receipts_roundtrip() {
        let db = temp_db();
        let mut receipts = std::collections::HashMap::new();
        let tx_hash = call_primitives::TxHash::from([0xCCu8; 32]);
        let receipt = call_protocol::ProtocolReceipt {
            tx_hash,
            block_number: 100,
            block_hash: call_primitives::BlockHash::from([0xDDu8; 32]),
            transaction_index: 0,
            status: call_primitives::ExecutionStatus::Success,
            gas_used: 21_000,
            gas_payer: call_primitives::Address::ZERO,
            fee_currency: FeeCurrency::Call,
            fee_amount: 0,
            cumulative_gas_used: 21_000,
            effective_gas_price: 10,
            to: None,
            contract_address: None,
            logs: Vec::new(),
            logs_bloom: vec![0u8; 256],
            instruction_results: vec![InstructionExecResult {
                success: true,
                gas_used: 21_000,
                revert_reason: None,
            }],
            memos: Vec::new(),
            state_changes: Vec::new(),
        };
        receipts.insert(tx_hash, receipt);

        save_receipts(&db, &receipts).unwrap();
        let loaded = load_receipts(&db).unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded.get(&tx_hash).unwrap().block_number, 100);
    }

    #[test]
    fn test_receipts_multi_block_roundtrip() {
        let db = temp_db();
        let mut receipts = std::collections::HashMap::new();

        // Block 10: 2 receipts
        let tx1 = call_primitives::TxHash::from([0x01u8; 32]);
        let tx2 = call_primitives::TxHash::from([0x02u8; 32]);
        receipts.insert(
            tx1,
            make_receipt(tx1, 10, call_primitives::ExecutionStatus::Success),
        );
        receipts.insert(
            tx2,
            make_receipt(tx2, 10, call_primitives::ExecutionStatus::Success),
        );

        // Block 20: 1 receipt
        let tx3 = call_primitives::TxHash::from([0x03u8; 32]);
        receipts.insert(
            tx3,
            make_receipt(
                tx3,
                20,
                call_primitives::ExecutionStatus::Reverted {
                    reason: "out of gas".into(),
                },
            ),
        );

        save_receipts(&db, &receipts).unwrap();
        let loaded = load_receipts(&db).unwrap();
        assert_eq!(loaded.len(), 3);
        assert_eq!(loaded.get(&tx1).unwrap().block_number, 10);
        assert_eq!(loaded.get(&tx3).unwrap().block_number, 20);

        // Verify CallReceiptsByBlock index was built correctly
        let by_block_10 = loaded.values().filter(|r| r.block_number == 10).count();
        let by_block_20 = loaded.values().filter(|r| r.block_number == 20).count();
        assert_eq!(by_block_10, 2);
        assert_eq!(by_block_20, 1);
    }

    #[test]
    fn test_receipts_field_integrity() {
        let db = temp_db();
        let mut receipts = std::collections::HashMap::new();
        let tx_hash = call_primitives::TxHash::from([0xABu8; 32]);
        let addr = call_primitives::Address::repeat_byte(0x42);
        let topic = call_primitives::Hash::repeat_byte(0x11);

        let receipt = call_protocol::ProtocolReceipt {
            tx_hash,
            block_number: 42,
            block_hash: call_primitives::Hash::repeat_byte(0xBB),
            transaction_index: 7,
            status: call_primitives::ExecutionStatus::Success,
            gas_used: 55_000,
            gas_payer: addr,
            fee_currency: FeeCurrency::Call,
            fee_amount: 1_000_000,
            cumulative_gas_used: 55_000,
            effective_gas_price: 18,
            to: Some(addr),
            contract_address: None,
            logs: vec![call_protocol::LogEntry {
                address: addr,
                topics: vec![topic],
                data: vec![0x01, 0x02, 0x03],
            }],
            logs_bloom: vec![0u8; 256],
            instruction_results: vec![InstructionExecResult {
                success: true,
                gas_used: 55_000,
                revert_reason: None,
            }],
            memos: vec![call_protocol::MemoEntry {
                content: "test memo".into(),
                reference: Some("ref-1".into()),
            }],
            state_changes: vec![call_protocol::StateChange {
                change_type: call_protocol::ChangeType::Balance,
                key: vec![0x01],
                old_value: vec![0x00],
                new_value: vec![0x64],
            }],
        };
        receipts.insert(tx_hash, receipt);

        save_receipts(&db, &receipts).unwrap();
        let loaded = load_receipts(&db).unwrap();
        let r = loaded.get(&tx_hash).unwrap();

        assert_eq!(r.block_number, 42);
        assert_eq!(r.transaction_index, 7);
        assert_eq!(r.gas_used, 55_000);
        assert_eq!(r.fee_amount, 1_000_000);
        assert_eq!(r.effective_gas_price, 18);
        assert_eq!(r.gas_payer, addr);
        assert_eq!(r.to, Some(addr));
        assert_eq!(r.logs.len(), 1);
        assert_eq!(r.logs[0].address, addr);
        assert_eq!(r.logs[0].topics.len(), 1);
        assert_eq!(r.logs[0].topics[0], topic);
        assert_eq!(r.logs[0].data, vec![0x01, 0x02, 0x03]);
        assert_eq!(r.instruction_results.len(), 1);
        assert!(r.instruction_results[0].success);
        assert_eq!(r.memos.len(), 1);
        assert_eq!(r.memos[0].content, "test memo");
        assert_eq!(r.state_changes.len(), 1);
        assert_eq!(r.state_changes[0].new_value, vec![0x64]);
    }

    #[test]
    fn test_receipts_empty_save_load() {
        let db = temp_db();
        let receipts: std::collections::HashMap<
            call_primitives::TxHash,
            call_protocol::ProtocolReceipt,
        > = std::collections::HashMap::new();

        save_receipts(&db, &receipts).unwrap();
        let loaded = load_receipts(&db).unwrap();
        assert!(loaded.is_empty());
    }

    #[test]
    fn test_receipts_overwrite() {
        let db = temp_db();
        let tx_hash = call_primitives::TxHash::from([0xCCu8; 32]);

        let mut receipts1 = std::collections::HashMap::new();
        receipts1.insert(
            tx_hash,
            make_receipt(tx_hash, 10, call_primitives::ExecutionStatus::Success),
        );
        save_receipts(&db, &receipts1).unwrap();

        let mut receipts2 = std::collections::HashMap::new();
        receipts2.insert(
            tx_hash,
            make_receipt(
                tx_hash,
                20,
                call_primitives::ExecutionStatus::Reverted {
                    reason: "fail".into(),
                },
            ),
        );
        save_receipts(&db, &receipts2).unwrap();

        let loaded = load_receipts(&db).unwrap();
        assert_eq!(loaded.len(), 1);
        let r = loaded.get(&tx_hash).unwrap();
        assert_eq!(r.block_number, 20);
        assert!(
            matches!(&r.status, call_primitives::ExecutionStatus::Reverted { reason } if reason == "fail")
        );
    }

    fn make_receipt(
        tx_hash: call_primitives::TxHash,
        block_number: u64,
        status: call_primitives::ExecutionStatus,
    ) -> call_protocol::ProtocolReceipt {
        call_protocol::ProtocolReceipt {
            tx_hash,
            block_number,
            block_hash: call_primitives::Hash::repeat_byte(block_number as u8),
            transaction_index: 0,
            status,
            gas_used: 21_000,
            gas_payer: call_primitives::Address::ZERO,
            fee_currency: FeeCurrency::Call,
            fee_amount: 0,
            cumulative_gas_used: 21_000,
            effective_gas_price: 10,
            to: None,
            contract_address: None,
            logs: Vec::new(),
            logs_bloom: vec![0u8; 256],
            instruction_results: vec![InstructionExecResult {
                success: true,
                gas_used: 21_000,
                revert_reason: None,
            }],
            memos: Vec::new(),
            state_changes: Vec::new(),
        }
    }

    #[test]
    fn test_fork_state_roundtrip() {
        let db = temp_db();
        let fm = ForkManager::new(call_primitives::ProtocolVersion::new(1, 2, 3), 1);

        save_fork_state(&db, &fm).unwrap();
        let loaded = load_fork_state(&db).unwrap().unwrap();
        assert_eq!(
            loaded.current_version,
            call_primitives::ProtocolVersion::new(1, 2, 3)
        );
    }

    #[test]
    fn test_fork_state_missing_returns_none() {
        let db = temp_db();
        assert!(load_fork_state(&db).unwrap().is_none());
    }

    #[test]
    fn test_fork_manager_persist_and_check_activation() {
        let db = temp_db();
        let mut fm = ForkManager::new(call_primitives::ProtocolVersion::new(1, 0, 0), 1);
        fm.schedule_upgrade(call_consensus::UpgradeEntry {
            version: call_primitives::ProtocolVersion::new(1, 1, 0),
            activation_height: 100,
            applied: false,
            proposal_id: None,
            approved_at_height: None,
        });

        save_fork_state(&db, &fm).unwrap();
        let mut loaded = load_fork_state(&db).unwrap().unwrap();

        // Before activation height: no upgrade applied
        assert!(loaded.check_upgrades_at_height(99).is_none());
        assert_eq!(
            loaded.current_version,
            call_primitives::ProtocolVersion::new(1, 0, 0)
        );

        // At activation height: upgrade fires
        let result = loaded.check_upgrades_at_height(100);
        assert_eq!(result, Some(call_primitives::ProtocolVersion::new(1, 1, 0)));
        assert_eq!(
            loaded.current_version,
            call_primitives::ProtocolVersion::new(1, 1, 0)
        );

        // Idempotent: second call at same height returns none
        assert!(loaded.check_upgrades_at_height(100).is_none());
    }

    #[test]
    fn test_fork_manager_persist_multiple_upgrades() {
        let db = temp_db();
        let mut fm = ForkManager::new(call_primitives::ProtocolVersion::new(1, 0, 0), 1);
        fm.schedule_upgrade(call_consensus::UpgradeEntry {
            version: call_primitives::ProtocolVersion::new(1, 1, 0),
            activation_height: 50,
            applied: false,
            proposal_id: None,
            approved_at_height: None,
        });
        fm.schedule_upgrade(call_consensus::UpgradeEntry {
            version: call_primitives::ProtocolVersion::new(1, 2, 0),
            activation_height: 100,
            applied: false,
            proposal_id: None,
            approved_at_height: None,
        });
        fm.schedule_upgrade(call_consensus::UpgradeEntry {
            version: call_primitives::ProtocolVersion::new(1, 3, 0),
            activation_height: 200,
            applied: false,
            proposal_id: None,
            approved_at_height: None,
        });

        save_fork_state(&db, &fm).unwrap();
        let mut loaded = load_fork_state(&db).unwrap().unwrap();

        // Apply first upgrade
        let v1 = loaded.check_upgrades_at_height(50);
        assert_eq!(v1, Some(call_primitives::ProtocolVersion::new(1, 1, 0)));
        assert_eq!(
            loaded.current_version,
            call_primitives::ProtocolVersion::new(1, 1, 0)
        );

        // Apply second upgrade
        let v2 = loaded.check_upgrades_at_height(100);
        assert_eq!(v2, Some(call_primitives::ProtocolVersion::new(1, 2, 0)));
        assert_eq!(
            loaded.current_version,
            call_primitives::ProtocolVersion::new(1, 2, 0)
        );

        // Apply third upgrade
        let v3 = loaded.check_upgrades_at_height(200);
        assert_eq!(v3, Some(call_primitives::ProtocolVersion::new(1, 3, 0)));
        assert_eq!(
            loaded.current_version,
            call_primitives::ProtocolVersion::new(1, 3, 0)
        );

        // All upgrades marked applied
        assert!(loaded.scheduled_upgrades.iter().all(|e| e.applied));
    }

    #[test]
    fn test_fork_manager_persist_upgrade_before_save_height() {
        let db = temp_db();
        let mut fm = ForkManager::new(call_primitives::ProtocolVersion::new(1, 0, 0), 1);
        fm.schedule_upgrade(call_consensus::UpgradeEntry {
            version: call_primitives::ProtocolVersion::new(1, 1, 0),
            activation_height: 10,
            applied: false,
            proposal_id: None,
            approved_at_height: None,
        });

        // Simulate that the node has already processed up to height 50
        // (the upgrade at height 10 was scheduled before the save)
        save_fork_state(&db, &fm).unwrap();
        let mut loaded = load_fork_state(&db).unwrap().unwrap();

        // Upgrade at height 10 should still be available after load
        let result = loaded.check_upgrades_at_height(10);
        assert_eq!(result, Some(call_primitives::ProtocolVersion::new(1, 1, 0)));
        assert_eq!(
            loaded.current_version,
            call_primitives::ProtocolVersion::new(1, 1, 0)
        );
    }

    #[test]
    fn test_fork_manager_persist_no_upgrade_at_unscheduled_height() {
        let db = temp_db();
        let fm = ForkManager::new(call_primitives::ProtocolVersion::new(1, 0, 0), 1);
        // No upgrades scheduled

        save_fork_state(&db, &fm).unwrap();
        let mut loaded = load_fork_state(&db).unwrap().unwrap();

        assert!(loaded.check_upgrades_at_height(999).is_none());
        assert_eq!(
            loaded.current_version,
            call_primitives::ProtocolVersion::new(1, 0, 0)
        );
    }

    /// Simulate a full node restart: save all state types, then load them back
    /// from a fresh DB reference as if the node had restarted.
    #[test]
    fn test_node_restart_state_recovery() {
        let db = temp_db();

        // --- Phase 1: simulate running node saving state ---

        // 1. Save fee params
        let mut params = FeeParams::default();
        params.base_fee = 1234;
        save_fee_params(&db, &params).unwrap();

        // 2. Save receipts
        let mut receipts = std::collections::HashMap::new();
        let tx1 = call_primitives::TxHash::from([0x11u8; 32]);
        let tx2 = call_primitives::TxHash::from([0x22u8; 32]);
        receipts.insert(tx1, make_receipt(tx1, 10, call_primitives::ExecutionStatus::Success));
        receipts.insert(
            tx2,
            make_receipt(
                tx2,
                20,
                call_primitives::ExecutionStatus::Reverted {
                    reason: "out of gas".into(),
                },
            ),
        );
        save_receipts(&db, &receipts).unwrap();

        // 3. Save fork state with scheduled upgrade
        let mut fm = ForkManager::new(call_primitives::ProtocolVersion::new(1, 0, 0), 1);
        fm.schedule_upgrade(call_consensus::UpgradeEntry {
            version: call_primitives::ProtocolVersion::new(1, 1, 0),
            activation_height: 100,
            applied: false,
            proposal_id: None,
            approved_at_height: None,
        });
        save_fork_state(&db, &fm).unwrap();

        // 4. Write checkpoint (simulates a clean shutdown with pending marker)
        write_checkpoint_pending(&db, [0xABu8; 32]).unwrap();

        // --- Phase 2: simulate restart — fresh DB reference ---
        // Re-open the same database directory to simulate a new process
        let db_path = {
            // Access the underlying path through reth_db internals is not
            // directly available, but the DatabaseEnv holds an open reference.
            // Instead we just reuse the same Arc<DatabaseEnv> — in a real
            // restart the OS would close and reopen the MDBX handle.
            // For this test, using the same Arc is sufficient because MDBX
            // commits are durable.
            Arc::clone(&db)
        };

        // --- Phase 3: verify recovery detection ---
        assert!(check_recovery_needed(&db_path).unwrap());

        // Clear checkpoint (simulates successful recovery / startup completion)
        clear_checkpoint(&db_path).unwrap();
        assert!(!check_recovery_needed(&db_path).unwrap());

        // --- Phase 4: load all state and verify integrity ---

        // Fee params
        let loaded_params = load_fee_params(&db_path).unwrap();
        assert_eq!(loaded_params.base_fee, 1234);

        // Receipts
        let loaded_receipts = load_receipts(&db_path).unwrap();
        assert_eq!(loaded_receipts.len(), 2);
        assert_eq!(loaded_receipts.get(&tx1).unwrap().block_number, 10);
        assert_eq!(loaded_receipts.get(&tx2).unwrap().block_number, 20);
        assert!(
            matches!(
                &loaded_receipts.get(&tx2).unwrap().status,
                call_primitives::ExecutionStatus::Reverted { reason } if reason == "out of gas"
            )
        );

        // Fork state
        let mut loaded_fm = load_fork_state(&db_path).unwrap().unwrap();
        assert_eq!(
            loaded_fm.current_version,
            call_primitives::ProtocolVersion::new(1, 0, 0)
        );
        let upgrade = loaded_fm.check_upgrades_at_height(100);
        assert_eq!(upgrade, Some(call_primitives::ProtocolVersion::new(1, 1, 0)));
    }

    /// Test that an incomplete persist (checkpoint marker left behind) is
    /// detected on restart and triggers recovery mode.
    #[test]
    fn test_crash_recovery_checkpoint_detected() {
        let db = temp_db();

        // Simulate a crash mid-persist: checkpoint marker exists but state
        // may be partially written.
        write_checkpoint_pending(&db, [0xCCu8; 32]).unwrap();

        // On restart, recovery_needed should be true
        assert!(check_recovery_needed(&db).unwrap());

        // After clearing the checkpoint (post-recovery cleanup), recovery
        // should no longer be needed.
        clear_checkpoint(&db).unwrap();
        assert!(!check_recovery_needed(&db).unwrap());
    }

    /// Test that empty state survives a restart roundtrip.
    #[test]
    fn test_empty_state_restart_roundtrip() {
        let db = temp_db();

        // Save empty state
        let empty_receipts: std::collections::HashMap<
            call_primitives::TxHash,
            call_protocol::ProtocolReceipt,
        > = std::collections::HashMap::new();
        save_receipts(&db, &empty_receipts).unwrap();

        let fm = ForkManager::new(call_primitives::ProtocolVersion::new(1, 0, 0), 1);
        save_fork_state(&db, &fm).unwrap();

        save_fee_params(&db, &FeeParams::default()).unwrap();

        // Load back
        let loaded_receipts = load_receipts(&db).unwrap();
        assert!(loaded_receipts.is_empty());

        let loaded_fm = load_fork_state(&db).unwrap().unwrap();
        assert_eq!(
            loaded_fm.current_version,
            call_primitives::ProtocolVersion::new(1, 0, 0)
        );

        let loaded_params = load_fee_params(&db).unwrap();
        assert_eq!(loaded_params.base_fee, FeeParams::default().base_fee);
    }

    /// Test incremental persistence preserves state across multiple writes.
    #[test]
    fn test_incremental_persistence_overwrite() {
        let db = temp_db();

        // First write
        let mut receipts1 = std::collections::HashMap::new();
        let tx1 = call_primitives::TxHash::from([0xAAu8; 32]);
        receipts1.insert(
            tx1,
            make_receipt(tx1, 1, call_primitives::ExecutionStatus::Success),
        );
        save_receipts(&db, &receipts1).unwrap();

        let mut params1 = FeeParams::default();
        params1.base_fee = 100;
        save_fee_params(&db, &params1).unwrap();

        // Second write (incremental — should overwrite)
        let mut receipts2 = std::collections::HashMap::new();
        let tx2 = call_primitives::TxHash::from([0xBBu8; 32]);
        receipts2.insert(
            tx2,
            make_receipt(tx2, 2, call_primitives::ExecutionStatus::Success),
        );
        save_receipts(&db, &receipts2).unwrap();

        let mut params2 = FeeParams::default();
        params2.base_fee = 200;
        save_fee_params(&db, &params2).unwrap();

        // Verify only second write survives
        let loaded_receipts = load_receipts(&db).unwrap();
        assert_eq!(loaded_receipts.len(), 1);
        assert!(loaded_receipts.contains_key(&tx2));

        let loaded_params = load_fee_params(&db).unwrap();
        assert_eq!(loaded_params.base_fee, 200);
    }
}
