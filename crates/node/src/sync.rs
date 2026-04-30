//! Sync protocol — handle incoming sync requests and apply synced blocks.

use std::sync::{Arc, RwLock};
use std::path::Path;

use call_consensus::{Block, SimplexConsensus};
use call_primitives::BlockHash;
use call_network::SyncResponse;
use call_primitives::FeeCurrency;
use call_protocol::ProtocolReceipt;
use call_rpc::RpcState;
use crate::persist_block;

pub(crate) fn handle_sync_request(data_dir: &Path, request: &call_network::SyncRequest) -> Option<SyncResponse> {
    let mut blocks = Vec::new();
    let end = request.start_height.saturating_add(request.count);
    for h in request.start_height..end {
        if let Some(block) = crate::load_block(data_dir, h) {
            // NOTE: Blocks are serialized as JSON for the SyncResponse payload
            // because the receiving side (`apply_synced_blocks` and the legacy
            // `start_sync` path) deserializes them with `serde_json::from_slice`.
            // Using bincode here would silently produce undeliverable responses.
            if let Ok(serialized) = serde_json::to_vec(&block) {
                blocks.push(serialized);
            }
        } else {
            break; // no more blocks available
        }
    }
    if blocks.is_empty() {
        return None;
    }
    let state_root = blocks.last().and_then(|b| {
        let block: Block = serde_json::from_slice(b).ok()?;
        Some(block.header.state_root)
    }).unwrap_or(BlockHash::ZERO);
    Some(SyncResponse {
        start_height: request.start_height,
        blocks,
        state_root,
    })
}

/// Apply blocks received in a `SyncResponse` to local state.
///
/// Used by full / archive nodes (and any validator catching up) to import
/// finalized blocks broadcast by validators. Returns the number of blocks
/// successfully applied.
///
/// Only blocks at `local_height` or above are applied; out-of-order /
/// already-known heights are skipped without error. Blocks are persisted to
/// disk and committed to the in-memory `SimplexConsensus` so that subsequent
/// `BlockAnnouncement`s correctly compare heights.
pub(crate) fn apply_synced_blocks(
    response: &SyncResponse,
    state: &Arc<RpcState>,
    consensus: &Arc<RwLock<SimplexConsensus>>,
    data_dir: &Path,
) -> usize {
    let mut applied = 0usize;
    let initial_height = state.get_current_block();

    // Initialise sync progress if not already tracking
    {
        let highest_block = {
            let heights = state.peer_heights.read().unwrap();
            heights.values().copied().max().unwrap_or(initial_height)
        };
        let mut sp = state.sync_progress.write().unwrap();
        if sp.is_none() {
            *sp = Some(call_rpc::handlers::SyncProgress {
                starting_block: initial_height,
                current_block: initial_height,
                highest_block,
            });
        }
    }

    for (i, block_data) in response.blocks.iter().enumerate() {
        let block_height = response.start_height.saturating_add(i as u64);
        let local_height = state.get_current_block();
        if block_height < local_height {
            continue; // already have this block
        }
        if block_height > local_height {
            // We can't apply blocks out of order — wait for an earlier batch.
            break;
        }
        let mut block: Block = match serde_json::from_slice(block_data) {
            Ok(b) => b,
            Err(e) => {
                tracing::warn!(height = block_height, error = %e, "sync: failed to deserialize block");
                continue;
            }
        };

        // State root verification on cloned state before applying to shared state
        let roots_valid = match state.read_all().execute_block_cloned(&block, block_height) {
            Ok(result) => {
                let state_ok = result.state_root == block.header.state_root;
                if !state_ok {
                    tracing::error!(
                        height = block_height,
                        state_ok,
                        result_state = %result.state_root,
                        header_state = %block.header.state_root,
                        "sync: state root mismatch — rejecting synced block"
                    );
                }
                state_ok
            }
            Err(_) => false,
        };

        if !roots_valid {
            break;
        }

        // Apply to shared state
        let execute_result = state.write_all().execute_block(&block, block_height);

        match execute_result {
            Ok(result) => {
                block.finalize(&result);
                if let Err(e) = persist_block(data_dir, block_height, &block) {
                    tracing::warn!(height = block_height, error = %e, "sync: failed to persist block to disk");
                }

                // Push fee history entry so RPC nodes have data even when syncing
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
                            evm_priority_fees.get(idx.min(n - 1)).copied().unwrap_or(call_protocol::transaction::MIN_PRIORITY_FEE_PER_GAS)
                        })
                        .collect();
                    let entry = call_rpc::handlers::BlockFeeEntry {
                        base_fee,
                        gas_used_ratio,
                        priority_fee_rewards,
                    };
                    if let Ok(mut history) = state.fee_history.write() {
                        history.push_back((block_height, entry));
                        while history.len() > 1024 {
                            history.pop_front();
                        }
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
                        block_number: block_height,
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

                // Broadcast ETH WebSocket events
                let gas_used: u64 = result.evm_tx_results.iter().map(|e| e.gas_used).sum::<u64>();
                let base_fee = state.fee_params.read().map(|p| p.base_fee).unwrap_or(0);
                state.subscriptions.broadcast_eth_new_head(serde_json::json!({
                    "hash": format!("0x{}", hex::encode(block_hash.as_slice())),
                    "parentHash": format!("0x{}", hex::encode(block.header.parent_hash.as_slice())),
                    "number": format!("0x{:x}", block_height),
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
                        state.subscriptions.broadcast_eth_log(serde_json::json!({
                            "address": format!("{:?}", log.address),
                            "topics": log.topics.iter().map(|t| format!("0x{}", hex::encode(t.as_slice()))).collect::<Vec<_>>(),
                            "data": format!("0x{}", hex::encode(&log.data)),
                            "blockNumber": format!("0x{:x}", block_height),
                            "blockHash": format!("0x{}", hex::encode(block_hash.as_slice())),
                            "transactionHash": format!("0x{}", hex::encode(evm.tx_hash.as_slice())),
                            "transactionIndex": format!("0x{:x}", idx),
                            "logIndex": format!("0x{:x}", log_idx),
                            "removed": false,
                        }));
                    }
                }

                if let Ok(mut c) = consensus.write() {
                    if let Err(e) = c.commit_block(&block, &result) {
                        tracing::warn!(height = block_height, error = %e, "sync: failed to commit block to consensus state");
                    }
                }
                state.set_current_block(block_height + 1);
                applied += 1;

                // Update sync progress
                if let Ok(mut sp) = state.sync_progress.write() {
                    if let Some(ref mut p) = *sp {
                        p.current_block = block_height + 1;
                    }
                }
            }
            Err(e) => {
                tracing::warn!(height = block_height, error = %e, "sync: block execution failed");
                break;
            }
        }
    }
    // After applying blocks, check if we crossed an epoch boundary.
    // If so, signal the BFT event loop to restart into the new epoch.
    if applied > 0 {
        let new_height = state.get_current_block();
        let epoch_length = state.consensus_params.read().unwrap().epoch_length;
        let old_epoch = initial_height / epoch_length;
        let new_epoch = new_height / epoch_length;
        if new_epoch > old_epoch {
            tracing::info!(
                old_epoch,
                new_epoch,
                new_height,
                "sync: crossed epoch boundary, requesting engine restart"
            );
            state.engine_restart_signal.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }

    applied
}
