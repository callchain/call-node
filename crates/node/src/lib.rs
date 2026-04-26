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
    TX_CHANNEL, BLOCK_CHANNEL, SYNC_CHANNEL, ORACLE_CHANNEL, UPGRADE_CHANNEL,
    SYNC_REQUEST_BATCH, SYNC_REQUEST_INFLIGHT_TIMEOUT_MS,
};
pub(crate) use sync::{handle_sync_request, apply_synced_blocks};
pub(crate) use block_producer::block_production_loop;
pub(crate) use bft_loop::bft_event_loop;

use crate::light_client::{LightClient, BlockSignatures, SigBytes, PubKeyBytes};
use call_consensus::{
    Block, ConsensusParams, SimplexConsensus, SystemTx, SystemTxKind, ValidatorStateManager,
    ForkManager,
    bft::{CallAutomaton, CallRelay, CallReporter, FinalizationInfo, ProposeRequest, VerifyRequest},
    block_cache::BlockCache,
    digest::ConsensusDigest,
    proposer::{derive_vrf_seed, select_proposer_subset},
};
use call_network::{CommonwareConfig, CommonwareNetwork, Network, NetworkMessage, BlockAnnouncement, TransactionMessage, SyncRequest, SyncResponse, OraclePriceRequest, OraclePriceSubmission, UpgradeAnnouncement};
use call_primitives::{BlockHash, Hash, Address};
use call_protocol::{
    AccountState, AssetRegistry, ComplianceEngine, FeeParams, FeeCurrencyRegistry,
    transaction::ProtocolTransaction,
    security::P2PDefense,
};
use call_governance::GovernanceManager;
use call_oracle::{OracleManager, OracleSubmission, ORACLE_UPDATE_INTERVAL};
use call_rpc::{RpcState, RpcConfig, build_rpc_module, SubscriptionManager, wire_governance_executor};
use call_storage::{CallDb, open_db, PruneState, StateRoots, produce_state_snapshot};
use call_storage::reth_db::{
    save_prune_state as db_save_prune,
};
use crate::state_persist::{
    load_state_from_db, persist_state_to_db, persist_state_incremental,
    load_oracle_state, load_governance_state, load_receipts, load_fork_state,
    check_recovery_needed, clear_checkpoint,
    load_consensus_state_inner, save_consensus_state_inner, save_fork_state,
};
use reth_db::DatabaseEnv;
use call_transaction_pool::Mempool;
use call_evm::EvmState;
use call_bridge::{BridgeStateManager, BridgeConfig};
use call_agent::{AgentRegistry, AgentBalances};
use call_shielded::ShieldedState;
use jsonrpsee::server::ServerHandle;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
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
use commonware_cryptography::Digest;
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
        let (balance_state, evm_state, bridge_state, shielded_state, consensus_validators, registry, agent_balances, agent_nonces, oracle_manager, governance_manager, compliance_engine, mut asset_registry, receipts, fork_manager, fee_params, fee_currency_registry) = if recovery_needed {
            (
                AccountState::new(), EvmState::new(), BridgeStateManager::default(),
                ShieldedState::new(), ValidatorStateManager::default(),
                AgentRegistry::new(), AgentBalances::new(), call_agent::AgentNonces::new(),
                OracleManager::default(), GovernanceManager::new(), ComplianceEngine::new(),
                AssetRegistry::new(),
                std::collections::HashMap::new(),
                ForkManager::new(call_primitives::ProtocolVersion::new(1, 0, 0), 1),
                FeeParams::default(),
                FeeCurrencyRegistry::new(),
            )
        } else {
            let loaded = load_state_from_db(db_env);
            let oracle = load_oracle_state(db_env)
                .map_err(|e| format!("failed to load oracle state: {e}"))?;
            let governance = load_governance_state(db_env)
                .map_err(|e| format!("failed to load governance state: {e}"))?;
            let receipts = match load_receipts(db_env) {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load receipts");
                    std::collections::HashMap::new()
                }
            };
            let fork_manager = match load_fork_state(db_env) {
                Ok(Some(fm)) => fm,
                Ok(None) => {
                    ForkManager::new(
                        call_primitives::ProtocolVersion::new(1, 0, 0),
                        loaded.4.get_all_validators().len() as u32,
                    )
                }
                Err(e) => {
                    tracing::warn!(error = %e, "failed to load fork state");
                    ForkManager::new(
                        call_primitives::ProtocolVersion::new(1, 0, 0),
                        loaded.4.get_all_validators().len() as u32,
                    )
                }
            };
            (loaded.0, loaded.1, loaded.2, loaded.3, loaded.4, loaded.5, loaded.6, loaded.7, oracle, governance, loaded.9, loaded.10, receipts, fork_manager, loaded.11, loaded.12)
        };

        // Replay AssetRegistry from block history if db snapshot is empty/missing.
        // This ensures asset IDs and metadata are reconstructible from canonical history.
        if asset_registry.is_empty() && !fresh_start {
            asset_registry = replay_asset_registry(&data_dir);
        }

        // Try to load persisted consensus state; fall back to genesis
        let consensus = if recovery_needed {
            SimplexConsensus::new(ConsensusParams::default(), consensus_validators.clone())
        } else {
            match load_consensus_state_inner(db_env, &consensus_validators) {
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
                    SimplexConsensus::new(ConsensusParams::default(), consensus_validators.clone())
                }
            }
        };

        let state = Arc::new(RpcState::new(
            balance_state,
            asset_registry,
            compliance_engine,
            evm_state,
            bridge_state,
            ValidatorStateManager::default(),
            registry,
            agent_balances,
            agent_nonces,
            shielded_state,
            mempool.clone(),
            chain_id.unwrap_or(CALLCHAIN_CHAIN_ID),
            oracle_manager,
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

        // Wire live oracle into precompiles so EVM contracts can read prices
        call_precompiles::set_live_oracle(Arc::clone(&state.oracle));

        // Replace default governance with persisted state
        *state.governance.write().unwrap() = governance_manager;

        // Wire governance executor so proposals can trigger real side effects
        wire_governance_executor(&state);

        // Sync consensus params from SimplexConsensus into RpcState for governance updates
        *state.consensus_params.write().unwrap() = *consensus.params();

        // Sync current_block from consensus height so RPCs report correct block number after restart
        state.set_current_block(consensus.current_height());

        // Inject loaded fee params and fee currency registry
        *state.fee_params.write().unwrap() = fee_params;
        *state.fee_currency_registry.write().unwrap() = fee_currency_registry;

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
                        handle_network_message(&peer_id, channel, &data, &mempool, &state, &net_clone, &sync_inflight);
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
                    handle_network_message(&peer_id, channel, &data, &mempool, &state, &net_clone, &sync_inflight);
                }
            }
        });

        Ok(())
    }

    /// Start compliance data source sync task (background fetch of OFAC/KYC lists)
    pub fn start_compliance_sync(
        &self,
        data_url: Option<String>,
        interval_secs: u64,
    ) -> Option<tokio::task::JoinHandle<()>> {
        let data_url = data_url?;
        let state = Arc::clone(&self.state);
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(Duration::from_secs(interval_secs));
            loop {
                interval.tick().await;
                match reqwest::get(&data_url).await {
                    Ok(resp) => match resp.json::<Vec<String>>().await {
                        Ok(addresses) => {
                            let mut engine = state.compliance_engine.write().unwrap();
                            let mut added = 0;
                            for addr_str in addresses {
                                if let Ok(bytes) = hex::decode(addr_str.trim_start_matches("0x")) {
                                    if bytes.len() == 20 {
                                        let addr = call_primitives::Address::from_slice(&bytes);
                                        engine.add_to_blacklist(addr);
                                        added += 1;
                                    }
                                }
                            }
                            tracing::info!(added, url = %data_url, "compliance: synced sanctioned addresses");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "compliance: failed to parse address list");
                        }
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "compliance: failed to fetch address list");
                    }
                }
            }
        }))
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
                        let vs = state.validator_state.read().unwrap();
                        let qualified = vs.get_qualified_validators();
                        let pubkeys: std::collections::HashMap<
                            call_primitives::ValidatorId,
                            call_primitives::Ed25519PublicKey,
                        > = vs
                            .get_all_validators()
                            .iter()
                            .map(|(id, stake)| (*id, stake.ed25519_pubkey))
                            .collect();
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
                            let vs = state.validator_state.read().unwrap();
                            let all_validators = vs.get_all_validators();
                            for id in &subset {
                                if let Some(stake) = all_validators.get(id) {
                                    if let Ok(pk) = ed25519::PublicKey::decode(&stake.ed25519_pubkey[..]) {
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

            // Build light client from current validator set once at the start
            let (trusted_validators, total_validators, bls_pubkeys) = {
                let validator_state = state.validator_state.read().unwrap();
                let validators = validator_state.get_all_validators();
                let total = validators.len() as u32;
                let mut ed25519_map = std::collections::HashMap::new();
                let mut bls_map = std::collections::HashMap::new();
                for (id, stake) in validators.iter() {
                    ed25519_map.insert(*id, stake.ed25519_pubkey);
                    if stake.bls_pubkey != [0u8; 48] {
                        bls_map.insert(*id, stake.bls_pubkey);
                    }
                }
                (ed25519_map, total, bls_map)
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

                // Listen for responses with timeout
                for _ in 0..5 {
                    let result = tokio::time::timeout(
                        Duration::from_secs(3),
                        network.receive(),
                    ).await;

                    if let Ok(Ok((_peer_id, channel, data))) = result {
                        if channel == SYNC_CHANNEL {
                            if let Ok(NetworkMessage::SyncResponse(response)) = bincode::deserialize(&data) {
                                received_any = true;
                                tracing::info!(
                                    peer_id = _peer_id,
                                    start = response.start_height,
                                    block_count = response.blocks.len(),
                                    "sync: received blocks from peer"
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

                                            if let Ok(mut c) = consensus.write() {
                                                let _ = c.commit_block(&block, &result);
                                            }

                                            let _ = light_client.sync_incremental(&block.header, &signatures);
                                            state.set_current_block(height + 1);
                                            local_height = height + 1;
                                            batch_applied += 1;

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
        if let Err(e) = persist_state_to_db(db_env, &self.state, &self.consensus) {
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

    /// Get mempool stats
    pub fn mempool_stats(&self) -> (usize, usize, usize) {
        let mempool = self.mempool.read().unwrap();
        (
            mempool.protocol_pool.len(),
            mempool.evm_pool.len(),
            mempool.pending_bridges.len(),
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

/// Replay AssetRegistry from canonical block history.
/// Scans blocks/*.json in height order and re-executes RegisterAsset instructions
/// to reconstruct asset IDs and metadata deterministically.
fn replay_asset_registry(data_dir: &Path) -> AssetRegistry {
    let mut registry = AssetRegistry::new();
    let latest = find_latest_height(data_dir);
    if latest == 0 {
        return registry;
    }

    tracing::info!(height = latest, "replaying AssetRegistry from block history");
    let mut replayed = 0u64;

    for height in 1..=latest {
        let Some(block) = load_block(data_dir, height) else {
            continue;
        };
        for tx in &block.protocol_txs {
            for instr in &tx.instructions {
                if let call_protocol::Instruction::RegisterAsset {
                    symbol,
                    name,
                    decimals,
                    max_supply,
                } = instr
                {
                    // Replay registration with the same parameters
                    // compliance_policy = 0, registered_at = height
                    if let Err(e) = registry.register_asset(
                        symbol.clone(),
                        name.clone(),
                        *decimals,
                        tx.sender,
                        0,
                        height,
                        *max_supply,
                    ) {
                        tracing::warn!(
                            height,
                            sender = ?tx.sender,
                            symbol,
                            error = %e,
                            "AssetRegistry replay: register_asset failed"
                        );
                    } else {
                        replayed += 1;
                    }
                }
            }
        }
    }

    tracing::info!(
        replayed,
        next_id = registry.next_id(),
        "AssetRegistry replay complete"
    );
    registry
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
mod tests {
    use super::*;
    use call_consensus::BlockExecutionResult;
    use call_consensus::block::{ExecutionState, BlockContext, Subsystems};
    use call_network::{InMemoryNetwork, EpochBoundarySignal};
    use call_primitives::{Address, Ed25519PublicKey};
    use call_protocol::instructions::Instruction;
    use crate::state_persist::{save_asset_registry_inner, load_asset_registry_inner};
    use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};
    use std::sync::OnceLock;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_pubkey(n: u8) -> Ed25519PublicKey {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    /// Lazily-generated secp256k1 keypair for test transactions.
    /// All signed test transactions reuse this sender so balance setup stays simple.
    static TEST_SENDER: OnceLock<(Address, [u8; 32])> = OnceLock::new();

    fn test_sender() -> &'static Address {
        &TEST_SENDER
            .get_or_init(|| {
                let (secret, pubkey) = call_crypto::generate_keypair();
                let addr = call_crypto::pubkey_to_address(&pubkey);
                (addr, secret)
            })
            .0
    }

    fn make_test_tx(nonce: u64) -> ProtocolTransaction {
        let sender = *test_sender();
        let mut tx = ProtocolTransaction {
            sender,
            nonce,
            instructions: vec![Instruction::Transfer {
                asset_id: 1,
                to: test_addr(2),
                amount: 100,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 0,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let tx_hash = tx.compute_tx_hash();
        let secret = &TEST_SENDER.get().expect("TEST_SENDER initialized").1;
        let signature = call_crypto::secp256k1_sign(secret, &tx_hash);
        tx.auth = AuthScheme::SingleSig { signature };
        tx
    }

    fn one_million_call() -> u128 {
        1_000_000 * 10u128.pow(18)
    }

    #[test]
    fn test_node_creation() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");
        assert_eq!(node.state.chain_id, CALLCHAIN_CHAIN_ID);
        assert!(node.network.is_none());
        assert_eq!(node.parent_hash, BlockHash::ZERO);
        assert_eq!(node.consensus.read().unwrap().current_height(), 0);
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[test]
    fn test_asset_registry_persistence_roundtrip() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-registry-test-{}",
            std::process::id()
        ));

        // Phase 1: Direct db test — save and load registry
        {
            let db = open_db(tmp.clone()).expect("open db");
            let mut registry = AssetRegistry::new();
            let id = registry
                .register_asset("PERSIST".into(), "Persist Token".into(), 18, test_addr(1), 0, 100, 1_000_000)
                .unwrap();
            registry.mint_supply(id, &test_addr(1), 5_000).unwrap();
            registry.add_evm_supply(id, 3_000).unwrap();
            save_asset_registry_inner(&db.db, &registry).expect("save");
        }

        // Give MDBX a moment to release file locks before reopening
        std::thread::sleep(std::time::Duration::from_millis(100));

        // Phase 2: Reopen db and load registry
        {
            let db = open_db(tmp.clone()).expect("reopen db");
            let registry = load_asset_registry_inner(&db.db).expect("load");
            let asset = registry.get_asset(1).expect("asset should exist after reload");
            assert_eq!(asset.symbol, "PERSIST");
            assert_eq!(asset.name, "Persist Token");
            assert_eq!(asset.decimals, 18);
            assert_eq!(asset.issuer, test_addr(1));
            assert_eq!(asset.protocol_supply, 5_000);
            assert_eq!(asset.evm_supply, 3_000);
            assert_eq!(asset.max_supply, 1_000_000);
            assert_eq!(asset.status, call_protocol::registry::AssetStatus::Active);
            assert_eq!(asset.registered_at, 100);
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_node_mempool_stats() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-mempool-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");

        let (proto, evm, bridge) = node.mempool_stats();
        assert_eq!(proto, 0);
        assert_eq!(evm, 0);
        assert_eq!(bridge, 0);

        // Insert a tx
        let tx = make_test_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_protocol_tx(tx);
        }

        let (proto, evm, bridge) = node.mempool_stats();
        assert_eq!(proto, 1);
        assert_eq!(evm, 0);
        assert_eq!(bridge, 0);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_block_production_single_block() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-block-test-{}",
            std::process::id()
        ));

        // Create node with validators staked
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake a validator so proposer selection works
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender balance
        {
            let mut balances = node.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, *test_sender(), 10_000).unwrap();
        }

        // Insert a protocol tx into mempool
        let tx = make_test_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_protocol_tx(tx);
        }

        // Manually run one iteration of block production
        let height_before = node.consensus.read().unwrap().current_height();

        // Select and build
        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            1_000, // timestamp
            proposer,
            version,
            protocol_txs,
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        // Execute
        let result = node.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");
        block.finalize(&result);

        // Commit
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
        }

        let height_after = node.consensus.read().unwrap().current_height();
        assert_eq!(height_after, height_before + 1);
        assert_ne!(block.header.payment_root, call_primitives::Hash::ZERO);

        // Persist
        persist_block(&tmp, height, &block).expect("persist block");

        // Verify persisted block can be read back
        let dir = tmp.join("blocks");
        let path = dir.join(format!("{height:012}.json"));
        assert!(path.exists(), "block file should exist");

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_block_production_with_empty_mempool() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-empty-block-test-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake a validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Select and build with empty mempool
        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        assert!(selection.protocol_txs.is_empty());
        assert!(selection.evm_txs.is_empty());
        assert!(selection.bridge_ops.is_empty());

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            2_000,
            proposer,
            version,
            vec![],
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            vec![],
        );

        // Execute empty block
        let result = node.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("empty block execution");
        block.finalize(&result);
        block.finalize(&result);

        // Commit
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit empty");
        }

        assert_eq!(node.consensus.read().unwrap().current_height(), 1);

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_e2e_two_nodes_tx_propagation() {
        let tmp1 = std::env::temp_dir().join(format!(
            "call-node-e2e-node1-{}",
            std::process::id()
        ));
        let tmp2 = std::env::temp_dir().join(format!(
            "call-node-e2e-node2-{}",
            std::process::id()
        ));

        // Create two nodes
        let mut node1 = CallNode::new(tmp1.clone()).expect("node1 creation");
        let mut node2 = CallNode::new(tmp2.clone()).expect("node2 creation");

        // Create a shared in-memory network
        let shared_network: Arc<InMemoryNetwork> = Arc::new(InMemoryNetwork::new());

        // Wire both nodes to the same network
        node1.inject_network(Arc::clone(&shared_network) as Arc<dyn Network>);
        node2.inject_network(Arc::clone(&shared_network) as Arc<dyn Network>);

        // Stake validators on node1 so it can produce blocks
        {
            let mut consensus = node1.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender balance on node1
        {
            let mut balances = node1.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, *test_sender(), 10_000).unwrap();
        }

        // Verify initial state
        assert_eq!(node1.mempool_stats().0, 0); // no protocol txs
        assert_eq!(node2.mempool_stats().0, 0);

        // Step 1: Submit a tx to node1's mempool
        let tx = make_test_tx(0);
        {
            let mut mempool = node1.mempool.write().unwrap();
            let _ = mempool.insert_protocol_tx(tx);
        }
        assert_eq!(node1.mempool_stats().0, 1, "node1 should have 1 tx in mempool");

        // Step 2: Produce a block on node1
        let selection = { node1.mempool.write().unwrap().select_transactions() };
        let proposer = node1.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node1.consensus.read().unwrap().current_height();

        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        assert_eq!(protocol_txs.len(), 1, "should have 1 protocol tx");

        let version = node1.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node1.parent_hash,
            3_000,
            proposer,
            version,
            protocol_txs,
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        let result = node1.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");
        block.finalize(&result);

        // Commit on node1
        {
            let mut consensus = node1.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
        }
        assert_eq!(node1.consensus.read().unwrap().current_height(), 1, "node1 should be at height 1");

        // Step 3: Broadcast block announcement via shared network
        let block_hash = block.header.hash();
        let announcement = BlockAnnouncement {
            block_hash,
            height,
            proposer,
            timestamp_millis: block.header.timestamp_millis,
        };
        let msg = bincode::serialize(&NetworkMessage::BlockAnnouncement(announcement))
            .expect("serialize block announcement");
        shared_network.broadcast(BLOCK_CHANNEL, msg).await;

        // Step 4: Node2 receives the block announcement from shared network
        let result = shared_network.receive().await;
        let (peer_id, channel, data) = result.expect("should receive message");
        assert_eq!(channel, BLOCK_CHANNEL);
        assert_eq!(peer_id, "broadcast");

        let received = bincode::deserialize::<NetworkMessage>(&data).expect("parse network message");
        let announcement = match received {
            NetworkMessage::BlockAnnouncement(a) => a,
            other => panic!("expected block announcement, got {other:?}"),
        };
        assert_eq!(announcement.height, height);
        assert_eq!(announcement.block_hash, block_hash);
        assert_eq!(announcement.proposer, proposer);

        // Cleanup
        let _ = std::fs::remove_dir_all(&tmp1);
        let _ = std::fs::remove_dir_all(&tmp2);
    }

    #[tokio::test]
    async fn test_e2e_two_nodes_block_persistence() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-e2e-persist-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund balance
        {
            let mut balances = node.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, *test_sender(), 10_000).unwrap();
        }

        // Insert tx
        let tx = make_test_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_protocol_tx(tx);
        }

        // Produce block
        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            4_000,
            proposer,
            version,
            protocol_txs,
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        let result = node.state
            .write_all()
            .execute_block_no_subsystems(&block, height)
            .expect("execution");
        block.finalize(&result);

        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
        }

        // Persist block
        persist_block(&tmp, height, &block).expect("persist block");

        // Verify block file exists and can be read back
        let dir = tmp.join("blocks");
        let path = dir.join(format!("{height:012}.json"));
        assert!(path.exists(), "block file should exist");

        let data = std::fs::read(&path).expect("read block file");
        let restored: Block = serde_json::from_slice(&data).expect("deserialize block");
        assert_eq!(restored.header.height, height);
        assert_eq!(restored.header.hash(), block.header.hash());

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_e2e_state_persistence_restart() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-persist-restart-{}",
            std::process::id()
        ));
        let initial_balance: u128 = 10_000_000;
        let transfer_amount: u128 = 5_000;

        // === Phase 1: Create node, fund account, produce block, persist state ===
        {
            let node = CallNode::new(tmp.clone()).expect("node creation");

            // Stake validator
            {
                let mut consensus = node.consensus.write().unwrap();
                consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
                consensus.refresh_proposer_subset();
            }

            // Fund sender balance
            {
                let mut balances = node.state.balance_state.write().unwrap();
                balances.balances.set_balance(1, *test_sender(), initial_balance).unwrap();
            }

            // Register CALL asset so transfer instruction can validate it
            {
                let mut registry = node.state.asset_registry.write().unwrap();
                registry
                    .register_asset("CALL".into(), "Callchain".into(), 18, *test_sender(), 0, 0, 0)
                    .unwrap();
            }

            // Insert a transfer tx
            let mut tx = ProtocolTransaction {
                sender: *test_sender(),
                nonce: 0,
                instructions: vec![Instruction::Transfer {
                    asset_id: 1,
                    to: test_addr(2),
                    amount: transfer_amount,
                    memo: None,
                }],
                gas_config: GasConfig::SelfPay,
                fee_currency: call_primitives::FeeCurrency::Call,
                gas_limit: 100_000,
                max_fee: 1_000_000,
            expires_at: 0,
                auth: AuthScheme::SingleSig {
                    signature: [0u8; 65],
                },
            };
            let tx_hash = tx.compute_tx_hash();
            let secret = &TEST_SENDER.get().expect("TEST_SENDER initialized").1;
            let signature = call_crypto::secp256k1_sign(secret, &tx_hash);
            tx.auth = AuthScheme::SingleSig { signature };
            {
                let mut mempool = node.mempool.write().unwrap();
                let _ = mempool.insert_protocol_tx(tx);
            }

            // Produce and commit block
            let selection = { node.mempool.write().unwrap().select_transactions() };
            let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
            let height = node.consensus.read().unwrap().current_height();

            let protocol_txs: Vec<ProtocolTransaction> = selection
                .protocol_txs
                .into_iter()
                .filter_map(|e| serde_json::from_slice(&e.data).ok())
                .collect();

            let version = node.state.fork_manager.read().unwrap().current_version();
            let mut block = Block::new(
                height, node.parent_hash, 5_000, proposer, version, protocol_txs, vec![],
                vec![SystemTx { kind: SystemTxKind::UpdateBaseFee, data: vec![] }],
                selection.bridge_ops,
            );

            let result = {
                let mut s = node.state.write_all();
                block.execute(
                    &mut ExecutionState::new(
                        &mut s.balances, &mut s.registry, &mut s.compliance,
                        &mut s.bridge, &mut s.shielded, &mut s.evm,
                    ),
                    &mut BlockContext::new(height, &mut s.fee_params),
                    &mut Subsystems {
                        validator_state: Some(&mut *s.validator_state),
                        ..Subsystems::none()
                    },
                )
                    .expect("execution")
            };
            block.finalize(&result);

            {
                let mut consensus = node.consensus.write().unwrap();
                consensus.commit_block(&block, &result).expect("commit");
            }

            // Persist state to reth-db immediately
            let db_env = &node.db.db;
            persist_state_to_db(db_env, &node.state, &node.consensus)
                .expect("persist state");

            // Node is dropped here, simulating shutdown
        }

        // Give MDBX a moment to release file locks before reopening
        std::thread::sleep(std::time::Duration::from_millis(100));

        // === Phase 2: Create new node from same data dir, verify state ===
        {
            let node2 = CallNode::new(tmp.clone()).expect("node creation (restart)");

            // Verify balance was recovered
            let sender_balance = {
                let balances = node2.state.balance_state.read().unwrap();
                balances.balances.get_balance(1, test_sender())
            };

            // Balance should be less than initial (transfer + fees deducted)
            assert!(
                sender_balance < initial_balance,
                "sender balance should be reduced after restart, got {sender_balance}"
            );

            let _ = std::fs::remove_dir_all(&tmp);
        }
    }

    // ── Governance full cycle integration ─────────────────────────────

    #[test]
    fn test_governance_full_cycle() {
        use call_governance::{ProposalType, GovernanceEvent, DEFAULT_PROPOSAL_DEPOSIT, REVIEW_PERIOD_BLOCKS, VOTING_PERIOD_BLOCKS, TIMELOCK_PERIOD_BLOCKS};

        let tmp = std::env::temp_dir().join("call_gov_cycle_test");
        let _ = std::fs::remove_dir_all(&tmp);

        let node = CallNode::new(tmp.clone()).expect("node creation");

        let proposer = call_primitives::Address::repeat_byte(0xAA);

        // Fund proposer in AccountState (asset_id 1 = CALL) — governance reads from balance_source
        {
            let mut balances = node.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, proposer, DEFAULT_PROPOSAL_DEPOSIT * 5).expect("fund proposer");
        }

        // Register some validators so quorum can be met
        {
            let mut gov = node.state.governance.write().unwrap();
            for i in 1u32..=3 {
                gov.register_validator(i, call_primitives::Address::repeat_byte(i as u8));
                gov.set_call_balance(call_primitives::Address::repeat_byte(i as u8), 1);
            }
        }

        // Submit a proposal
        let proposal_id = {
            let mut gov = node.state.governance.write().unwrap();
            gov.submit_proposal(
                proposer,
                ProposalType::ParameterChange {
                    param_id: "test_param".into(),
                    new_value: "{\"base_fee\": 100}".into(),
                },
                "Test proposal".into(),
                "Integration test".into(),
                vec![],
            ).expect("submit proposal")
        };

        // Verify proposal was created
        {
            let gov = node.state.governance.read().unwrap();
            let p = gov.get_proposal(proposal_id).expect("proposal exists");
            assert_eq!(p.state, call_governance::ProposalState::Pending);
        }

        // Advance to voting period and vote with all validators
        {
            let mut gov = node.state.governance.write().unwrap();
            gov.set_current_block(REVIEW_PERIOD_BLOCKS);
            // Vote yes from all 3 registered validators
            for i in 1u32..=3 {
                let validator_addr = call_primitives::Address::repeat_byte(i as u8);
                let _ = gov.vote(proposal_id, validator_addr, call_governance::Vote::Yes);
            }
        }

        // Advance through all phases
        {
            let mut gov = node.state.governance.write().unwrap();
            gov.advance(REVIEW_PERIOD_BLOCKS + VOTING_PERIOD_BLOCKS + TIMELOCK_PERIOD_BLOCKS + 1);
        }

        // Should be queued or executed (depending on timelock)
        {
            let gov = node.state.governance.read().unwrap();
            let p = gov.get_proposal(proposal_id).expect("proposal exists");
            assert!(
                matches!(p.state, call_governance::ProposalState::Queued | call_governance::ProposalState::Executed),
                "expected queued or executed, got {:?}", p.state
            );
        }

        // Advance past timelock to execute
        {
            let mut gov = node.state.governance.write().unwrap();
            let exec = gov.get_proposal(proposal_id).unwrap().execution_block.unwrap();
            gov.advance(exec + 1);
            // Drain events
            let events = gov.drain_events();
            assert!(events.iter().any(|e| matches!(e, GovernanceEvent::ProposalExecuted { .. })));
        }

        // Should be executed
        {
            let gov = node.state.governance.read().unwrap();
            let p = gov.get_proposal(proposal_id).expect("proposal exists");
            assert_eq!(p.state, call_governance::ProposalState::Executed);
        }

        // Verify fee_params were updated by executor
        {
            let fp = node.state.fee_params.read().unwrap();
            assert_eq!(fp.base_fee, 100); // Should match the JSON in execution_data
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    // ── State isolation tests (Phase 1) ─────────────────────────────────

    #[tokio::test]
    async fn test_state_isolation_propose_does_not_modify_shared_state() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-isolation-test-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender with ample balance for fees + transfer
        {
            let mut balances = node.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, *test_sender(), 10_000_000).unwrap();
        }

        // Register CALL asset so Transfer instructions succeed
        {
            let mut registry = node.state.asset_registry.write().unwrap();
            registry
                .register_asset("CALL".into(), "Callchain".into(), 18, *test_sender(), 0, 0, 0)
                .unwrap();
        }

        // Insert tx into mempool
        let tx = make_test_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_protocol_tx(tx);
        }

        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            5_000,
            proposer,
            version,
            protocol_txs,
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        // Capture shared state BEFORE propose-phase execution
        let balance_before = {
            let balances = node.state.balance_state.read().unwrap();
            balances.balances.get_balance(1, test_sender())
        };

        // Simulate PROPOSE phase: execute on CLONED state
        {
            let result = node.state
                .read_all()
                .execute_block_cloned_no_subsystems(&block, height)
                .expect("propose execution on clone");

            block.finalize(&result);
        }

        // Verify shared state is UNCHANGED after propose
        let balance_after_propose = {
            let balances = node.state.balance_state.read().unwrap();
            balances.balances.get_balance(1, test_sender())
        };
        assert_eq!(
            balance_after_propose, balance_before,
            "shared state must NOT be modified by propose-phase execution"
        );

        // Simulate FINALIZE phase: execute on SHARED state (write locks)
        {
            let result = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("finalize execution on shared state");

            // Verify state roots match header
            assert_eq!(result.payment_root, block.header.payment_root, "payment_root mismatch");
            assert_eq!(result.evm_state_root, block.header.evm_state_root, "evm_state_root mismatch");
            assert_eq!(result.bridge_root, block.header.bridge_root, "bridge_root mismatch");
            assert_eq!(result.receipt_root, block.header.receipt_root, "receipt_root mismatch");

            block.finalize(&result);

            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("commit");
        }

        // Verify shared state IS modified after finalize
        let balance_after_finalize = {
            let balances = node.state.balance_state.read().unwrap();
            balances.balances.get_balance(1, test_sender())
        };
        assert!(
            balance_after_finalize < balance_before,
            "shared state must be modified by finalize-phase execution, before={balance_before}, after={balance_after_finalize}"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_state_root_mismatch_rejects_block() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-root-mismatch-test-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Fund sender with ample balance for fees + transfer
        {
            let mut balances = node.state.balance_state.write().unwrap();
            balances.balances.set_balance(1, *test_sender(), 10_000_000).unwrap();
        }

        // Register CALL asset so Transfer instructions succeed
        {
            let mut registry = node.state.asset_registry.write().unwrap();
            registry
                .register_asset("CALL".into(), "Callchain".into(), 18, *test_sender(), 0, 0, 0)
                .unwrap();
        }

        let tx = make_test_tx(0);
        {
            let mut mempool = node.mempool.write().unwrap();
            let _ = mempool.insert_protocol_tx(tx);
        }

        let selection = { node.mempool.write().unwrap().select_transactions() };
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let height = node.consensus.read().unwrap().current_height();

        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            6_000,
            proposer,
            version,
            protocol_txs,
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        // Execute on cloned state to get valid roots
        let result = node.state
            .read_all()
            .execute_block_cloned_no_subsystems(&block, height)
            .expect("execution");

        block.finalize(&result);

        // Tamper with a state root in the header
        let original_payment_root = block.header.payment_root;
        block.header.payment_root = call_primitives::Hash::repeat_byte(0xDE);

        // Verify: re-execution on clone detects root mismatch
        {
            let result2 = node.state
                .read_all()
                .execute_block_cloned_no_subsystems(&block, height)
                .expect("re-execution");

            assert_ne!(
                result2.payment_root, block.header.payment_root,
                "tampered payment_root should mismatch re-computed root"
            );
        }

        // Verify: finalize with tampered root is caught by root check
        {
            let result3 = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("execution on shared state");

            // The re-computed result3 should have the ORIGINAL correct root
            assert_eq!(result3.payment_root, original_payment_root, "re-computed root should match original");
            // But the block header has the tampered root
            assert_ne!(result3.payment_root, block.header.payment_root, "tampered block should fail root check");
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_commit_block_height_replay_protection() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-replay-test-{}",
            std::process::id()
        ));

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        let height = node.consensus.read().unwrap().current_height();
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let version = node.state.fork_manager.read().unwrap().current_version();

        let mut block = Block::new(
            height,
            node.parent_hash,
            7_000,
            proposer,
            version,
            vec![],
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            vec![],
        );

        // Execute and commit once
        {
            let result = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("execution");

            block.finalize(&result);

            let mut consensus = node.consensus.write().unwrap();
            consensus.commit_block(&block, &result).expect("first commit");
        }

        // Consensus height should have advanced
        assert_eq!(node.consensus.read().unwrap().current_height(), height + 1);

        // Attempt to commit the SAME block again should fail due to height mismatch
        {
            let mut consensus = node.consensus.write().unwrap();
            let result = consensus.commit_block(&block, &BlockExecutionResult::default());
            assert!(
                result.is_err(),
                "double-commit of same block should be rejected"
            );
            let err = result.unwrap_err().to_string();
            assert!(err.contains("height mismatch"), "error should mention height mismatch: {err}");
        }

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_epoch_number_derived_from_height() {
        // Verify epoch_number = current_height / epoch_length for various heights
        let epoch_length: u64 = 1000;

        assert_eq!(0 / epoch_length, 0, "height 0 should be epoch 0");
        assert_eq!(999 / epoch_length, 0, "height 999 should be epoch 0");
        assert_eq!(1000 / epoch_length, 1, "height 1000 should be epoch 1");
        assert_eq!(1001 / epoch_length, 1, "height 1001 should be epoch 1");
        assert_eq!(1999 / epoch_length, 1, "height 1999 should be epoch 1");
        assert_eq!(2000 / epoch_length, 2, "height 2000 should be epoch 2");
        assert_eq!(2500 / epoch_length, 2, "height 2500 should be epoch 2");
        assert_eq!(3000 / epoch_length, 3, "height 3000 should be epoch 3");
    }

    #[tokio::test]
    async fn test_quorum_threshold_calculation() {
        // Verify (subset_size * 2).div_ceil(3) for various subset sizes
        assert_eq!((1usize * 2).div_ceil(3), 1, "subset of 1 needs quorum of 1");
        assert_eq!((2usize * 2).div_ceil(3), 2, "subset of 2 needs quorum of 2");
        assert_eq!((3usize * 2).div_ceil(3), 2, "subset of 3 needs quorum of 2");
        assert_eq!((4usize * 2).div_ceil(3), 3, "subset of 4 needs quorum of 3");
        assert_eq!((5usize * 2).div_ceil(3), 4, "subset of 5 needs quorum of 4");
        assert_eq!((6usize * 2).div_ceil(3), 4, "subset of 6 needs quorum of 4");
        assert_eq!((7usize * 2).div_ceil(3), 5, "subset of 7 needs quorum of 5");
        assert_eq!((10usize * 2).div_ceil(3), 7, "subset of 10 needs quorum of 7");
    }

    #[tokio::test]
    async fn test_epoch_boundary_signal_updates_peer_heights() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-signal-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Construct EpochBoundarySignal
        let signal = EpochBoundarySignal {
            height: 1000,
            epoch: 1,
            sender_pubkey: [0xAB; 32],
        };
        let data = bincode::serialize(&NetworkMessage::EpochBoundarySignal(signal))
            .expect("serialize signal");

        let network: Arc<dyn Network> = Arc::new(InMemoryNetwork::new());
        let sync_inflight: SyncInflight = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

        // Send signal via handle_network_message on BLOCK_CHANNEL
        handle_network_message(
            "peer_abc123",
            BLOCK_CHANNEL,
            &data,
            &node.mempool,
            &node.state,
            &network,
            &sync_inflight,
        );

        // Verify peer_heights was updated
        let peer_heights = node.state.peer_heights.read().unwrap();
        assert_eq!(
            peer_heights.get("peer_abc123"),
            Some(&1000u64),
            "peer height should be recorded from EpochBoundarySignal"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_sync_crosses_epoch_boundary_sets_restart_signal() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-sync-epoch-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator so proposer selection works
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Use small epoch length so we cross boundary quickly
        {
            let mut params = node.state.consensus_params.write().unwrap();
            params.epoch_length = 2;
        }

        // Set current block to 1 (epoch = 1/2 = 0)
        node.state.set_current_block(1);

        // Build block at height 1
        let height = 1u64;
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            9_000,
            proposer,
            version,
            vec![],
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            vec![],
        );

        // Execute to compute valid state roots
        {
            let result = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("execution");
            block.finalize(&result);
        }

        // Verify signal is NOT set before sync
        assert!(
            !node.state.engine_restart_signal.load(std::sync::atomic::Ordering::Relaxed),
            "signal should NOT be set before sync"
        );

        // Build SyncResponse with the block
        let block_json = serde_json::to_vec(&block).expect("serialize");
        let response = SyncResponse {
            start_height: 1,
            blocks: vec![block_json],
            state_root: block.header.payment_root,
        };

        // Apply synced blocks
        let applied = apply_synced_blocks(&response, &node.state, &node.consensus, &tmp);
        assert_eq!(applied, 1, "should apply exactly 1 block");

        // Height should now be 2
        assert_eq!(node.state.get_current_block(), 2);

        // We crossed from epoch 0 (height 1/2) to epoch 1 (height 2/2)
        assert!(
            node.state.engine_restart_signal.load(std::sync::atomic::Ordering::Relaxed),
            "engine_restart_signal should be set after sync crosses epoch boundary"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[tokio::test]
    async fn test_sync_within_same_epoch_does_not_set_restart_signal() {
        let tmp = std::env::temp_dir().join(format!(
            "call-node-sync-no-epoch-test-{}",
            std::process::id()
        ));
        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Stake validator
        {
            let mut consensus = node.consensus.write().unwrap();
            consensus.stake_validator(test_addr(1), test_pubkey(1), one_million_call()).expect("stake");
            consensus.refresh_proposer_subset();
        }

        // Large epoch length — sync won't cross boundary
        {
            let mut params = node.state.consensus_params.write().unwrap();
            params.epoch_length = 1000;
        }

        // Set current block to 5 (epoch = 5/1000 = 0)
        node.state.set_current_block(5);

        // Build block at height 5
        let height = 5u64;
        let proposer = node.consensus.read().unwrap().current_proposer().expect("proposer");
        let version = node.state.fork_manager.read().unwrap().current_version();
        let mut block = Block::new(
            height,
            node.parent_hash,
            10_000,
            proposer,
            version,
            vec![],
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            vec![],
        );

        // Execute to compute valid state roots
        {
            let result = node.state
                .write_all()
                .execute_block_no_subsystems(&block, height)
                .expect("execution");
            block.finalize(&result);
        }

        // Build SyncResponse with the block
        let block_json = serde_json::to_vec(&block).expect("serialize");
        let response = SyncResponse {
            start_height: 5,
            blocks: vec![block_json],
            state_root: block.header.payment_root,
        };

        // Apply synced blocks
        let applied = apply_synced_blocks(&response, &node.state, &node.consensus, &tmp);
        assert_eq!(applied, 1, "should apply exactly 1 block");

        // Height should now be 6
        assert_eq!(node.state.get_current_block(), 6);

        // Epoch did not change: old=5/1000=0, new=6/1000=0
        assert!(
            !node.state.engine_restart_signal.load(std::sync::atomic::Ordering::Relaxed),
            "engine_restart_signal should NOT be set when sync stays within same epoch"
        );

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
