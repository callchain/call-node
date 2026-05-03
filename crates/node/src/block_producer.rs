//! Block production loop — solo-mode block building, execution, and broadcast.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use call_bridge::BridgeConfig;
use call_consensus::{Block, SimplexConsensus};
use call_governance::GovernanceManager;
use call_primitives::BlockHash;
use call_network::{Network, NetworkMessage, BlockAnnouncement, OraclePriceRequest, UpgradeAnnouncement};
use call_oracle::{OracleManager, ORACLE_UPDATE_INTERVAL};
use call_primitives::{FeeCurrency, TxHash};
use call_protocol::ProtocolReceipt;
use call_rpc::{RpcState, SubscriptionManager};
use call_storage::{CallDb, PruneState, StateRoots, produce_state_snapshot};
use call_transaction_pool::Mempool;
use crate::{persist_block, persist_state_incremental, persist_state_to_db};
use crate::network_handler::{BLOCK_CHANNEL, ORACLE_CHANNEL, UPGRADE_CHANNEL};

pub(crate) async fn block_production_loop(
    state: Arc<RpcState>,
    mempool: Arc<RwLock<Mempool>>,
    consensus: Arc<RwLock<SimplexConsensus>>,
    network: Option<Arc<dyn Network>>,
    initial_parent_hash: BlockHash,
    db: CallDb,
    mut prune_state: PruneState,
    subscriptions: SubscriptionManager,
    telemetry: Arc<crate::telemetry::TelemetryRegistry>,
    _audit_log: Arc<RwLock<crate::logging::AuditLog>>,
    oracle: Arc<RwLock<OracleManager>>,
    governance: Arc<RwLock<GovernanceManager>>,
) {
    let mut parent_hash = initial_parent_hash;
    let prune_config = call_storage::PruneConfig::default();

    let mut interval = tokio::time::interval(Duration::from_millis(
        state.consensus_params.read().ok().map(|p| p.block_time_millis).unwrap_or(250),
    ));

    loop {
        interval.tick().await;

        // 1. Select transactions from mempool (EVM-only)
        let selection = { mempool.write().unwrap().select_transactions() };
        telemetry.set_mempool_size(selection.evm_txs.len());

        // 2. Build block
        let (proposer, height) = {
            let c = consensus.read().unwrap();
            (c.current_proposer(), c.current_height())
        };
        let Some(proposer) = proposer else {
            continue; // not our turn, skip this round
        };

        let block_start = Instant::now();

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        // Get current protocol version from ForkManager
        let version = {
            let fm = state.fork_manager.read().unwrap();
            fm.current_version()
        };

        let mut block = Block::new(
            height,
            parent_hash,
            crate::current_timestamp_millis(),
            proposer,
            version,
            evm_txs,
        );

        // 3. Execute block
        let exec_start = Instant::now();
        let result = state.write_all().execute_block(&block, height);
        let exec_duration = exec_start.elapsed().as_millis() as u64;
        telemetry.record_tx_latency(exec_duration);
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = ?e, "block execution failed");
                continue;
            }
        };
        block.finalize(&result);

        // Remove confirmed transactions from mempool so they are not re-selected
        // in the next block.  Only confirm txs that were actually executed
        // (skipped txs remain in the mempool for the next block).
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

        // 3c. Finalize bridge deposits whose challenge period has expired
        {
            let mut evm_state = state.evm_state.write().unwrap();
            let config = BridgeConfig::default();
            let finalized = call_consensus::exec::state_accessors::finalize_pending_external_deposits_evm(
                &mut evm_state,
                height,
                config.challenge_period_blocks,
            );
            if finalized > 0 {
                tracing::info!(
                    count = finalized,
                    "bridge: deposits finalized and credited"
                );
            }
        }

        // 3b. Sign the block (if validator with signing key)
        {
            let signer_guard = state.signer.read().unwrap();
            if let Some(ref signer) = *signer_guard {
                let block_hash = block.header.hash();
                match signer.sign(&block_hash) {
                    Ok(sig) => {
                        block.header.signature = call_consensus::BlockSignature(sig);
                        tracing::debug!(height, proposer, "block signed");
                    }
                    Err(e) => {
                        tracing::warn!(height, error = ?e, "block signing failed");
                    }
                }
            }
        }

        // 4. Request oracle price submissions from validators before advancing
        let is_oracle_boundary = height.is_multiple_of(ORACLE_UPDATE_INTERVAL);
        if is_oracle_boundary {
            if let Some(ref net) = network {
                let tracked = {
                    let oracle_guard = oracle.read().unwrap();
                    oracle_guard.tracked_pairs.clone()
                };
                if !tracked.is_empty() {
                    let proposer_id = proposer;
                    let request = OraclePriceRequest {
                        pairs: tracked,
                        block: height,
                        requester_id: proposer_id,
                    };
                    let msg = bincode::serialize(&NetworkMessage::OraclePriceRequest(request))
                        .expect("serialize oracle request");
                    net.broadcast(ORACLE_CHANNEL, msg).await;
                    // Configurable delay to allow validators to respond
                    let delay_ms = state.consensus_params.read()
                        .ok()
                        .map(|p| p.oracle_request_delay_ms)
                        .unwrap_or(200);
                    tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                }
            }
        }

        // 5. Advance oracle period at interval boundaries
        if is_oracle_boundary {
            let mut oracle_guard = oracle.write().unwrap();
            oracle_guard.advance_period(height);
        }

        // 6. Slash oracle outliers before clearing tracking
        {
            let oracle_guard = oracle.read().unwrap();
            let outliers: Vec<u32> = oracle_guard.last_outliers().to_vec();
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
            drop(oracle_guard);

            // Distribute oracle rewards to contributors before clearing tracking
            let contributions = {
                let mut oracle_guard = oracle.write().unwrap();
                oracle_guard.distribute_rewards()
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

            // Clear tracking after slashing and reward distribution
            let mut oracle_guard = oracle.write().unwrap();
            oracle_guard.clear_tracking();
        }

        // 7. Commit via consensus (BFT engine handles proposal/verification)
        {
            let mut evm_state = state.evm_state.write().unwrap();
            let mut c = consensus.write().unwrap();
            if let Err(e) = c.commit_block(&block, &result, &mut evm_state) {
                tracing::warn!(error = ?e, "commit failed");
                continue;
            }
        }
        telemetry.record_block_produced();
        telemetry.record_block_committed();
        let block_duration = block_start.elapsed().as_millis() as u64;
        telemetry.record_block_latency(block_duration);

        // 7a. Check and apply any scheduled protocol upgrades at this height
        {
            let mut fm = state.fork_manager.write().unwrap();
            if let Some(new_version) = fm.check_upgrades_at_height(height) {
                tracing::info!(height, ?new_version, "protocol upgrade activated");
            }
        }

        // 10. Update state
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

        // 10b. Advance governance proposal state machine
        {
            let mut gov = governance.write().unwrap();
            gov.set_current_block(new_height);
            gov.advance(new_height);
            // Drain and broadcast governance events
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

        // 10c. Sync validators from EVM storage into governance
        {
            let mut gov = governance.write().unwrap();
            let evm_state = state.evm_state.read().unwrap();
            let validators = call_consensus::exec::state_accessors::read_validators(&evm_state);
            drop(evm_state);
            for (id, addr, _stake) in validators {
                gov.register_validator(id as u32, addr);
            }
        }

        // 11. Generate and store receipts for EVM transactions
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

        // 11b. Broadcast ETH WebSocket events (newHeads + logs)
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

        // Broadcast logs for eth_subscribe("logs")
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

        // 12. Persist block to disk
        if let Err(ref e) = persist_block(&db.data_dir, height, &block) {
            tracing::warn!(error = %e, "failed to persist block");
        }

        // 12. Update prune tracking state
        prune_state.add_block_body(height, call_storage::BlockBody {
            block_hash: parent_hash,
            tx_count: result.total_tx_count() as u32,
            body_size: 0, // would be actual serialized size in production
        });

        // 13. Run periodic prune checks
        if let Err(ref e) = call_storage::maybe_prune(&mut prune_state, new_height, &prune_config, Some(&db.db)) {
            tracing::warn!(error = %e, "prune check failed");
        }
        telemetry.record_storage_prune(
            prune_state.traces_pruned,
            prune_state.receipts_pruned,
            prune_state.bodies_pruned,
            prune_state.snapshots_pruned,
        );

        // 13a. Produce state snapshot at snapshot interval boundaries
        if new_height % prune_config.snapshot_interval == 0 {
            let evm_root = {
                let evm_state = state.evm_state.read().unwrap();
                let root = evm_state.compute_state_root();
                call_primitives::Hash::from(root.0)
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
                    tracing::info!(height = new_height, "state snapshot produced");
                }
                Err(ref e) => {
                    tracing::warn!(error = %e, "snapshot production failed");
                }
            }
        }

        // 14. Incrementally persist state changes after every block
        let db_env = &db.db;
        if let Err(ref e) = persist_state_incremental(db_env, &state, &consensus, &oracle, &governance) {
            tracing::warn!(error = %e, "failed to incrementally persist state");
        }

        // 15. Full table rebuild every 1000 blocks as safety net
        if new_height % 1000 == 0 {
            let db_env = &db.db;
            if let Err(ref e) = persist_state_to_db(db_env, &state, &consensus, &oracle, &governance) {
                tracing::warn!(error = %e, "failed to full-rebuild persist state");
            }
        }

        // 16. Broadcast to WebSocket subscribers

        tracing::info!(height = new_height, tx_count = result.total_tx_count(), "committed block");

        // Broadcast to WebSocket subscribers
        let tx_count = result.total_tx_count();
        subscriptions.broadcast_block(height, format!("{:?}", block.header.hash()), proposer, tx_count);

        // 17. Broadcast block announcement via P2P (post-commit)
        if let Some(ref net) = network {
            let announcement = BlockAnnouncement {
                block_hash: parent_hash,
                height,
                proposer,
                timestamp_millis: block.header.timestamp_millis,
            };
            let msg = bincode::serialize(&NetworkMessage::BlockAnnouncement(announcement))
                .expect("serialize block announcement");
            let broadcast_start = Instant::now();
            net.broadcast(BLOCK_CHANNEL, msg.clone()).await;
            telemetry.record_p2p_latency(broadcast_start.elapsed().as_millis() as u64);
            telemetry.record_p2p_bytes_sent(msg.len());

            // 17a. Gossip scheduled upgrade announcement if one exists
            let upgrade = {
                let fm = state.fork_manager.read().unwrap();
                fm.next_upgrade(height).map(|entry| UpgradeAnnouncement {
                    version: entry.version,
                    activation_height: entry.activation_height,
                    proposal_id: entry.proposal_id,
                })
            };
            if let Some(upgrade) = upgrade {
                tracing::debug!(activation_height = upgrade.activation_height, ?upgrade.version, "broadcast upgrade announcement");
                let msg = bincode::serialize(&NetworkMessage::UpgradeAnnouncement(upgrade))
                    .expect("serialize upgrade announcement");
                let broadcast_start = Instant::now();
                net.broadcast(UPGRADE_CHANNEL, msg.clone()).await;
                telemetry.record_p2p_latency(broadcast_start.elapsed().as_millis() as u64);
                telemetry.record_p2p_bytes_sent(msg.len());
            }
        }
    }
}
