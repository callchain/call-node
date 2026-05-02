//! Network message handler — dispatches P2P messages by channel.

use std::sync::{Arc, RwLock};

use call_network::{
    Network, NetworkMessage,
    TransactionMessage, SyncRequest, OraclePriceRequest, OraclePriceSubmission,
    UpgradeAnnouncement,
};
use call_oracle::{OracleManager, OracleSubmission};
use call_rpc::RpcState;
use call_transaction_pool::Mempool;

/// P2P channel for transaction gossip.
pub(crate) const TX_CHANNEL: u64 = 1;
/// P2P channel for block announcements.
pub(crate) const BLOCK_CHANNEL: u64 = 2;
/// P2P channel for sync requests/responses.
pub(crate) const SYNC_CHANNEL: u64 = 3;
/// P2P channel for oracle price submissions.
pub(crate) const ORACLE_CHANNEL: u64 = 4;
/// P2P channel for upgrade announcements.
pub(crate) const UPGRADE_CHANNEL: u64 = 5;

/// How many blocks to request per sync batch.
/// 100 blocks is a safe default: at ~1 KB per block it stays well under the
/// network's 10 MB `max_message_size` (a 1 KB block × 100 = 100 KB ≪ 10 MB).
pub(crate) const SYNC_REQUEST_BATCH: u64 = 100;

/// How long a peer may be considered "busy serving our SyncRequest" before
/// we allow a fresh request to be issued for that peer. Acts as both a
/// debounce against the high-frequency BlockAnnouncement stream and a
/// timeout in case the peer never responds.
pub(crate) const SYNC_REQUEST_INFLIGHT_TIMEOUT_MS: u64 = 5_000;

/// Tracks the most recent `SyncRequest` we sent to each peer so the
/// announcement-driven request path doesn't spawn a fresh request on every
/// `BlockAnnouncement`. Maps peer_id → unix-millis of the last request.
pub(crate) type SyncInflight = Arc<std::sync::Mutex<std::collections::HashMap<String, u64>>>;

pub(crate) fn handle_network_message(
    peer_id: &str,
    channel: u64,
    data: &[u8],
    mempool: &Arc<RwLock<Mempool>>,
    state: &Arc<RpcState>,
    network: &Arc<dyn Network>,
    sync_inflight: &SyncInflight,
    oracle: &Arc<RwLock<OracleManager>>,
) {
    match channel {
        TX_CHANNEL => {
            if let Ok(tx_msg) = serde_json::from_slice::<TransactionMessage>(data) {
                if tx_msg.verify_checksum() {
                    if let Ok(mut pool) = mempool.write() {
                        // EVM-only mempool
                        if let Ok(evm_tx) = serde_json::from_slice::<call_evm::EvmTransaction>(&tx_msg.data) {
                            let _ = pool.insert_evm_tx(evm_tx);
                        }
                    }
                }
            }
        }
        BLOCK_CHANNEL => {
            // Handle block announcements (post-commit) — trigger sync if behind.
            // The sender wraps the announcement in `NetworkMessage::BlockAnnouncement`
            // and uses bincode (see the BFT event-loop broadcast path); the
            // receiver MUST use the same codec to deserialize.
            let announcement = match bincode::deserialize::<NetworkMessage>(data) {
                Ok(NetworkMessage::BlockAnnouncement(a)) => a,
                Ok(NetworkMessage::EpochBoundarySignal(signal)) => {
                    let mut peer_heights = state.peer_heights.write().unwrap();
                    peer_heights.insert(peer_id.to_string(), signal.height);
                    tracing::debug!(
                        peer_id,
                        height = signal.height,
                        "epoch boundary signal received"
                    );
                    return;
                }
                _ => return,
            };
            let local_height = state.get_current_block();
            if announcement.height <= local_height {
                tracing::debug!(
                    peer_id,
                    height = announcement.height,
                    local = local_height,
                    "block announcement: already caught up"
                );
                return;
            }

            // Debounce: don't pile up SyncRequests against the same peer.
            // At default block_time=250ms we'd otherwise emit ~16 requests/sec
            // per peer just from announcements, blowing past the 100 msg/sec
            // P2PDefense rate limit on the validator and causing most
            // SyncResponses to be silently dropped. Allow at most one
            // outstanding request per peer at a time, with a timeout that
            // covers a slow / lost response.
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_millis() as u64)
                .unwrap_or(0);
            {
                let mut inflight = sync_inflight.lock().unwrap();
                match inflight.get(peer_id) {
                    Some(&ts) if now_ms.saturating_sub(ts) < SYNC_REQUEST_INFLIGHT_TIMEOUT_MS => {
                        tracing::debug!(
                            peer_id,
                            height = announcement.height,
                            local = local_height,
                            "block announcement: sync request already in flight, debounced"
                        );
                        return;
                    }
                    _ => {
                        inflight.insert(peer_id.to_string(), now_ms);
                    }
                }
            }

            tracing::info!(
                peer_id,
                height = announcement.height,
                local = local_height,
                "block announcement: peer ahead, requesting sync"
            );
            let request = SyncRequest {
                start_height: local_height,
                count: SYNC_REQUEST_BATCH,
                full_state: false,
            };
            let req_data = bincode::serialize(&NetworkMessage::SyncRequest(request))
                .expect("serialize sync request");
            let peer_id_owned = peer_id.to_string();
            let net = Arc::clone(network);
            tokio::spawn(async move {
                net.send_to(SYNC_CHANNEL, vec![peer_id_owned], req_data).await;
            });
        }
        SYNC_CHANNEL => {
            // SyncRequest / SyncResponse are handled by the sync task separately
        }
        ORACLE_CHANNEL => {
            if let Ok(request) = serde_json::from_slice::<OraclePriceRequest>(data) {
                // Validator received a price request from proposer.
                // If this node is a registered validator, fetch prices and submit them back.
                let state_clone = Arc::clone(state);
                let net_clone = Arc::clone(network);
                let peer_id_owned = peer_id.to_string();
                tokio::spawn(async move {
                    let is_validator = {
                        let evm_state = state_clone.evm_state.read().unwrap();
                        let count = call_consensus::exec::evm_instructions::read_validator_count(&evm_state);
                        let mut active = 0;
                        for id in 1..=count {
                            let addr = call_consensus::exec::evm_instructions::read_validator_addr(
                                &evm_state, id);
                            if addr != call_primitives::Address::ZERO {
                                let status = call_consensus::exec::evm_instructions::read_validator_status(
                                    &evm_state, addr);
                                if status != 0 {
                                    active += 1;
                                }
                            }
                        }
                        active > 0
                    };
                    if is_validator {
                        // Fetch prices for requested assets using the oracle's tracked assets
                        let current_block = state_clone.get_current_block();
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);

                        // Submit a price for each requested pair
                        for pair in &request.pairs {
                            // Read last known price from EVM storage
                            let price = {
                                let evm = state_clone.evm_state.read().unwrap();
                                call_consensus::exec::evm_instructions::read_oracle_price(&evm, pair.base)
                            };
                            if price != 0 {
                                // Send back as an oracle price submission
                                let submission = OraclePriceSubmission {
                                    validator_id: 0, // would be this validator's ID
                                    pair: *pair,
                                    price,
                                    block_number: current_block,
                                    timestamp,
                                    signature: [0u8; 64], // would be signed
                                    sources: vec!["local_oracle".into()],
                                };
                                if let Ok(msg) = bincode::serialize(&NetworkMessage::OraclePriceSubmission(submission)) {
                                    net_clone.send_to(ORACLE_CHANNEL, vec![peer_id_owned.clone()], msg).await;
                                }
                            }
                        }
                    }
                });
            } else if let Ok(submission) = serde_json::from_slice::<OraclePriceSubmission>(data) {
                // Proposer received a price submission from a validator.
                // Feed it through the oracle's full validation pipeline via RPC-style submission.
                let state_clone = Arc::clone(state);
                let oracle_clone = Arc::clone(oracle);
                tokio::spawn(async move {
                    let current_block = state_clone.get_current_block();
                    let oracle_submission = OracleSubmission {
                        validator_id: submission.validator_id,
                        pair: submission.pair,
                        price: submission.price,
                        block_number: current_block,
                        timestamp: submission.timestamp,
                        signature: submission.signature,
                        sources: submission.sources,
                    };
                    let mut oracle_guard = oracle_clone.write().unwrap();
                    match oracle_guard.submit_price(oracle_submission) {
                        Ok(Some(aggregated)) => {
                            // Quorum reached — write aggregated price to EVM storage
                            let mut evm = state_clone.evm_state.write().unwrap();
                            call_consensus::exec::evm_instructions::seed_oracle_price(
                                &mut evm,
                                aggregated.pair.base,
                                aggregated.median_price,
                                aggregated.median_price, // simplified TWAP (no history in transient manager)
                                aggregated.timestamp,
                                aggregated.block_number,
                                aggregated.submission_count as u64,
                            );
                            tracing::info!(
                                asset_id = aggregated.pair.base,
                                price = aggregated.median_price,
                                contributors = aggregated.submission_count,
                                "oracle: aggregated price written to EVM"
                            );
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::debug!(error = %e, validator_id = submission.validator_id, "oracle P2P submission rejected");
                        }
                    }
                });
            }
        }
        UPGRADE_CHANNEL => {
            if let Ok(announcement) = serde_json::from_slice::<UpgradeAnnouncement>(data) {
                tracing::info!(
                    peer_id,
                    version = ?announcement.version,
                    activation_height = announcement.activation_height,
                    "upgrade announcement received"
                );
                let state_clone = Arc::clone(state);
                tokio::spawn(async move {
                    let mut fm = state_clone.fork_manager.write().unwrap();
                    // Only schedule if we don't already have this exact upgrade pending
                    let already_scheduled = fm.scheduled_upgrades.iter().any(|e| {
                        e.version == announcement.version
                            && e.activation_height == announcement.activation_height
                    });
                    if !already_scheduled {
                        fm.schedule_upgrade(call_consensus::UpgradeEntry {
                            version: announcement.version,
                            activation_height: announcement.activation_height,
                            applied: false,
                            proposal_id: announcement.proposal_id,
                            approved_at_height: None,
                        });
                        tracing::info!(
                            version = ?announcement.version,
                            activation_height = announcement.activation_height,
                            "scheduled upgrade from peer announcement"
                        );
                    }
                });
            }
        }
        _ => {}
    }
}
