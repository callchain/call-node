//! BFT consensus event loop — propose / verify / finalize / broadcast.

use std::sync::{Arc, RwLock};
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, oneshot};

use call_consensus::{
    Block, BlockCache, BlockExecutionResult, ConsensusDigest, FinalizationInfo, ProposeRequest,
    SimplexConsensus, VerifyRequest,
};
use call_primitives::BlockHash;
use crate::EpochRotationReason;
use call_network::{
    Network, NetworkMessage, BlockAnnouncement, OraclePriceRequest, SyncRequest,
    EpochBoundarySignal,
};
use call_governance::GovernanceManager;
use call_oracle::{OracleManager, ORACLE_UPDATE_INTERVAL};
use call_primitives::{Address, Hash, FeeCurrency, TxHash};
use call_protocol::ProtocolReceipt;
use call_rpc::{RpcState, SubscriptionManager};
use call_storage::{CallDb, PruneState, StateRoots, produce_state_snapshot};
use call_transaction_pool::Mempool;
use commonware_codec::extensions::DecodeExt;
use commonware_cryptography::Digest;
use crate::{current_timestamp_millis, persist_block, load_block, persist_state_incremental, persist_state_to_db};
use crate::network_handler::{BLOCK_CHANNEL, ORACLE_CHANNEL, SYNC_CHANNEL};
use crate::state_persist::save_fork_state;

fn apply_rollback_plan(
    plan: &call_consensus::RollbackPlan,
    state: &Arc<RpcState>,
    consensus: &Arc<RwLock<SimplexConsensus>>,
    block_cache: &Arc<std::sync::Mutex<BlockCache>>,
    parent_hash: &mut BlockHash,
    prune_state: &mut PruneState,
    data_dir: &std::path::Path,
    db_env: &Arc<reth_db::DatabaseEnv>,
    oracle: &Arc<RwLock<OracleManager>>,
    governance: &Arc<RwLock<GovernanceManager>>,
) {
    tracing::warn!(
        target_height = plan.target_height,
        target_version = ?plan.target_version,
        "applying emergency rollback"
    );

    // 1. Reset in-memory block height
    {
        let mut current_block = state.current_block.write().unwrap();
        *current_block = plan.target_height;
    }

    // 2. Reset consensus height
    {
        let mut c = consensus.write().unwrap();
        c.set_current_height(plan.target_height);
    }

    // 3. Clear block cache
    {
        let mut cache = block_cache.lock().unwrap();
        cache.clear();
    }

    // 4. Clear execution results above target (via prune helper)
    {
        let mut receipts = state.receipts.write().unwrap();
        receipts.retain(|_, r| r.block_number <= plan.target_height);
    }

    // 5. Reset governance block
    {
        let mut gov = governance.write().unwrap();
        gov.set_current_block(plan.target_height);
    }

    // 6. Reset oracle block tracking
    {
        let mut oracle_guard = oracle.write().unwrap();
        oracle_guard.set_current_block(plan.target_height);
    }

    // 8. Delete block files above target height
    let blocks_dir = data_dir.join("blocks");
    if blocks_dir.exists() {
        if let Ok(entries) = std::fs::read_dir(&blocks_dir) {
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    if let Some(height_str) = name.strip_prefix("block-").and_then(|s| s.strip_suffix(".json")) {
                        if let Ok(height) = height_str.parse::<u64>() {
                            if height > plan.target_height {
                                let _ = std::fs::remove_file(entry.path());
                            }
                        }
                    }
                }
            }
        }
    }

    // 9. Load parent hash of the target block (or genesis if unavailable)
    *parent_hash = load_block(data_dir, plan.target_height)
        .map(|b| b.header.hash())
        .unwrap_or_else(|| BlockHash::ZERO);

    // 10. Clear prune state entries above target
    prune_state.retain_up_to(plan.target_height);

    // 11. Persist the structural rollback marker to DB
    let _ = save_fork_state(db_env, &state.fork_manager.read().unwrap());

    tracing::info!(
        target_height = plan.target_height,
        parent_hash = ?parent_hash,
        "emergency rollback applied — restart recommended for full state consistency"
    );
}

/// BFT event loop — handles propose / verify / finalize / broadcast from the
/// Commonware Simplex BFT engine running in a background thread.
pub(crate) async fn bft_event_loop(
    mut propose_rx: mpsc::Receiver<ProposeRequest>,
    mut verify_rx: mpsc::Receiver<VerifyRequest>,
    mut finalize_rx: mpsc::Receiver<FinalizationInfo>,
    mut broadcast_rx: mpsc::Receiver<Vec<u8>>,
    state: Arc<RpcState>,
    mempool: Arc<RwLock<Mempool>>,
    consensus: Arc<RwLock<SimplexConsensus>>,
    block_cache: Arc<std::sync::Mutex<BlockCache>>,
    db: CallDb,
    mut prune_state: PruneState,
    subscriptions: SubscriptionManager,
    mut parent_hash: BlockHash,
    network: Option<Arc<dyn Network>>,
    data_dir: PathBuf,
    telemetry: Arc<crate::telemetry::TelemetryRegistry>,
    _audit_log: Arc<RwLock<crate::logging::AuditLog>>,
    epoch_number: u64,
    exit_tx: oneshot::Sender<EpochRotationReason>,
    subset_pubkeys: Vec<[u8; 32]>,
    my_pubkey: [u8; 32],
    oracle: Arc<RwLock<OracleManager>>,
    governance: Arc<RwLock<GovernanceManager>>,
) {
    let mut execution_results: std::collections::HashMap<
        ConsensusDigest,
        BlockExecutionResult,
    > = std::collections::HashMap::new();
    let prune_config = call_storage::PruneConfig::default();

    // Build a mapping from ed25519 pubkey -> validator id for propose lookups
    let pubkey_to_id = {
        let evm_state = state.evm_state.read().unwrap();
        let count = call_consensus::exec::state_accessors::read_validator_count(&evm_state);
        let mut map = std::collections::HashMap::new();
        for id in 1..=count {
            let addr = call_consensus::exec::state_accessors::read_validator_addr(&evm_state, id);
            if addr != Address::ZERO {
                let pk = call_consensus::exec::state_accessors::read_validator_pubkey(&evm_state, addr);
                if let Ok(pk) = commonware_cryptography::ed25519::PublicKey::decode(&pk[..]) {
                    map.insert(pk, id as u32);
                }
            }
        }
        map
    };

    // Epoch boundary quorum waiting state
    let mut awaiting_quorum = false;
    let mut boundary_height: u64 = 0;
    let mut quorum_wait_start: Option<std::time::Instant> = None;
    let quorum_timeout = std::time::Duration::from_secs(30);

    // Helper: compute quorum threshold from subset size
    let quorum_threshold = (subset_pubkeys.len() * 2).div_ceil(3);

    loop {
        // Check for pending emergency rollback and apply if present
        if let Some(plan) = state.pending_rollback.write().unwrap().take() {
            apply_rollback_plan(&plan, &state, &consensus, &block_cache, &mut parent_hash, &mut prune_state, &data_dir, &db.db, &oracle, &governance);
        }

        // === Epoch boundary quorum check ===
        if awaiting_quorum {
            let (ready, timed_out) = {
                let peer_heights = state.peer_heights.read().unwrap();
                let ready_count = subset_pubkeys
                    .iter()
                    .filter(|pk| {
                        let pk_hex = hex::encode(pk);
                        peer_heights.get(&pk_hex).is_some_and(|h| *h >= boundary_height)
                    })
                    .count();
                let ready = ready_count >= quorum_threshold;
                let timed_out = quorum_wait_start
                    .is_some_and(|t| t.elapsed() >= quorum_timeout);
                (ready, timed_out)
            };

            if ready {
                tracing::info!(
                    epoch = epoch_number + 1,
                    boundary_height,
                    "BFT: quorum ready for epoch rotation"
                );
                let _ = exit_tx.send(EpochRotationReason::EpochBoundary);
                break;
            } else if timed_out {
                tracing::warn!(
                    epoch = epoch_number + 1,
                    boundary_height,
                    "BFT: quorum wait timed out, rotating anyway"
                );
                let _ = exit_tx.send(EpochRotationReason::EpochBoundary);
                break;
            }
        }

        // Check if network layer requested an engine restart (e.g. sync crossed epoch)
        if state.engine_restart_signal.load(std::sync::atomic::Ordering::Relaxed) {
            state.engine_restart_signal.store(false, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("BFT: engine restart requested by sync, exiting event loop");
            let _ = exit_tx.send(EpochRotationReason::EpochBoundary);
            break;
        }

        tokio::select! {
            Some((context, reply_tx)) = propose_rx.recv() => {
                // The BFT engine selected us as the leader for this view.
                // Build a block from mempool and return its digest.
                let selection = { mempool.write().unwrap().select_transactions() };

                // Map the BFT leader pubkey to our validator id
                let proposer = pubkey_to_id.get(&context.leader).copied().unwrap_or(0);
                let height = {
                    let c = consensus.read().unwrap();
                    c.current_height()
                };

                // Refresh parent_hash in case sync recovered missed blocks independently
                parent_hash = {
                    let c = consensus.read().unwrap();
                    c.last_block_hash()
                };

                let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

                let version = {
                    let fm = state.fork_manager.read().unwrap();
                    fm.current_version()
                };

                // Request oracle price submissions at boundary intervals
                let is_oracle_boundary = height.is_multiple_of(ORACLE_UPDATE_INTERVAL);
                if is_oracle_boundary {
                    if let Some(ref net) = network {
                        let tracked = { oracle.read().unwrap().tracked_pairs.clone() };
                        if !tracked.is_empty() {
                            let request = OraclePriceRequest {
                                pairs: tracked,
                                block: height,
                                requester_id: proposer,
                            };
                            let msg = bincode::serialize(&NetworkMessage::OraclePriceRequest(request))
                                .expect("serialize oracle request");
                            net.broadcast(ORACLE_CHANNEL, msg).await;
                            let delay_ms = state.consensus_params.read()
                                .ok()
                                .map(|p| p.oracle_request_delay_ms)
                                .unwrap_or(200);
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                        }
                    }
                }

                let mut block = Block::new(
                    height,
                    parent_hash,
                    current_timestamp_millis(),
                    proposer,
                    version,
                    evm_txs,
                );

                // Execute the block on cloned state — do NOT modify shared state.
                // State changes are only applied after BFT finalization.
                let exec_start = Instant::now();
                let result = state.read_all().execute_block_cloned(&block, height);
                telemetry.record_tx_latency(exec_start.elapsed().as_millis() as u64);

                match result {
                    Ok(result) => {
                        block.finalize(&result);
                        telemetry.record_block_produced();

                        let digest = ConsensusDigest::from(block.header.hash());

                        // Cache block and execution result for verify/finalize
                        block_cache.lock().unwrap().insert(digest, block);
                        execution_results.insert(digest, result);

                        let _ = reply_tx.send(digest);
                        tracing::debug!(height, proposer, "BFT propose: block built");
                    }
                    Err(e) => {
                        tracing::warn!(error = ?e, height, "BFT propose: block execution failed");
                        let _ = reply_tx.send(ConsensusDigest::EMPTY);
                    }
                }
            }

            Some((_context, digest, reply_tx)) = verify_rx.recv() => {
                // Another validator proposed this block; verify it.
                // Retry briefly to allow P2P relay delivery.
                let mut block = {
                    let cache = block_cache.lock().unwrap();
                    cache.get(&digest).cloned()
                };

                if block.is_none() {
                    for attempt in 1..=30 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        let cache = block_cache.lock().unwrap();
                        if let Some(b) = cache.get(&digest) {
                            block = Some(b.clone());
                            break;
                        }
                        drop(cache);
                        if attempt == 30 {
                            let cache_size = block_cache.lock().unwrap().len();
                            tracing::warn!(digest = %digest, cache_size, "BFT verify: block not in cache after waiting 3s");
                        }
                    }
                }

                let valid = if let Some(block) = block {
                    let height = block.header.height;
                    let exec_start = Instant::now();

                    // Verify on cloned state — do NOT modify shared state.
                    // State root check ensures the proposer computed roots honestly.
                    let result = state.read_all().execute_block_cloned(&block, height);
                    telemetry.record_tx_latency(exec_start.elapsed().as_millis() as u64);

                    match result {
                        Ok(result) => {
                            // State root verification: re-computed root must match header root
                            let state_ok = result.state_root == block.header.state_root;
                            if !state_ok {
                                tracing::warn!(
                                    digest = %digest,
                                    height,
                                    state_ok,
                                    result_state = %result.state_root,
                                    header_state = %block.header.state_root,
                                    "BFT verify: state root mismatch — block rejected"
                                );
                            }
                            state_ok
                        }
                        Err(e) => {
                            tracing::warn!(error = ?e, digest = %digest, "BFT verify: execution failed");
                            false
                        }
                    }
                } else {
                    false
                };

                let _ = reply_tx.send(valid);
            }

            Some(info) = finalize_rx.recv() => {
                // Block has been finalized by BFT consensus.
                let mut block = {
                    let mut cache = block_cache.lock().unwrap();
                    cache.remove(&info.digest)
                };

                // Cache miss: try disk, then trigger sync as last resort
                if block.is_none() {
                    let height = {
                        let c = consensus.read().unwrap();
                        c.current_height()
                    };
                    if let Some(b) = load_block(&data_dir, height) {
                        tracing::info!(digest = %info.digest, height, "BFT finalize: block recovered from disk");
                        block = Some(b);
                    } else {
                        tracing::warn!(digest = %info.digest, height, "BFT finalize: block not in cache or disk, triggering sync");
                        if let Some(ref net) = network {
                            let request = SyncRequest {
                                start_height: height,
                                count: crate::network_handler::SYNC_REQUEST_BATCH,
                                full_state: false,
                            };
                            if let Ok(req_data) = bincode::serialize(&NetworkMessage::SyncRequest(request)) {
                                let net_clone = Arc::clone(net);
                                tokio::spawn(async move {
                                    net_clone.broadcast(SYNC_CHANNEL, req_data).await;
                                });
                            }
                        }
                    }
                }

                if let Some(block) = block {
                    let height = block.header.height;

                    // Height replay protection
                    let current_height = {
                        let c = consensus.read().unwrap();
                        c.current_height()
                    };
                    if height < current_height {
                        tracing::debug!(height, current = current_height, "BFT finalize: already finalized");
                        continue;
                    }

                    // Execute block on shared state — this is the ONLY place state is committed.
                    let result = match state.write_all().execute_block(&block, height) {
                        Ok(r) => r,
                        Err(e) => {
                            tracing::warn!(error = ?e, height, "BFT finalize: execution failed");
                            continue;
                        }
                    };

                    // State root check: computed root must match header root
                    if result.state_root != block.header.state_root {
                        tracing::error!(height, "BFT finalize: state root mismatch — block rejected");
                        continue;
                    }

                    // Remove confirmed transactions from mempool so they are not
                    // re-selected in the next block. Only confirm executed txs.
                    {
                        let evm_hashes: Vec<TxHash> = result.evm_tx_results.iter()
                            .map(|r| r.tx_hash)
                            .collect();
                        let mut mp = mempool.write().unwrap();
                        mp.confirm_transactions(&evm_hashes);
                        // Also notify mempool defense so per-address tx_counts are decremented
                        drop(mp);
                        let mut defense = state.mempool_defense.write().unwrap();
                        for evm in &result.evm_tx_results {
                            defense.on_tx_confirmed(evm.caller);
                        }
                    }

                    // Handle oracle period transitions at boundary heights
                    // (non-proposing validators advance period but don't broadcast requests — proposer already did)
                    let is_oracle_boundary = height.is_multiple_of(ORACLE_UPDATE_INTERVAL);
                    if is_oracle_boundary {
                        let mut oracle_guard = oracle.write().unwrap();
                        oracle_guard.advance_period(height);
                        let outliers: Vec<u32> = oracle_guard.last_outliers().to_vec();
                        drop(oracle_guard);
                        if !outliers.is_empty() {
                            let mut evm_state = state.evm_state.write().unwrap();
                            let mut c = consensus.write().unwrap();
                            for vid in &outliers {
                                if let Err(e) = c.handle_oracle_outlier(&mut evm_state, *vid) {
                                    tracing::warn!(validator_id = vid, error = ?e, "failed to slash oracle outlier");
                                }
                            }
                            tracing::info!(outliers = ?outliers, "slashed oracle outliers");
                        }
                        let contributions = {
                            oracle.write().unwrap().distribute_rewards()
                        };
                        if !contributions.is_empty() {
                            let mut evm_state = state.evm_state.write().unwrap();
                            let mut c = consensus.write().unwrap();
                            for (vid, amount) in &contributions {
                                if let Err(e) = c.distribute_oracle_reward(&mut evm_state, *vid, *amount) {
                                    tracing::warn!(validator_id = vid, amount, error = ?e, "failed to distribute oracle reward");
                                }
                            }
                            tracing::info!(count = contributions.len(), "distributed oracle rewards");
                        }
                        oracle.write().unwrap().clear_tracking();
                    }

                    // Commit via consensus
                    {
                        let mut evm_state = state.evm_state.write().unwrap();
                        let mut c = consensus.write().unwrap();
                        if let Err(e) = c.commit_block(&block, &result, &mut evm_state) {
                            tracing::warn!(error = ?e, height, "BFT finalize: commit failed");
                            continue;
                        }
                    }
                    telemetry.record_block_committed();

                    // Advance state
                    let new_height = height + 1;
                    state.set_current_block(new_height);
                    parent_hash = block.header.hash();
                    state.finalize_block();

                    // Push fee history entry
                    {
                        let fee_params = state.fee_params.read().unwrap();
                        let base_fee = fee_params.base_fee;
                        let max_gas = fee_params.max_gas_per_block.max(1);
                        drop(fee_params);
                        let total_gas = result.evm_gas_used;
                        let gas_used_ratio = (total_gas as f64 / max_gas as f64).min(1.0);
                        let mut evm_priority_fees: Vec<u128> = result.evm_tx_results.iter()
                            .map(|e| e.gas_price.saturating_sub(base_fee))
                            .collect();
                        evm_priority_fees.sort_unstable();
                        let n = evm_priority_fees.len().max(1);
                        let priority_fee_rewards: Vec<u128> = [0.0, 10.0, 50.0, 90.0, 100.0]
                            .iter()
                            .map(|p| {
                                let p = (*p as f64).min(100.0).max(0.0);
                                let idx = ((n - 1) as f64 * p / 100.0).round() as usize;
                                evm_priority_fees.get(idx.min(n - 1)).copied().unwrap_or(call_protocol::gas::MIN_PRIORITY_FEE_PER_GAS)
                            })
                            .collect();
                        let entry = call_rpc::handlers::BlockFeeEntry {
                            base_fee,
                            gas_used_ratio,
                            priority_fee_rewards,
                        };
                        if let Ok(mut history) = state.fee_history.write() {
                            history.push_back((height, entry));
                            while history.len() > 1024 {
                                history.pop_front();
                            }
                        }
                    }

                    // Advance governance
                    {
                        let mut gov = governance.write().unwrap();
                        gov.set_current_block(new_height);
                        gov.advance(new_height);
                        for event in gov.drain_events() {
                            let (event_str, proposal_id) = match &event {
                                call_governance::GovernanceEvent::ProposalAdvanced { id, from, to } => {
                                    (format!("{:?} → {:?}", from, to), *id)
                                }
                                call_governance::GovernanceEvent::ProposalExecuted { id, proposal_type } => {
                                    (format!("executed: {}", proposal_type), *id)
                                }
                                call_governance::GovernanceEvent::ProposalExpired { id } => {
                                    ("expired".to_string(), *id)
                                }
                                call_governance::GovernanceEvent::ProposalDefeated { id } => {
                                    ("defeated".to_string(), *id)
                                }
                            };
                            subscriptions.broadcast_governance(event_str, proposal_id, String::new());
                        }
                    }

                    // Sync validators from EVM storage into governance
                    {
                        let mut gov = governance.write().unwrap();
                        let evm_state = state.evm_state.read().unwrap();
                        let validators = call_consensus::exec::state_accessors::read_validators(&evm_state);
                        drop(evm_state);
                        for (id, addr, _stake) in validators {
                            gov.register_validator(id as u32, addr);
                        }
                    }

                    // Generate and store receipts for EVM transactions
                    let block_hash = block.header.hash();
                    let mut cumulative_gas: u64 = 0;
                    let mut tx_index: u64 = 0;

                    for evm in &result.evm_tx_results {
                        let fee_amount = evm.gas_used as u128 * evm.gas_price;
                        let status = if evm.status {
                            call_primitives::ExecutionStatus::Success
                        } else {
                            call_primitives::ExecutionStatus::Reverted {
                                reason: "evm execution failed".into(),
                            }
                        };
                        cumulative_gas += evm.gas_used;
                        let receipt = ProtocolReceipt {
                            tx_hash: evm.tx_hash,
                            status,
                            gas_used: evm.gas_used,
                            gas_payer: evm.caller,
                            fee_currency: FeeCurrency::Call,
                            fee_amount,
                            block_number: height,
                            block_hash,
                            transaction_index: tx_index,
                            to: evm.to,
                            contract_address: evm.contract_address,
                            cumulative_gas_used: cumulative_gas,
                            effective_gas_price: evm.gas_price,
                            logs_bloom: vec![],
                            instruction_results: vec![],
                            logs: evm.logs.clone(),
                            memos: vec![],
                            state_changes: vec![],
                        };
                        state.store_receipt(evm.tx_hash, receipt);
                        tx_index += 1;
                    }

                    // Broadcast ETH WebSocket events (newHeads + logs)
                    let gas_used: u64 = result.evm_tx_results.iter().map(|e| e.gas_used).sum::<u64>();
                    let base_fee = state.fee_params.read().map(|p| p.base_fee).unwrap_or(0);
                    subscriptions.broadcast_eth_new_head(serde_json::json!({
                        "hash": format!("0x{}", hex::encode(block_hash.as_slice())),
                        "parentHash": format!("0x{}", hex::encode(block.header.parent_hash.as_slice())),
                        "number": format!("0x{:x}", height),
                        "timestamp": format!("0x{:x}", block.header.timestamp_millis / 1000),
                        "gasLimit": "0x1c9c380",
                        "gasUsed": format!("0x{:x}", gas_used),
                        "miner": format!("0x{}", hex::encode([0u8; 20])),
                        "difficulty": "0x0",
                        "totalDifficulty": "0x0",
                        "nonce": "0x0000000000000000",
                        "sha3Uncles": format!("0x{}", hex::encode([0u8; 32])),
                        "receiptsRoot": format!("0x{}", hex::encode(block.header.state_root.as_slice())),
                        "transactionsRoot": format!("0x{}", hex::encode(block.header.state_root.as_slice())),
                        "stateRoot": format!("0x{}", hex::encode(block.header.state_root.as_slice())),
                        "size": format!("0x{:x}", serde_json::to_vec(&block).map(|v| v.len()).unwrap_or(0)),
                        "extraData": "0x",
                        "mixHash": format!("0x{}", hex::encode([0u8; 32])),
                        "baseFeePerGas": format!("0x{:x}", base_fee),
                    }));

                    for (idx, evm) in result.evm_tx_results.iter().enumerate() {
                        for (log_idx, log) in evm.logs.iter().enumerate() {
                            subscriptions.broadcast_eth_log(serde_json::json!({
                                "address": format!("{:?}", log.address),
                                "topics": log.topics.iter().map(|t| format!("0x{}", hex::encode(t.as_slice()))).collect::<Vec<_>>(),
                                "data": format!("0x{}", hex::encode(&log.data)),
                                "blockNumber": format!("0x{:x}", height),
                                "blockHash": format!("0x{}", hex::encode(block_hash.as_slice())),
                                "transactionHash": format!("0x{}", hex::encode(evm.tx_hash.as_slice())),
                                "transactionIndex": format!("0x{:x}", idx),
                                "logIndex": format!("0x{:x}", log_idx),
                                "removed": false,
                            }));
                        }
                    }

                    // Persist block to disk
                    if let Err(e) = persist_block(&data_dir, height, &block) {
                        tracing::warn!(error = %e, height, "BFT finalize: persist block failed");
                    }

                    // Update prune tracking
                    prune_state.add_block_body(height, call_storage::BlockBody {
                        block_hash: parent_hash,
                        tx_count: result.total_tx_count() as u32,
                        body_size: 0,
                    });

                    if let Err(e) = call_storage::maybe_prune(
                        &mut prune_state, new_height, &prune_config, Some(&db.db)
                    ) {
                        tracing::warn!(error = %e, "BFT finalize: prune check failed");
                    }
                    telemetry.record_storage_prune(
                        prune_state.traces_pruned,
                        prune_state.receipts_pruned,
                        prune_state.bodies_pruned,
                        prune_state.snapshots_pruned,
                    );

                    // Produce state snapshot at snapshot interval boundaries
                    if new_height % prune_config.snapshot_interval == 0 {
                        let evm_root = {
                            let evm_state = state.evm_state.read().unwrap();
                            let root = evm_state.compute_state_root();
                            Hash::from(root.0)
                        };

                        let shielded_root = {
                            let evm_state = state.evm_state.read().unwrap();
                            call_consensus::exec::state_accessors::read_shielded_merkle_root(&evm_state)
                        };

                        let agent_root = {
                            let evm_state = state.evm_state.read().unwrap();
                            let count = call_consensus::exec::state_accessors::read_agent_count(&evm_state);
                            let mut agents = std::collections::HashMap::new();
                            for id in 0..count {
                                let owner = call_consensus::exec::state_accessors::agent_get_owner(&evm_state, id);
                                if owner == call_primitives::Address::ZERO {
                                    continue;
                                }
                                let name = call_consensus::exec::state_accessors::agent_get_name(&evm_state, id);
                                let registered_at = call_consensus::exec::state_accessors::agent_get_registered_at(&evm_state, id);
                                agents.insert(id, (owner, name, registered_at));
                            }
                            call_storage::compute_agent_root(&agents)
                        };

                        let roots = StateRoots { protocol_root: evm_root, evm_root, shielded_root, agent_root, consensus_root: evm_root };
                        let snapshot_dir = db.data_dir.join("snapshots");
                        match produce_state_snapshot(&mut prune_state, roots, new_height, Some(&snapshot_dir)) {
                            Ok(_) => {
                                tracing::info!(height = new_height, "BFT finalize: state snapshot produced");
                            }
                            Err(ref e) => {
                                tracing::warn!(error = %e, "BFT finalize: snapshot production failed");
                            }
                        }
                    }

                    // Incremental state persistence
                    let db_env = &db.db;
                    if let Err(e) = persist_state_incremental(db_env, &state, &consensus, &oracle, &governance) {
                        tracing::warn!(error = %e, "BFT finalize: incremental persist failed");
                    }

                    // Full rebuild every 1000 blocks
                    if new_height % 1000 == 0 {
                        let db_env = &db.db;
                        if let Err(e) = persist_state_to_db(db_env, &state, &consensus, &oracle, &governance) {
                            tracing::warn!(error = %e, "BFT finalize: full persist failed");
                        }
                    }

                    tracing::info!(height = new_height, tx_count = result.total_tx_count(), "BFT finalized block");

                    // Broadcast to WebSocket subscribers
                    let tx_count = result.total_tx_count();
                    subscriptions.broadcast_block(height, format!("{:?}", block.header.hash()), block.header.proposer, tx_count);

                    // Broadcast block announcement via P2P
                    if let Some(ref net) = network {
                        let announcement = BlockAnnouncement {
                            block_hash: parent_hash,
                            height,
                            proposer: block.header.proposer,
                            timestamp_millis: block.header.timestamp_millis,
                        };
                        let msg = bincode::serialize(&NetworkMessage::BlockAnnouncement(announcement))
                            .expect("serialize block announcement");
                        let net_clone = Arc::clone(net);
                        tokio::spawn(async move {
                            net_clone.broadcast(BLOCK_CHANNEL, msg).await;
                        });
                    }
                    // Check for epoch boundary
                    let epoch_length = state.consensus_params.read().unwrap().epoch_length;
                    let new_height = height + 1;
                    if new_height % epoch_length == 0 && !awaiting_quorum {
                        tracing::info!(
                            epoch = epoch_number + 1,
                            height = new_height,
                            threshold = quorum_threshold,
                            "BFT: epoch boundary reached, entering quorum wait"
                        );

                        // Enter quorum waiting state
                        awaiting_quorum = true;
                        boundary_height = new_height;
                        quorum_wait_start = Some(std::time::Instant::now());

                        // Record self in peer_heights
                        {
                            let mut peer_heights = state.peer_heights.write().unwrap();
                            peer_heights.insert(hex::encode(&my_pubkey), new_height);
                        }

                        // Broadcast EpochBoundarySignal to subset peers
                        if let Some(ref net) = network {
                            let signal = EpochBoundarySignal {
                                height: new_height,
                                epoch: epoch_number,
                                sender_pubkey: my_pubkey,
                            };
                            let msg = bincode::serialize(&NetworkMessage::EpochBoundarySignal(signal))
                                .expect("serialize epoch boundary signal");
                            let net_clone = Arc::clone(net);
                            tokio::spawn(async move {
                                net_clone.broadcast(BLOCK_CHANNEL, msg).await;
                            });
                        }

                        // Do NOT break — continue event loop, wait for quorum
                    }

                }
            }

            Some(block_bytes) = broadcast_rx.recv() => {
                // Relay wants us to broadcast a block via the app P2P network
                tracing::info!(bytes = block_bytes.len(), "BFT: broadcasting block via P2P");
                if let Some(ref net) = network {
                    let net_clone = Arc::clone(net);
                    tokio::spawn(async move {
                        net_clone.broadcast(BLOCK_CHANNEL, block_bytes).await;
                    });
                }
            }

            else => {
                tracing::info!("BFT event loop: all channels closed, shutting down");
                let _ = exit_tx.send(EpochRotationReason::EpochBoundary);
                break;
            }
        }
    }
}
