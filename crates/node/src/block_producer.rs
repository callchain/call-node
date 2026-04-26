//! Block production loop — solo-mode block building, execution, and broadcast.

use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use call_bridge::BridgeConfig;
use call_consensus::{Block, SimplexConsensus, SystemTx, SystemTxKind};
use call_primitives::BlockHash;
use call_network::{Network, NetworkMessage, BlockAnnouncement, OraclePriceRequest, UpgradeAnnouncement};
use call_oracle::ORACLE_UPDATE_INTERVAL;
use call_primitives::Address;
use call_protocol::transaction::ProtocolTransaction;
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
    audit_log: Arc<RwLock<crate::logging::AuditLog>>,
) {
    let mut parent_hash = initial_parent_hash;
    let prune_config = call_storage::PruneConfig::default();

    let mut interval = tokio::time::interval(Duration::from_millis(
        state.consensus_params.read().ok().map(|p| p.block_time_millis).unwrap_or(250),
    ));

    loop {
        interval.tick().await;

        // 1. Select transactions from mempool
        let selection = { mempool.write().unwrap().select_transactions() };
        telemetry.set_mempool_size(selection.protocol_txs.len() + selection.evm_txs.len());
        telemetry.set_bridge_pending(selection.bridge_ops.len());

        // 2. Build block
        let (proposer, height) = {
            let c = consensus.read().unwrap();
            (c.current_proposer(), c.current_height())
        };
        let Some(proposer) = proposer else {
            continue; // not our turn, skip this round
        };

        let block_start = Instant::now();

        // Deserialize protocol txs from mempool data
        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

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
            protocol_txs,
            evm_txs,
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
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

        // 3c. Finalize bridge deposits whose challenge period has expired
        {
            let mut balances = state.balance_state.write().unwrap();
            let mut bridge_state = state.bridge_state.write().unwrap();
            let config = BridgeConfig::default();
            let finalized = bridge_state.finalize_pending_external_deposits(
                height,
                config.challenge_period_blocks,
            );
            for deposit in finalized {
                if let Err(e) = balances.mint(deposit.asset_id, &call_primitives::Address::ZERO, deposit.recipient, deposit.amount) {
                    tracing::warn!(error = %e, "bridge: failed to credit finalized deposit");
                } else {
                    tracing::info!(
                        tx_hash = %deposit.source_tx_hash,
                        recipient = %deposit.recipient,
                        amount = deposit.amount,
                        "bridge: deposit finalized and credited"
                    );
                }
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
                    let oracle = state.oracle.read().unwrap();
                    oracle.tracked_pairs.clone()
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
            let mut oracle = state.oracle.write().unwrap();
            oracle.advance_period(height);
        }

        // 6. Slash oracle outliers before clearing tracking
        {
            let oracle = state.oracle.read().unwrap();
            let outliers: Vec<u32> = oracle.last_outliers().to_vec();
            if !outliers.is_empty() {
                let mut c = consensus.write().unwrap();
                for vid in &outliers {
                    if let Err(e) = c.handle_oracle_outlier(*vid) {
                        tracing::warn!(validator_id = vid, error = ?e, "failed to slash oracle outlier");
                    }
                }
                tracing::info!(outliers = ?outliers, "slashed oracle outliers");
            }
            drop(oracle);

            // Distribute oracle rewards to contributors before clearing tracking
            let contributions = {
                let mut oracle = state.oracle.write().unwrap();
                oracle.distribute_rewards()
            };
            if !contributions.is_empty() {
                let mut c = consensus.write().unwrap();
                for (vid, amount) in &contributions {
                    if let Err(e) = c.distribute_oracle_reward(*vid, *amount) {
                        tracing::warn!(validator_id = vid, amount, error = ?e, "failed to distribute oracle reward");
                    }
                }
                tracing::info!(count = contributions.len(), "distributed oracle rewards");
            }

            // Clear tracking after slashing and reward distribution
            let mut oracle = state.oracle.write().unwrap();
            oracle.clear_tracking();
        }

        // 7. Commit via consensus (BFT engine handles proposal/verification)
        {
            let mut c = consensus.write().unwrap();
            if let Err(e) = c.commit_block(&block, &result) {
                tracing::warn!(error = ?e, "commit failed");
                continue;
            }
        }
        telemetry.record_block_produced();
        telemetry.record_block_committed();
        let block_duration = block_start.elapsed().as_millis() as u64;
        telemetry.record_block_latency(block_duration);

        // Append audit entries for each protocol transaction
        {
            let mut audit = audit_log.write().unwrap();
            for (tx_idx, tx) in block.protocol_txs.iter().enumerate() {
                let tx_hash = {
                    let data = serde_json::to_vec(tx).unwrap_or_default();
                    call_crypto::keccak256(&data)
                };
                for instr in &tx.instructions {
                    let entry = crate::logging::AuditEntry {
                        block_height: block.header.height,
                        tx_index: tx_idx as u32,
                        tx_type: format!("{:?}", std::mem::discriminant(instr)),
                        action: format!("{:?}", instr),
                        agent_id: None,
                        fee_payer: Some(tx.sender),
                        before_state: serde_json::Value::Null,
                        after_state: serde_json::Value::Null,
                        tx_hash,
                        shielded_details: None,
                    };
                    if let Err(e) = audit.append(entry) {
                        tracing::warn!(error = %e, "audit log append failed");
                    }
                }
            }
        }

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

        // 10b. Advance governance proposal state machine
        {
            let mut gov = state.governance.write().unwrap();
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

        // 10c. Sync validators from consensus into governance
        {
            let mut gov = state.governance.write().unwrap();
            let vs = state.validator_state.read().unwrap();
            for (id, stake) in vs.get_all_validators().iter() {
                gov.register_validator(*id, stake.address);
            }
        }

        // 11. Persist block to disk
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
            let balance_state = state.balance_state.read().unwrap();
            let protocol_root = call_storage::compute_protocol_root(
                balance_state.balances.balances_map(),
                balance_state.allowances.allowances_map(),
            );
            drop(balance_state);

            let evm_root = {
                let evm_state = state.evm_state.read().unwrap();
                let root = evm_state.compute_state_root();
                call_primitives::Hash::from(root.0)
            };

            let shielded_root = {
                let shielded_state = state.shielded_state.read().unwrap();
                shielded_state.merkle_root()
            };

            let agent_root = {
                let registry = state.agent_registry.read().unwrap();
                let agents: std::collections::HashMap<u64, (Address, String, u64)> = registry
                    .agents
                    .iter()
                    .map(|(id, reg)| (*id, (reg.owner, reg.name.clone(), reg.registered_at)))
                    .collect();
                call_storage::compute_agent_root(&agents)
            };

            let consensus_root = {
                let validator_state = state.validator_state.read().unwrap();
                let validators: std::collections::HashMap<u32, (Address, u128)> = validator_state
                    .get_all_validators()
                    .iter()
                    .map(|(id, stake)| (*id, (stake.address, stake.staked_call)))
                    .collect();
                call_storage::compute_consensus_root(&validators)
            };

            let roots = StateRoots { protocol_root, evm_root, shielded_root, agent_root, consensus_root };
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
        if let Err(ref e) = persist_state_incremental(db_env, &state, &consensus) {
            tracing::warn!(error = %e, "failed to incrementally persist state");
        }

        // 15. Full table rebuild every 1000 blocks as safety net
        if new_height % 1000 == 0 {
            let db_env = &db.db;
            if let Err(ref e) = persist_state_to_db(db_env, &state, &consensus) {
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
