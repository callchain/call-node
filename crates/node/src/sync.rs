//! Sync protocol — handle incoming sync requests and apply synced blocks.

use std::sync::{Arc, RwLock};
use std::path::Path;

use call_consensus::{Block, SimplexConsensus};
use call_primitives::BlockHash;
use call_network::SyncResponse;
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
        Some(block.header.payment_root)
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
                result.payment_root == block.header.payment_root
                    && result.evm_state_root == block.header.evm_state_root
                    && result.bridge_root == block.header.bridge_root
                    && result.receipt_root == block.header.receipt_root
            }
            Err(_) => false,
        };

        if !roots_valid {
            tracing::error!(height = block_height, "sync: state root mismatch — rejecting synced block");
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

                // Generate and store receipts for protocol transactions
                for tr in &result.transaction_results {
                    let tx_hash = tr.tx_hash;
                    let Some(tx) = block.protocol_txs.iter().find(|t| {
                        call_primitives::TxHash::from(t.compute_tx_hash()) == tx_hash
                    }) else { continue; };

                    let receipt = ProtocolReceipt {
                        tx_hash,
                        status: tr.status.clone(),
                        gas_used: tr.gas_used,
                        gas_payer: tx.sender,
                        fee_currency: tx.fee_currency,
                        fee_amount: tr.fee_amount,
                        block_number: block_height,
                        instruction_results: vec![],
                        logs: vec![],
                        memos: vec![],
                        state_changes: vec![],
                    };
                    state.store_receipt(tx_hash, receipt);
                }

                if let Ok(mut c) = consensus.write() {
                    if let Err(e) = c.commit_block(&block, &result) {
                        tracing::warn!(height = block_height, error = %e, "sync: failed to commit block to consensus state");
                    }
                }
                state.set_current_block(block_height + 1);
                applied += 1;
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
