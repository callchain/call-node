//! call-node — Callchain node application.
//!
//! Minimal node that initializes all state components,
//! starts an HTTP RPC server, runs consensus, and processes transactions.

pub mod cli;
pub mod config;
pub mod boot;
pub mod telemetry;
pub mod light_client;
pub mod logging;
pub mod wallet;
pub mod state_persist;

pub mod block_producer;
pub mod bft_loop;
pub mod sync;
pub mod network_handler;

pub(crate) use network_handler::{
    handle_network_message, SyncInflight,
    BLOCK_CHANNEL, SYNC_CHANNEL,
    SYNC_REQUEST_BATCH,
};
pub(crate) use sync::{handle_sync_request, apply_synced_blocks};
pub(crate) use block_producer::block_production_loop;
pub(crate) use bft_loop::bft_event_loop;

use crate::light_client::{LightClient, BlockSignatures, SigBytes, PubKeyBytes};
use call_consensus::{
    Block, ConsensusParams, SimplexConsensus,
    ForkManager,
    bft::{CallAutomaton, CallRelay, CallReporter, FinalizationInfo, ProposeRequest, VerifyRequest},
    block_cache::BlockCache,
    digest::ConsensusDigest,
    proposer::{derive_vrf_seed, select_proposer_subset},
};
use call_network::{CommonwareConfig, CommonwareNetwork, Network, NetworkMessage, SyncRequest};
use call_primitives::BlockHash;
use call_protocol::{
    FeeParams,
    security::P2PDefense,
};
use call_governance::GovernanceManager;
use call_oracle::OracleTracker;
use call_rpc::{RpcState, RpcConfig, build_rpc_module, wire_governance_executor};
use call_storage::{CallDb, open_db, PruneState};
use call_storage::reth_db::{
    save_prune_state as db_save_prune,
};
use crate::state_persist::{
    load_state_from_db, persist_state_to_db, persist_state_incremental,
    load_receipts, load_fork_state,
    check_recovery_needed, clear_checkpoint,
    load_consensus_state_inner, save_consensus_state_inner,
};
use call_transaction_pool::Mempool;
use call_evm::EvmState;
use jsonrpsee::server::ServerHandle;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::Duration;
use std::num::{NonZeroU16, NonZeroU32, NonZeroUsize};
use tokio::sync::mpsc;
use tokio::sync::oneshot;
use commonware_codec::extensions::DecodeExt;

// Commonware Simplex BFT imports
use commonware_consensus::simplex::{Config as SimplexConfig, Engine, ForwardingPolicy};
use commonware_consensus::simplex::elector::RoundRobin;
use commonware_consensus::simplex::scheme::ed25519::Scheme as Ed25519Scheme;
use commonware_consensus::types::{Epoch, ViewDelta};
use commonware_cryptography::ed25519;
use commonware_cryptography::Signer;
use commonware_parallel::Sequential;
use commonware_p2p::AddressableManager;
use commonware_p2p::authenticated::lookup::{self as p2p_lookup, Config as P2PConfig};
use commonware_p2p::utils::mux::Muxer;
use commonware_runtime::tokio::{Config as RuntimeConfig, Runner as TokioRunner};
use commonware_runtime::{Quota, Runner, Metrics};
use commonware_runtime::buffer::paged::CacheRef;
use commonware_utils::ordered::Set;

/// Chain ID for Callchain devnet
pub const CALLCHAIN_CHAIN_ID: u64 = 1337;

/// Reason why the BFT engine exited and needs epoch rotation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EpochRotationReason {
    /// Epoch boundary reached (height % epoch_length == 0)
    EpochBoundary,
}


/// The Callchain node
pub struct CallNode {
    pub state: Arc<RpcState>,
    pub mempool: Arc<RwLock<Mempool>>,
    pub consensus: Arc<RwLock<SimplexConsensus>>,
    pub network: Option<Arc<dyn Network>>,
    pub db: CallDb,
    pub prune_state: PruneState,
    pub server_handle: Option<ServerHandle>,
    pub ws_server_handle: Option<ServerHandle>,
    pub parent_hash: BlockHash,
    /// Shared block cache for BFT digest → block mapping.
    /// Populated by propose, received blocks from P2P relay, and consumed by verify/finalize.
    pub block_cache: Arc<std::sync::Mutex<BlockCache>>,
    /// Telemetry registry for metrics, alerts, and latency histograms
    pub telemetry: Arc<crate::telemetry::TelemetryRegistry>,
    /// Append-only audit log for compliance and tamper evidence
    pub audit_log: Arc<RwLock<crate::logging::AuditLog>>,
    /// True when the node started from empty or corrupted state (genesis should be applied).
    pub fresh_start: bool,
    /// Transient oracle coordinator (prices/TWAP live in EVM storage)
    pub oracle_tracker: Arc<RwLock<OracleTracker>>,
    /// Governance proposal state machine (proposals live in EVM, sidecar manages transitions)
    pub governance: Arc<RwLock<GovernanceManager>>,
}

impl CallNode {
    /// Create a new node with default state
    pub fn new(data_dir: PathBuf) -> Result<Self, String> {
        Self::new_with_chain_id(data_dir, None)
    }

    /// Create a new node with an optional chain_id override (from genesis).
    pub fn new_with_chain_id(data_dir: PathBuf, chain_id: Option<u64>) -> Result<Self, String> {
        let mempool = Arc::new(RwLock::new(Mempool::new()));
        let db = open_db(data_dir.clone()).map_err(|e| format!("failed to open db: {e}"))?;

        // Restore prune state from disk if previously persisted
        let prune_state = db.load_prune_state()
            .map_err(|e| format!("failed to load prune state: {e}"))?;

        let db_env = &db.db;

        // Crash recovery: if a pending checkpoint exists, state may be inconsistent.
        // Clear the marker and start from genesis (safe — partial state is ignored).
        let recovery_needed = check_recovery_needed(db_env)
            .map_err(|e| format!("checkpoint check failed: {e}"))?;
        if recovery_needed {
            tracing::warn!("pending checkpoint detected — previous shutdown was unclean; starting from genesis");
            let _ = clear_checkpoint(db_env);
        }

        let blocks_exist = data_dir.join("blocks").exists();
        let fresh_start = recovery_needed || !blocks_exist;

        // Load persisted state from reth-db (skip if recovery needed)
        let loaded = if recovery_needed {
            state_persist::LoadedState {
                evm_state: EvmState::new(),
                governance: GovernanceManager::new(),
                fee_params: FeeParams::default(),
            }
        } else {
            load_state_from_db(db_env)
        };

        let oracle_tracker = Arc::new(RwLock::new(call_oracle::OracleTracker::default()));

        let receipts = if recovery_needed {
            std::collections::HashMap::new()
        } else {
            match load_receipts(db_env) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load receipts");
                    std::collections::HashMap::new()
                }
            }
        };

        let fork_manager = if recovery_needed {
            ForkManager::new(call_primitives::ProtocolVersion::new(1, 0, 0), 1)
        } else {
            match load_fork_state(db_env) {
                Ok(Some(fm)) => fm,
                Ok(None) => {
                    ForkManager::new(
                        call_primitives::ProtocolVersion::new(1, 0, 0),
                        call_consensus::exec::state_accessors::read_validator_count(&loaded.evm_state) as u32,
                    )
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load fork state");
                    ForkManager::new(
                        call_primitives::ProtocolVersion::new(1, 0, 0),
                        call_consensus::exec::state_accessors::read_validator_count(&loaded.evm_state) as u32,
                    )
                }
            }
        };


        // Try to load persisted consensus state; fall back to genesis
        let consensus = if recovery_needed {
            SimplexConsensus::new(ConsensusParams::default(), &loaded.evm_state)
        } else {
            match load_consensus_state_inner(db_env, &loaded.evm_state) {
                Ok(consensus) => {
                    tracing::info!(
                        height = consensus.current_height(),
                        round = consensus.current_round(),
                        "restored consensus state from db"
                    );
                    consensus
                }
                Err(e) => {
                    tracing::info!(error = %e, "no persisted consensus state, starting from genesis");
                    SimplexConsensus::new(ConsensusParams::default(), &loaded.evm_state)
                }
            }
        };

        let state = Arc::new(RpcState::new(
            loaded.evm_state,
            mempool.clone(),
            chain_id.unwrap_or(CALLCHAIN_CHAIN_ID),
        ));

        state.set_data_dir(data_dir.clone());

        // Rebuild log_index from loaded receipts so eth_getLogs queries work correctly after restart
        for (tx_hash, receipt) in &receipts {
            for (idx, log) in receipt.logs.iter().enumerate() {
                if let Ok(mut index) = state.log_index.write() {
                    index
                        .entry(log.address)
                        .or_insert_with(Vec::new)
                        .push((receipt.block_number, *tx_hash, idx));
                }
            }
        }

        // Inject loaded receipts
        *state.receipts.write().unwrap() = receipts;

        // Inject loaded fork state
        *state.fork_manager.write().unwrap() = fork_manager;

        let mut governance = loaded.governance;
        // Wire governance executor so proposals can trigger real side effects
        wire_governance_executor(&mut governance, &state);
        let governance = Arc::new(RwLock::new(governance));

        // Sync consensus params from SimplexConsensus into RpcState for governance updates
        *state.consensus_params.write().unwrap() = *consensus.params();

        // Sync current_block from consensus height so RPCs report correct block number after restart
        state.set_current_block(consensus.current_height());

        // Inject loaded fee params
        *state.fee_params.write().unwrap() = loaded.fee_params;

        // Set parent_hash to the last committed block hash from persisted state
        let parent_hash = consensus.last_block_hash();

        // Initialize telemetry and audit log
        let telemetry = Arc::new(crate::telemetry::TelemetryRegistry::new(data_dir.clone()));
        let audit_path = data_dir.join("audit.log");
        let audit_log = Arc::new(RwLock::new(
            crate::logging::AuditLog::open_file(&audit_path)
                .map_err(|e| format!("failed to open audit log: {e}"))?,
        ));

        Ok(Self {
            state,
            mempool,
            consensus: Arc::new(RwLock::new(consensus)),
            network: None,
            db,
            prune_state,
            server_handle: None,
            ws_server_handle: None,
            parent_hash,
            block_cache: Arc::new(std::sync::Mutex::new(BlockCache::new(1000))),
            telemetry,
            audit_log,
            fresh_start,
            oracle_tracker,
            governance,
        })
    }

    /// Start the HTTP RPC server (HTTP or HTTPS depending on TLS config)
    pub async fn start_rpc(&mut self, config: RpcConfig) -> Result<(), String> {
        let module = build_rpc_module(Arc::clone(&self.state))
            .map_err(|e| format!("failed to build RPC module: {e}"))?;

        let handle = call_rpc::start_http_server(config, module)
            .await
            .map_err(|e| format!("HTTP server start failed: {e}"))?;

        self.server_handle = Some(handle);
        Ok(())
    }

    /// Start the WebSocket RPC server for subscriptions.
    /// Note: jsonrpsee 0.24 Server handles both HTTP and WS on the same port.
    /// This starts a second server on the WS address for WS-only connections.
    pub async fn start_ws_rpc(&mut self, config: RpcConfig) -> Result<(), String> {
        let module = build_rpc_module(Arc::clone(&self.state))
            .map_err(|e| format!("failed to build WS module: {e}"))?;

        let handle = call_rpc::start_ws_server(config, module)
            .await
            .map_err(|e| format!("WS server start failed: {e}"))?;

        self.ws_server_handle = Some(handle);
        Ok(())
    }

    /// Start P2P network
    pub async fn start_network(
        &mut self,
        config: CommonwareConfig,
        identity_key: ed25519::PrivateKey,
    ) -> Result<(), String> {
        let network = CommonwareNetwork::new(&config, identity_key)
            .await
            .map_err(|e| format!("network init failed: {e}"))?;
        let network: Arc<dyn Network> = Arc::new(network);
        self.network = Some(Arc::clone(&network));

        // Wire network into RpcState so RPC handlers can gossip protocol txs
        if let Ok(mut net_guard) = self.state.network.write() {
            *net_guard = Some(Arc::clone(&network));
        }

        // Start receive loop
        let mempool = Arc::clone(&self.mempool);
        let state = Arc::clone(&self.state);
        let consensus = Arc::clone(&self.consensus);
        let data_dir = self.db.data_dir.clone();
        let block_cache = Arc::clone(&self.block_cache);
        let net_clone = Arc::clone(&network);
        let telemetry = Arc::clone(&self.telemetry);
        let oracle_tracker = Arc::clone(&self.oracle_tracker);
        let p2p_defense = std::sync::Mutex::new(P2PDefense::new(10000, 1000, 10 * 1024 * 1024));
        // Tracks SyncRequests we've sent recently. Both the announcement
        // handler (BLOCK_CHANNEL) and the response handler (SYNC_CHANNEL)
        // share this map so an in-flight request for a given peer suppresses
        // new requests to that peer until either the response lands or the
        // timeout elapses. Without this debounce, a 4-validator network at
        // 4 Hz produces ~16 announcements/sec per validator and we'd spawn
        // an equal number of redundant SyncRequests, each pulling a fresh
        // SyncResponse and tripping the per-peer P2PDefense rate limit.
        let sync_inflight: SyncInflight =
            Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
        tokio::spawn(async move {
            while let Ok((peer_id, channel, data)) = net_clone.receive().await {
                // P2P defense: rate limiting + max message size
                let now_ms = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as u64;
                if let Err(e) = p2p_defense.lock().unwrap().validate_message(peer_id.clone(), data.len(), now_ms) {
                    tracing::warn!(peer_id, error = %e, size = data.len(), "p2p: message rejected by defense");
                    continue;
                }
                telemetry.record_p2p_bytes_received(data.len());
                telemetry.set_p2p_peers(net_clone.peer_count());
                if channel == SYNC_CHANNEL {
                    // Handle sync requests: respond with blocks
                    if let Ok(NetworkMessage::SyncRequest(request)) = bincode::deserialize(&data) {
                        tracing::debug!(peer_id, start = request.start_height, count = request.count, "sync: request from peer");
                        // Spawn the disk-and-serialize work into its own task
                        // so the receive loop doesn't block while we read up
                        // to SYNC_REQUEST_BATCH blocks from MDBX and JSON-
                        // serialize them. Without this, a single inbound
                        // SyncRequest can stall *all* incoming traffic
                        // (including the SyncResponses we ourselves are
                        // waiting on) for hundreds of milliseconds.
                        let data_dir_owned = data_dir.clone();
                        let net_for_resp = Arc::clone(&net_clone);
                        let peer_for_resp = peer_id.clone();
                        tokio::spawn(async move {
                            if let Some(response) = handle_sync_request(&data_dir_owned, &request) {
                                let resp_data = bincode::serialize(&NetworkMessage::SyncResponse(response))
                                    .expect("serialize sync response");
                                net_for_resp.send_to(SYNC_CHANNEL, vec![peer_for_resp], resp_data).await;
                            }
                        });
                    } else if let Ok(NetworkMessage::SyncResponse(response)) = bincode::deserialize(&data) {
                        // A response for our outstanding request landed —
                        // free the in-flight slot for this peer so the next
                        // BlockAnnouncement (or our own catch-up below) can
                        // immediately drive another request.
                        sync_inflight.lock().unwrap().remove(&peer_id);

                        // Apply blocks delivered by a peer in response to a SyncRequest.
                        // This is the path full / archive nodes use to follow the
                        // canonical chain finalized by the validators (a peer's
                        // BlockAnnouncement triggers a SyncRequest in the
                        // BLOCK_CHANNEL handler, the response lands here).
                        let applied = apply_synced_blocks(&response, &state, &consensus, &data_dir);
                        let new_local = state.get_current_block();
                        if applied > 0 {
                            tracing::info!(
                                peer_id,
                                start = response.start_height,
                                applied,
                                local_height = new_local,
                                "sync: applied blocks from peer"
                            );

                            // Catch-up: if the peer just gave us a full
                            // batch (i.e. it likely has more), proactively
                            // request the next chunk from the same peer
                            // instead of waiting for another BlockAnnouncement
                            // (which arrives at most every block_time and
                            // would otherwise leave us several seconds
                            // behind real time on every batch).
                            if applied as u64 >= SYNC_REQUEST_BATCH {
                                let now_ms2 = std::time::SystemTime::now()
                                    .duration_since(std::time::UNIX_EPOCH)
                                    .map(|d| d.as_millis() as u64)
                                    .unwrap_or(0);
                                let mut should_request = false;
                                {
                                    let mut inflight = sync_inflight.lock().unwrap();
                                    if !inflight.contains_key(&peer_id) {
                                        inflight.insert(peer_id.clone(), now_ms2);
                                        should_request = true;
                                    }
                                }
                                if should_request {
                                    let next = SyncRequest {
                                        start_height: new_local,
                                        count: SYNC_REQUEST_BATCH,
                                        full_state: false,
                                    };
                                    if let Ok(req_data) = bincode::serialize(&NetworkMessage::SyncRequest(next)) {
                                        let net = Arc::clone(&net_clone);
                                        let peer = peer_id.clone();
                                        tokio::spawn(async move {
                                            net.send_to(SYNC_CHANNEL, vec![peer], req_data).await;
                                        });
                                    }
                                }
                            }
                        }
                    }
                } else if channel == BLOCK_CHANNEL {
                    // Two valid payload kinds on this channel:
                    //   1) `NetworkMessage::BlockAnnouncement` (bincode) — the
                    //      post-finalize broadcast emitted by validators in the
                    //      BFT event loop.
                    //   2) A serde_json-serialized `Block` — the BFT relay's
                    //      block dissemination so peers can satisfy proposals
                    //      under verification (see `CallRelay::broadcast`).
                    // Each codec MUST match its sender; mixing them silently
                    // dropped traffic in the past.
                    if let Ok(NetworkMessage::BlockAnnouncement(_)) =
                        bincode::deserialize::<NetworkMessage>(&data)
                    {
                        handle_network_message(&peer_id, channel, &data, &mempool, &state, &net_clone, &sync_inflight, &oracle_tracker);
                    } else if let Ok(block) = serde_json::from_slice::<Block>(&data) {
                        // Full block received from BFT relay — insert into cache for verify
                        let digest = ConsensusDigest::from(block.header.hash());
                        let cache_size = {
                            let mut cache = block_cache.lock().unwrap();
                            cache.insert(digest, block);
                            cache.len()
                        };
                        tracing::info!(digest = %digest, peer_id, cache_size, "BFT: relayed block received, inserted into cache");
                    } else {
                        tracing::debug!(peer_id, "BLOCK_CHANNEL: unknown message format");
                    }
                } else {
                    handle_network_message(&peer_id, channel, &data, &mempool, &state, &net_clone, &sync_inflight, &oracle_tracker);
                }
            }
        });

        Ok(())
    }

    /// Start the consensus block production loop
    pub fn start_consensus_loop(&self) -> tokio::task::JoinHandle<()> {
        let state = Arc::clone(&self.state);
        let mempool = Arc::clone(&self.mempool);
        let consensus = Arc::clone(&self.consensus);
        let network = self.network.clone();
        let parent_hash = self.parent_hash;
        let db = self.db.clone();
        let prune_state = self.prune_state.clone();
        let telemetry = Arc::clone(&self.telemetry);
        let audit_log = Arc::clone(&self.audit_log);

        let subscriptions = self.state.subscriptions.clone();
        let oracle_tracker = Arc::clone(&self.oracle_tracker);
        let governance = Arc::clone(&self.governance);

        tokio::spawn(block_production_loop(
            state,
            mempool,
            consensus,
            network,
            parent_hash,
            db,
            prune_state,
            subscriptions,
            telemetry,
            audit_log,
            oracle_tracker,
            governance,
        ))
    }

    /// Start the Commonware Simplex BFT consensus engine.
    ///
    /// Epoch coordinator: selects VRF participant subset each epoch,
    /// starts the BFT engine if this node is selected, and sleeps
    /// until the next epoch boundary if not.
    pub fn start_bft_engine(
        &self,
        ed25519_private_key: ed25519::PrivateKey,
        consensus_p2p_port: u16,
        bft_bootstrap_peers: Vec<(ed25519::PublicKey, std::net::SocketAddr)>,
    ) -> std::thread::JoinHandle<()> {
        let state = Arc::clone(&self.state);
        let mempool = Arc::clone(&self.mempool);
        let consensus = Arc::clone(&self.consensus);
        let db = self.db.clone();
        let prune_state = self.prune_state.clone();
        let subscriptions = self.state.subscriptions.clone();
        let network = self.network.clone();
        let data_dir = self.db.data_dir.clone();
        let block_cache = Arc::clone(&self.block_cache);
        let telemetry = Arc::clone(&self.telemetry);
        let audit_log = Arc::clone(&self.audit_log);
        let oracle_tracker = Arc::clone(&self.oracle_tracker);
        let governance = Arc::clone(&self.governance);

        std::thread::spawn(move || {
            let bft_data_dir = data_dir.join("bft_journal");
            let runtime_cfg = RuntimeConfig::new().with_storage_directory(&bft_data_dir);
            let runner = TokioRunner::new(runtime_cfg);
            runner.start(|context| async move {
                let signer = ed25519_private_key.clone();
                let listen_addr = std::net::SocketAddr::from(([0, 0, 0, 0], consensus_p2p_port));

                let p2p_cfg = P2PConfig::local(
                    signer,
                    b"callchain-consensus",
                    listen_addr,
                    10 * 1024 * 1024,
                );
                let (mut network_p2p, mut oracle) = p2p_lookup::Network::new(
                    context.with_label("consensus-p2p"),
                    p2p_cfg,
                );

                // Register bootstrap peers
                if !bft_bootstrap_peers.is_empty() {
                    let entries: Vec<(ed25519::PublicKey, commonware_p2p::Address)> =
                        bft_bootstrap_peers
                            .iter()
                            .map(|(pk, addr)| (pk.clone(), commonware_p2p::Address::Symmetric(*addr)))
                            .collect();
                    let peer_map = commonware_utils::ordered::Map::from_iter_dedup(entries);
                    oracle.track(0, peer_map).await;
                }

                // Register 3 consensus channels and wrap each in a Muxer
                let quota = Quota::per_second(NonZeroU32::new(10000).unwrap());
                let (vote_s, vote_r) = network_p2p.register(1, quota.clone(), 100_000);
                let (cert_s, cert_r) = network_p2p.register(2, quota.clone(), 100_000);
                let (resolve_s, resolve_r) = network_p2p.register(3, quota, 100_000);
                let _net_handle = network_p2p.start();

                let (vote_mux, mut vote_handle) = Muxer::new(
                    context.with_label("vote_mux"),
                    vote_s,
                    vote_r,
                    1024,
                );
                let (cert_mux, mut cert_handle) = Muxer::new(
                    context.with_label("cert_mux"),
                    cert_s,
                    cert_r,
                    1024,
                );
                let (resolve_mux, mut resolve_handle) = Muxer::new(
                    context.with_label("resolve_mux"),
                    resolve_s,
                    resolve_r,
                    1024,
                );
                vote_mux.start();
                cert_mux.start();
                resolve_mux.start();

                let mut subchannel_counter: u64 = 100;
                // Monotonic epoch counter for commonware-consensus.
                // Each engine restart (epoch boundary or validator-set change)
                // must use a unique epoch so stale BFT messages from the old
                // engine are rejected rather than causing panics.
                let mut epoch_counter: u64 = 0;

                loop {
                    epoch_counter += 1;
                    let (parent_hash, _current_height, epoch_number, subset, my_index, subset_pubkeys) = {
                        let c = consensus.read().unwrap();
                        let ph = c.last_block_hash();
                        let height = c.current_height();
                        let epoch_length = state.consensus_params.read().unwrap().epoch_length;
                        let epoch_number = height / epoch_length;
                        let seed = derive_vrf_seed(&ph, epoch_number);

                        let (qualified, pubkeys) = {
                            let evm_state = state.evm_state.read().unwrap();
                            let params = state.consensus_params.read().unwrap();
                            let count = call_consensus::exec::state_accessors::read_validator_count(&evm_state);
                            let mut qualified = Vec::new();
                            let mut pubkeys = std::collections::HashMap::new();
                            for id in 1..=count {
                                let addr = call_consensus::exec::state_accessors::read_validator_addr(&evm_state, id);
                                if addr == call_primitives::Address::ZERO { continue; }
                                let stake = call_consensus::exec::state_accessors::read_validator_stake(&evm_state, addr);
                                let status = call_consensus::exec::state_accessors::read_validator_status(&evm_state, addr);
                                let pk = call_consensus::exec::state_accessors::read_validator_pubkey(&evm_state, addr);
                                if status != 0 && stake >= params.min_self_stake {
                                    qualified.push(id as u32);
                                }
                                pubkeys.insert(id as u32, pk);
                            }
                            (qualified, pubkeys)
                        };

                        let params = state.consensus_params.read().unwrap();
                        let subset =
                            select_proposer_subset(&qualified, &pubkeys, &seed, params.subset_size);

                        let subset_pubkeys: Vec<[u8; 32]> = subset
                            .iter()
                            .filter_map(|id| pubkeys.get(id).copied())
                            .collect();

                        let my_pk = ed25519_private_key.public_key();
                        let my_encoded = commonware_codec::Encode::encode(&my_pk);
                        let my_index = subset
                            .iter()
                            .enumerate()
                            .find(|(_, id)| {
                                pubkeys
                                    .get(id)
                                    .is_some_and(|pk| pk.as_slice() == my_encoded.as_ref())
                            })
                            .map(|(i, _)| i);

                        (ph, height, epoch_number, subset, my_index, subset_pubkeys)
                    };

                    if my_index.is_some() {
                        tracing::info!(
                            epoch = epoch_number,
                            subset_size = subset.len(),
                            "BFT: selected for epoch, starting engine"
                        );

                        let mut keys: Vec<ed25519::PublicKey> = Vec::new();
                        {
                            let evm_state = state.evm_state.read().unwrap();
                            for id in &subset {
                                let addr = call_consensus::exec::state_accessors::read_validator_addr(&evm_state, *id as u64);
                                if addr != call_primitives::Address::ZERO {
                                    let pk = call_consensus::exec::state_accessors::read_validator_pubkey(&evm_state, addr);
                                    if let Ok(pk) = ed25519::PublicKey::decode(&pk[..]) {
                                        keys.push(pk);
                                    }
                                }
                            }
                        }
                        let participants = Set::from_iter_dedup(keys);

                        let scheme = Ed25519Scheme::signer(
                            b"callchain-consensus",
                            participants,
                            ed25519_private_key.clone(),
                        )
                        .expect("ed25519 key must be in participant set");

                        let (propose_tx, propose_rx) = mpsc::channel::<ProposeRequest>(16);
                        let (verify_tx, verify_rx) = mpsc::channel::<VerifyRequest>(16);
                        let (finalize_tx, finalize_rx) = mpsc::channel::<FinalizationInfo>(16);
                        let (broadcast_tx, broadcast_rx) = mpsc::channel::<Vec<u8>>(256);

                        let automaton = CallAutomaton::new(propose_tx, verify_tx);
                        let relay = CallRelay::new(Arc::clone(&block_cache), broadcast_tx);
                        let reporter = CallReporter::new(finalize_tx);

                        let (exit_tx, exit_rx) = oneshot::channel::<EpochRotationReason>();

                        let (vote_sub_s, vote_sub_r) = vote_handle.register(subchannel_counter).await.unwrap();
                        let (cert_sub_s, cert_sub_r) = cert_handle.register(subchannel_counter + 1).await.unwrap();
                        let (resolve_sub_s, resolve_sub_r) = resolve_handle.register(subchannel_counter + 2).await.unwrap();
                        subchannel_counter += 3;

                        let page_cache = CacheRef::from_pooler(
                            &context,
                            NonZeroU16::new(4096).unwrap(),
                            NonZeroUsize::new(1024).unwrap(),
                        );

                        let cfg = SimplexConfig {
                            scheme,
                            elector: RoundRobin::<commonware_cryptography::Sha256>::default(),
                            blocker: oracle.clone(),
                            automaton,
                            relay,
                            reporter,
                            strategy: Sequential,
                            partition: "callchain".to_string(),
                            mailbox_size: 1024,
                            epoch: Epoch::new(epoch_counter),
                            replay_buffer: NonZeroUsize::new(8192).unwrap(),
                            write_buffer: NonZeroUsize::new(8192).unwrap(),
                            page_cache,
                            leader_timeout: Duration::from_millis(500),
                            certification_timeout: Duration::from_millis(750),
                            timeout_retry: Duration::from_millis(250),
                            activity_timeout: ViewDelta::new(10),
                            skip_timeout: ViewDelta::new(5),
                            fetch_timeout: Duration::from_secs(2),
                            fetch_concurrent: 4,
                            forwarding: ForwardingPolicy::SilentVoters,
                        };

                        let engine = Engine::new(
                            context.with_label("simplex"),
                            cfg,
                        );
                        let engine_handle = engine.start(
                            (vote_sub_s, vote_sub_r),
                            (cert_sub_s, cert_sub_r),
                            (resolve_sub_s, resolve_sub_r),
                        );

                        let event_loop_handle = tokio::spawn(bft_event_loop(
                            propose_rx,
                            verify_rx,
                            finalize_rx,
                            broadcast_rx,
                            state.clone(),
                            mempool.clone(),
                            consensus.clone(),
                            block_cache.clone(),
                            db.clone(),
                            prune_state.clone(),
                            subscriptions.clone(),
                            parent_hash,
                            network.clone(),
                            data_dir.clone(),
                            telemetry.clone(),
                            audit_log.clone(),
                            epoch_number,
                            exit_tx,
                            subset_pubkeys,
                            {
                                let pk = ed25519_private_key.public_key();
                                let encoded = commonware_codec::Encode::encode(&pk);
                                let bytes: [u8; 32] = encoded.as_ref().try_into().expect("ed25519 pubkey is 32 bytes");
                                bytes
                            },
                            oracle_tracker.clone(),
                            governance.clone(),
                        ));

                        let reason = exit_rx.await;
                        event_loop_handle.abort();
                        engine_handle.abort();

                        // Grace period: old engine tasks need time to cancel and
                        // SubReceivers need time to deregister before we re-register
                        // on the next loop iteration. Without this, stale BFT messages
                        // from the old engine can leak into the new one.
                        tokio::time::sleep(Duration::from_millis(500)).await;

                        match reason {
                            Ok(r) => {
                                tracing::info!(
                                    ?r,
                                    epoch = epoch_number,
                                    "BFT: engine exited for epoch rotation"
                                );
                            }
                            Err(e) => {
                                tracing::error!(?e, "BFT: exit channel canceled");
                            }
                        }
                        // epoch_number is derived from current_height / epoch_length on next iteration
                        // do NOT increment here
                    } else {
                        let epoch_length = {
                            state.consensus_params.read().unwrap().epoch_length
                        };
                        let current_height = {
                            consensus.read().unwrap().current_height()
                        };
                        let next_epoch_height =
                            (current_height / epoch_length + 1) * epoch_length;
                        let blocks_to_wait = next_epoch_height.saturating_sub(current_height);
                        let sleep_secs =
                            Duration::from_secs(blocks_to_wait.saturating_mul(2).max(1));
                        tracing::info!(
                            epoch = current_height / epoch_length,
                            wait_blocks = blocks_to_wait,
                            "BFT: not selected for epoch, sleeping"
                        );
                        tokio::time::sleep(sleep_secs).await;
                        // epoch_number is derived from current_height / epoch_length on next iteration
                        // do NOT increment here
                    }
                }
            });
        })
    }

    /// Start P2P sync: compare local height with peer height and catch up if behind.
    /// Returns a handle that performs sync then exits.
    pub fn start_sync(&self, network: Arc<dyn Network>) -> tokio::task::JoinHandle<()> {
        let data_dir = self.db.data_dir.clone();
        let state = Arc::clone(&self.state);
        let consensus = Arc::clone(&self.consensus);
        let db_env = self.db.db.clone();

        tokio::spawn(async move {
            // Give the network a moment to connect to peers
            tokio::time::sleep(Duration::from_secs(2)).await;

            // Start from persisted consensus height (more accurate than scanning blocks dir)
            let consensus_height = consensus.read().unwrap().current_height();
            let disk_height = find_latest_height(&data_dir);
            let mut local_height = consensus_height.max(disk_height);
            tracing::info!(local_height, consensus_height, disk_height, "sync: checking local state");

            const BATCH_SIZE: u64 = 100;
            const MAX_EMPTY_ROUNDS: u32 = 3;
            let mut empty_rounds = 0;

            // Initialise sync progress for eth_syncing
            {
                let highest_block = {
                    let heights = state.peer_heights.read().unwrap();
                    heights.values().copied().max().unwrap_or(local_height)
                };
                let mut sp = state.sync_progress.write().unwrap();
                *sp = Some(call_rpc::handlers::SyncProgress {
                    starting_block: local_height,
                    current_block: local_height,
                    highest_block,
                });
            }

            // Build light client from current validator set once at the start
            let (trusted_validators, total_validators, bls_pubkeys) = {
                let evm_state = state.evm_state.read().unwrap();
                let count = call_consensus::exec::state_accessors::read_validator_count(&evm_state);
                let mut ed25519_map = std::collections::HashMap::new();
                let mut bls_map = std::collections::HashMap::new();
                for id in 1..=count {
                    let addr = call_consensus::exec::state_accessors::read_validator_addr(&evm_state, id);
                    if addr == call_primitives::Address::ZERO { continue; }
                    let pk = call_consensus::exec::state_accessors::read_validator_pubkey(&evm_state, addr);
                    let bls_pk = call_consensus::exec::state_accessors::read_validator_bls_pubkey(&evm_state, addr);
                    ed25519_map.insert(id as u32, pk);
                    if bls_pk != [0u8; 48] {
                        bls_map.insert(id as u32, bls_pk);
                    }
                }
                (ed25519_map, count as u32, bls_map)
            };

            let mut light_client = LightClient::new(
                state.chain_id,
                trusted_validators,
                total_validators,
            );
            light_client.set_bls_pubkeys(bls_pubkeys);

            loop {
                // Check if we have peers
                if network.peer_count() == 0 {
                    tracing::warn!("sync: no peers available");
                    break;
                }

                // Request next batch of blocks
                let request = SyncRequest {
                    start_height: local_height,
                    count: BATCH_SIZE,
                    full_state: false,
                };
                let req_data = bincode::serialize(&NetworkMessage::SyncRequest(request))
                    .expect("serialize sync request");
                network.broadcast(SYNC_CHANNEL, req_data).await;

                let mut batch_applied = 0;
                let mut received_any = false;

                // Dynamic response collection: adapt to peer count instead of hardcoded 5.
                //
                // Algorithm:
                //   1. expected = min(peer_count, 10).max(1)
                //   2. First response waits up to 5s (network latency + processing).
                //   3. After first response, shorten timeout to 1s for fast convergence.
                //   4. Hard ceiling: 30s per request round to avoid hanging forever.
                //   5. Process the first valid SyncResponse with the most blocks and
                //      skip the rest—every peer serves the same range.
                let peer_count = network.peer_count();
                let expected_responses = (peer_count as usize).clamp(1, 10);
                let first_timeout = Duration::from_secs(5);
                let subsequent_timeout = Duration::from_secs(1);
                let round_deadline = Duration::from_secs(30);

                let mut timeout = first_timeout;
                let round_start = std::time::Instant::now();
                let mut responses_received = 0usize;
                let mut best_response: Option<call_network::SyncResponse> = None;
                let mut best_peer: String = String::new();

                while responses_received < expected_responses
                    && round_start.elapsed() < round_deadline
                {
                    let remaining = round_deadline.saturating_sub(round_start.elapsed());
                    let wait = timeout.min(remaining);

                    let result = tokio::time::timeout(wait, network.receive()).await;

                    match result {
                        Ok(Ok((peer_id, channel, data))) => {
                            if channel != SYNC_CHANNEL {
                                continue;
                            }
                            if let Ok(NetworkMessage::SyncResponse(response)) =
                                bincode::deserialize(&data)
                            {
                                received_any = true;
                                responses_received += 1;
                                tracing::info!(
                                    peer_id = %peer_id,
                                    start = response.start_height,
                                    block_count = response.blocks.len(),
                                    "sync: received blocks from peer"
                                );

                                // Keep the response with the most blocks.
                                if best_response.as_ref().map_or(
                                    true,
                                    |best| response.blocks.len() > best.blocks.len(),
                                ) {
                                    best_peer = peer_id;
                                    best_response = Some(response);
                                }

                                // After first valid response, switch to shorter timeout
                                // for fast convergence.
                                timeout = subsequent_timeout;
                            }
                        }
                        Ok(Err(e)) => {
                            tracing::debug!(error = %e, "sync: network receive error");
                        }
                        Err(_) => {
                            // Timeout on this iteration. If we already have a valid
                            // response, stop waiting; otherwise keep looping until
                            // round_deadline.
                            if best_response.is_some() {
                                tracing::debug!(
                                    received = responses_received,
                                    expected = expected_responses,
                                    "sync: early stop after timeout with valid response"
                                );
                                break;
                            }
                        }
                    }
                }

                // Apply the best response (if any)
                if let Some(response) = best_response {
                    tracing::info!(
                        peer_id = %best_peer,
                        block_count = response.blocks.len(),
                        "sync: applying best response"
                    );

                    for block_data in &response.blocks {
                        if let Ok(mut block) = serde_json::from_slice::<Block>(block_data) {
                            let height = block.header.height;

                            // Light client verification
                            let sig = &block.header.signature;
                            let signatures = BlockSignatures {
                                block_hash: block.header.hash(),
                                signatures: vec![(
                                    block.header.proposer,
                                    PubKeyBytes([0u8; 32]),
                                    SigBytes(sig.0),
                                )],
                            };

                            if let Err(e) = light_client.verify_header(&block.header, &signatures) {
                                tracing::warn!(height, error = %e, "sync: header verification failed, skipping");
                                continue;
                            }

                            // Execute block
                            let execute_result = state.write_all().execute_block(&block, height);

                            if let Ok(result) = execute_result {
                                block.finalize(&result);
                                let _ = persist_block(&data_dir, height, &block);

                                // Push fee history entry for light-client sync path
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
                                    let priority_fee_rewards: Vec<u128> = [0.0_f64, 10.0, 50.0, 90.0, 100.0]
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

                                if let Ok(mut c) = consensus.write() {
                                    let mut evm_state = state.evm_state.write().unwrap();
                                    let _ = c.commit_block(&block, &result, &mut evm_state);
                                }

                                let _ = light_client.sync_incremental(&block.header, &signatures);
                                state.set_current_block(height + 1);
                                local_height = height + 1;
                                batch_applied += 1;

                                // Update sync progress for eth_syncing
                                if let Ok(mut sp) = state.sync_progress.write() {
                                    if let Some(ref mut p) = *sp {
                                        p.current_block = local_height;
                                    }
                                }

                                // Periodically save consensus state during sync
                                if local_height % 100 == 0 {
                                    if let Ok(c) = consensus.read() {
                                        let _ = save_consensus_state_inner(&db_env, &c);
                                    }
                                }
                            }
                        }
                    }
                }

                if received_any {
                    empty_rounds = 0;
                } else {
                    empty_rounds += 1;
                    if empty_rounds >= MAX_EMPTY_ROUNDS {
                        tracing::info!("sync: no responses after {MAX_EMPTY_ROUNDS} rounds, stopping");
                        break;
                    }
                    continue;
                }

                if batch_applied > 0 {
                    tracing::info!(applied = batch_applied, height = local_height, "sync: batch applied");
                }

                // If we received fewer blocks than requested, we're caught up
                if batch_applied < BATCH_SIZE {
                    tracing::info!(height = local_height, "sync: caught up");
                    break;
                }
            }

            // Clear sync progress — node is fully synced (or gave up)
            *state.sync_progress.write().unwrap() = None;

            if local_height > 0 {
                tracing::info!(height = local_height, "sync: completed");
            } else {
                tracing::info!("sync: no peers responded, starting fresh");
            }

            // Save recovered consensus state after sync
            let c = consensus.read().unwrap();
            if let Err(e) = save_consensus_state_inner(&db_env, &c) {
                tracing::warn!(error = %e, "sync: failed to save consensus state after sync");
            }
        })
    }

    /// Stop the RPC server and flush final state to disk.
    pub async fn stop(&mut self) -> Result<(), String> {
        if let Some(handle) = self.server_handle.take() {
            let _ = handle.stop();
        }
        if let Some(handle) = self.ws_server_handle.take() {
            let _ = handle.stop();
        }
        // Flush final state to reth-db
        let db_env = &self.db.db;
        if let Err(e) = persist_state_to_db(db_env, &self.state, &self.consensus, &self.governance) {
            tracing::warn!(error = %e, "failed to flush state on shutdown");
        }
        if let Err(e) = db_save_prune(db_env, &self.prune_state) {
            tracing::warn!(error = %e, "failed to flush prune state on shutdown");
        }
        // Save consensus state explicitly
        {
            let c = self.consensus.read().unwrap();
            if let Err(e) = save_consensus_state_inner(db_env, &c) {
                tracing::warn!(error = %e, "failed to flush consensus state on shutdown");
            }
        }
        tracing::info!("flushed state to reth-db on shutdown");
        Ok(())
    }

    /// Get mempool stats (EVM count, known tx count)
    pub fn mempool_stats(&self) -> (usize, usize) {
        let mempool = self.mempool.read().unwrap();
        (
            mempool.evm_pool.len(),
            mempool.known_txs.len(),
        )
    }

    /// Inject a network implementation (for testing).
    pub fn inject_network(&mut self, network: Arc<dyn Network>) {
        self.network = Some(Arc::clone(&network));
    }
}

fn current_timestamp_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1)
}

pub(crate) fn persist_block(data_dir: &Path, height: u64, block: &Block) -> Result<(), String> {
    let dir = data_dir.join("blocks");
    std::fs::create_dir_all(&dir)
        .map_err(|e| format!("failed to create blocks dir: {e}"))?;
    let path = dir.join(format!("{height:012}.json"));
    let data = serde_json::to_vec(block)
        .map_err(|e| format!("failed to serialize block: {e}"))?;
    std::fs::write(&path, data)
        .map_err(|e| format!("failed to write block {height}: {e}"))?;
    Ok(())
}

/// Load a single block from disk by height.
fn load_block(data_dir: &Path, height: u64) -> Option<Block> {
    let path = data_dir.join("blocks").join(format!("{height:012}.json"));
    let data = std::fs::read(&path).ok()?;
    serde_json::from_slice(&data).ok()
}

/// Find the highest block height on disk by scanning the blocks directory.
fn find_latest_height(data_dir: &Path) -> u64 {
    let dir = data_dir.join("blocks");
    if !dir.exists() {
        return 0;
    }
    let mut max_height = 0u64;
    if let Ok(entries) = std::fs::read_dir(&dir) {
        for entry in entries.flatten() {
            if let Some(name) = entry.file_name().to_str() {
                if let Ok(h) = name.strip_suffix(".json").unwrap_or(name).parse::<u64>() {
                    if h > max_height {
                        max_height = h;
                    }
                }
            }
        }
    }
    max_height
}

impl Default for CallNode {
    fn default() -> Self {
        Self::new(PathBuf::from(".call-data")).expect("default node creation")
    }
}

#[cfg(test)]
mod tests;

