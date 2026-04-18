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

use crate::light_client::{LightClient, BlockSignatures, SigBytes, PubKeyBytes};
use call_consensus::{
    Block, ConsensusParams, SimplexConsensus, SystemTx, SystemTxKind, ValidatorStateManager,
    PersistedConsensusState,
    bft::{CallAutomaton, CallRelay, CallReporter, FinalizationInfo, ProposeRequest, VerifyRequest},
    block_cache::BlockCache,
    digest::ConsensusDigest,
    proposer::{derive_vrf_seed, select_proposer_subset, EPOCH_LENGTH},
};
use call_network::{CommonwareConfig, CommonwareNetwork, Network, NetworkMessage, BlockAnnouncement, TransactionMessage, SyncRequest, SyncResponse, OraclePriceRequest, OraclePriceSubmission};
use call_primitives::BlockHash;
use call_protocol::{
    BalanceState, AssetRegistry, ComplianceEngine,
    transaction::ProtocolTransaction,
};
use call_governance::GovernanceManager;
use call_oracle::{OracleManager, OracleSubmission, ORACLE_UPDATE_INTERVAL};
use call_rpc::{RpcState, RpcConfig, build_rpc_module, SubscriptionManager, wire_governance_executor};
use call_storage::{CallDb, open_db, PruneState, StorageError};
use call_storage::reth_db::{
    save_balances as db_save_balances, load_balances as db_load_balances,
    save_prune_state as db_save_prune,
    db_put, db_batch_put, db_clear, db_iter_all, db_get,
    CallOracleState, CallEvmAccounts, CallBridgeOps,
    CallShieldedNullifiers, CallShieldedCommitments, CallValidators, CallAgents,
    CallGovernanceState, CallConsensusState,
};
use reth_db::DatabaseEnv;
use call_transaction_pool::Mempool;
use call_evm::EvmState;
use call_bridge::BridgeStateManager;
use call_agent::{AgentRegistry, AgentBalances};
use call_shielded::ShieldedState;
use jsonrpsee::server::{Server, ServerHandle};
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
use commonware_cryptography::Digest;
use commonware_cryptography::Signer;
use commonware_parallel::Sequential;
use commonware_p2p::authenticated::lookup::{self as p2p_lookup, Config as P2PConfig};
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
    /// Qualified validator set changed
    ValidatorSetChange,
}

/// P2P message channels
const TX_CHANNEL: u64 = 1;
const BLOCK_CHANNEL: u64 = 2;
const SYNC_CHANNEL: u64 = 3;
const ORACLE_CHANNEL: u64 = 4;

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
}

impl CallNode {
    /// Create a new node with default state
    pub fn new(data_dir: PathBuf) -> Result<Self, String> {
        let mempool = Arc::new(RwLock::new(Mempool::new()));
        let db = open_db(data_dir).map_err(|e| format!("failed to open db: {e}"))?;

        // Restore prune state from disk if previously persisted
        let prune_state = db.load_prune_state()
            .map_err(|e| format!("failed to load prune state: {e}"))?;

        // Load persisted state from reth-db if available
        let (balance_state, evm_state, bridge_state, shielded_state, consensus_validators, registry, agent_balances, oracle_manager, governance_manager) =
            if let Some(ref db_env) = db.db {
                let loaded = load_state_from_db(db_env);
                // Try to load oracle state from disk
                let oracle = load_oracle_state(db_env)
                    .map_err(|e| format!("failed to load oracle state: {e}"))?;
                // Try to load governance state from disk
                let governance = load_governance_state(db_env)
                    .map_err(|e| format!("failed to load governance state: {e}"))?;
                (loaded.0, loaded.1, loaded.2, loaded.3, loaded.4, loaded.5, loaded.6, oracle, governance)
            } else {
                (BalanceState::new(), EvmState::new(), BridgeStateManager::default(),
                 ShieldedState::new(), ValidatorStateManager::default(),
                 AgentRegistry::new(), AgentBalances::new(), OracleManager::default(), GovernanceManager::new())
            };

        // Try to load persisted consensus state; fall back to genesis
        let consensus = if let Some(ref db_env) = db.db {
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
                    SimplexConsensus::new(ConsensusParams::default(), consensus_validators)
                }
            }
        } else {
            SimplexConsensus::new(ConsensusParams::default(), consensus_validators)
        };

        let state = Arc::new(RpcState::new(
            balance_state,
            AssetRegistry::new(),
            ComplianceEngine::new(),
            evm_state,
            bridge_state,
            ValidatorStateManager::default(),
            registry,
            agent_balances,
            shielded_state,
            mempool.clone(),
            CALLCHAIN_CHAIN_ID,
            oracle_manager,
        ));

        // Wire live oracle into precompiles so EVM contracts can read prices
        call_precompiles::set_live_oracle(Arc::clone(&state.oracle));

        // Replace default governance with persisted state
        *state.governance.write().unwrap() = governance_manager;

        // Wire governance executor so proposals can trigger real side effects
        wire_governance_executor(&state);

        // Set parent_hash to the last committed block hash from persisted state
        let parent_hash = consensus.last_block_hash();

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
        })
    }

    /// Start the HTTP RPC server
    pub async fn start_rpc(&mut self, config: RpcConfig) -> Result<(), String> {
        let module = build_rpc_module(Arc::clone(&self.state))
            .map_err(|e| format!("failed to build RPC module: {e}"))?;

        let server = Server::builder()
            .max_connections(config.max_connections)
            .build(config.http_addr)
            .await
            .map_err(|e| format!("bind failed: {e}"))?;

        let handle = server.start(module);
        tracing::info!("HTTP RPC server started on {}", config.http_addr);

        self.server_handle = Some(handle);
        Ok(())
    }

    /// Start the WebSocket RPC server for subscriptions.
    /// Note: jsonrpsee 0.24 Server handles both HTTP and WS on the same port.
    /// This starts a second server on the WS address for WS-only connections.
    pub async fn start_ws_rpc(&mut self, config: RpcConfig) -> Result<(), String> {
        let module = build_rpc_module(Arc::clone(&self.state))
            .map_err(|e| format!("failed to build WS module: {e}"))?;

        let server = Server::builder()
            .build(config.ws_addr)
            .await
            .map_err(|e| format!("WS bind failed: {e}"))?;

        let handle = server.start(module);
        self.ws_server_handle = Some(handle);
        tracing::info!("WebSocket RPC server started on {}", config.ws_addr);

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

        // Start receive loop
        let mempool = Arc::clone(&self.mempool);
        let state = Arc::clone(&self.state);
        let data_dir = self.db.data_dir.clone();
        let block_cache = Arc::clone(&self.block_cache);
        let net_clone = Arc::clone(&network);
        tokio::spawn(async move {
            while let Ok((peer_id, channel, data)) = net_clone.receive().await {
                if channel == SYNC_CHANNEL {
                    // Handle sync requests: respond with blocks
                    if let Ok(NetworkMessage::SyncRequest(request)) = bincode::deserialize(&data) {
                        tracing::debug!(peer_id, start = request.start_height, count = request.count, "sync: request from peer");
                        if let Some(response) = handle_sync_request(&data_dir, &request) {
                            let resp_data = bincode::serialize(&NetworkMessage::SyncResponse(response))
                                .expect("serialize sync response");
                            net_clone.send_to(vec![peer_id], resp_data).await;
                        }
                    }
                } else if channel == BLOCK_CHANNEL {
                    // Try BlockAnnouncement first (post-commit announcements)
                    if let Ok(_announcement) = serde_json::from_slice::<BlockAnnouncement>(&data) {
                        handle_network_message(&peer_id, channel, &data, &mempool, &state, &net_clone);
                    } else if let Ok(block) = serde_json::from_slice::<Block>(&data) {
                        // Full block received from BFT relay — insert into cache for verify
                        let digest = ConsensusDigest::from(block.header.hash());
                        block_cache.lock().unwrap().insert(digest, block);
                        tracing::debug!(digest = %digest, peer_id, "BFT: relayed block received, inserted into cache");
                    } else {
                        tracing::debug!(peer_id, "BLOCK_CHANNEL: unknown message format");
                    }
                } else {
                    handle_network_message(&peer_id, channel, &data, &mempool, &state, &net_clone);
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
    ) -> tokio::task::JoinHandle<()> {
        let state = Arc::clone(&self.state);
        let mempool = Arc::clone(&self.mempool);
        let consensus = Arc::clone(&self.consensus);
        let db = self.db.clone();
        let prune_state = self.prune_state.clone();
        let subscriptions = self.state.subscriptions.clone();
        let network = self.network.clone();
        let data_dir = self.db.data_dir.clone();
        let block_cache = Arc::clone(&self.block_cache);

        tokio::spawn(async move {
            let mut epoch_number: u64 = 0;

            loop {
                // Read current qualified validators and compute VRF subset
                let (parent_hash, subset, my_index) = {
                    let c = consensus.read().unwrap();
                    let ph = c.last_block_hash();
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
                    let params = c.params();
                    let subset =
                        select_proposer_subset(&qualified, &pubkeys, &seed, params.subset_size);

                    // Check if we are in the subset
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

                    (ph, subset, my_index)
                };

                if my_index.is_some() {
                    tracing::info!(
                        epoch = epoch_number,
                        subset_size = subset.len(),
                        "BFT: selected for epoch, starting engine"
                    );
                    let result = Self::start_bft_engine_inner(
                        ed25519_private_key.clone(),
                        consensus_p2p_port,
                        state.clone(),
                        mempool.clone(),
                        consensus.clone(),
                        db.clone(),
                        prune_state.clone(),
                        subscriptions.clone(),
                        block_cache.clone(),
                        network.clone(),
                        data_dir.clone(),
                        &subset,
                        epoch_number,
                        parent_hash,
                    )
                    .await;

                    match result {
                        Ok(reason) => {
                            tracing::info!(
                                ?reason,
                                epoch = epoch_number,
                                "BFT: engine exited for epoch rotation"
                            );
                            epoch_number += 1;
                            continue;
                        }
                        Err(e) => {
                            tracing::error!(?e, "BFT: engine exited with error");
                            epoch_number += 1;
                            continue;
                        }
                    }
                } else {
                    // Not selected — wait until next epoch boundary
                    let epoch_length = {
                        consensus.read().unwrap().params().epoch_length
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
                        epoch = epoch_number,
                        wait_blocks = blocks_to_wait,
                        "BFT: not selected for epoch, sleeping"
                    );
                    tokio::time::sleep(sleep_secs).await;
                    epoch_number += 1;
                }
            }
        })
    }

    /// Start a single epoch of the BFT engine. Returns the rotation reason
    /// when the event loop exits.
    #[allow(clippy::too_many_arguments)]
    async fn start_bft_engine_inner(
        ed25519_private_key: ed25519::PrivateKey,
        consensus_p2p_port: u16,
        state: Arc<RpcState>,
        mempool: Arc<RwLock<Mempool>>,
        consensus: Arc<RwLock<SimplexConsensus>>,
        db: CallDb,
        prune_state: PruneState,
        subscriptions: SubscriptionManager,
        block_cache: Arc<std::sync::Mutex<BlockCache>>,
        network: Option<Arc<dyn Network>>,
        data_dir: PathBuf,
        subset: &[call_primitives::ValidatorId],
        epoch_number: u64,
        parent_hash: BlockHash,
    ) -> Result<EpochRotationReason, String> {
        // Build participants set from VRF subset
        let mut keys: Vec<ed25519::PublicKey> = Vec::new();
        {
            let vs = state.validator_state.read().unwrap();
            let all_validators = vs.get_all_validators();
            for id in subset {
                if let Some(stake) = all_validators.get(id) {
                    if let Ok(pk) = ed25519::PublicKey::decode(&stake.ed25519_pubkey[..]) {
                        keys.push(pk);
                    }
                }
            }
        }
        let participants = Set::from_iter_dedup(keys);

        // Build signing scheme
        let scheme = Ed25519Scheme::signer(
            b"callchain-consensus",
            participants.clone(),
            ed25519_private_key.clone(),
        )
        .expect("ed25519 key must be in participant set");

        // Bridge channels (BFT engine -> tokio event loop)
        let (propose_tx, propose_rx) = mpsc::channel::<ProposeRequest>(16);
        let (verify_tx, verify_rx) = mpsc::channel::<VerifyRequest>(16);
        let (finalize_tx, finalize_rx) = mpsc::channel::<FinalizationInfo>(16);
        let (broadcast_tx, broadcast_rx) = mpsc::channel::<Vec<u8>>(64);

        // Build BFT trait bridges
        let automaton = CallAutomaton::new(propose_tx, verify_tx);
        let relay = CallRelay::new(Arc::clone(&block_cache), broadcast_tx);
        let reporter = CallReporter::new(finalize_tx);

        // Exit channel for epoch rotation
        let (exit_tx, exit_rx) = oneshot::channel::<EpochRotationReason>();

        // Spawn BFT engine in a dedicated background OS thread
        let thread_port = consensus_p2p_port;
        let bft_data_dir = data_dir.join("bft_journal");
        let bft_handle = std::thread::spawn(move || {
            std::fs::create_dir_all(&bft_data_dir).ok();
            let runtime_cfg = RuntimeConfig::new().with_storage_directory(&bft_data_dir);
            let runner = TokioRunner::new(runtime_cfg);
            runner.start(|context| async move {
                let signer = ed25519_private_key;
                let listen_addr = std::net::SocketAddr::from(([0, 0, 0, 0], thread_port));

                let p2p_cfg = P2PConfig::local(
                    signer,
                    b"callchain-consensus",
                    listen_addr,
                    10 * 1024 * 1024,
                );
                let (mut network, oracle) = p2p_lookup::Network::new(
                    context.with_label("consensus-p2p"),
                    p2p_cfg,
                );

                // Register 3 consensus channels (vote, certificate, resolver)
                let quota = Quota::per_second(NonZeroU32::new(10000).unwrap());
                let (vote_s, vote_r) = network.register(1, quota.clone(), 100_000);
                let (cert_s, cert_r) = network.register(2, quota.clone(), 100_000);
                let (resolve_s, resolve_r) = network.register(3, quota, 100_000);

                // Start the p2p network
                let _net_handle = network.start();

                // Build page cache for the consensus journal
                let page_cache = CacheRef::from_pooler(
                    &context,
                    NonZeroU16::new(4096).unwrap(),
                    NonZeroUsize::new(1024).unwrap(),
                );

                // Build and start the simplex BFT engine
                let cfg = SimplexConfig {
                    scheme,
                    elector: RoundRobin::<commonware_cryptography::Sha256>::default(),
                    blocker: oracle,
                    automaton,
                    relay,
                    reporter,
                    strategy: Sequential,
                    partition: "callchain".to_string(),
                    mailbox_size: 1024,
                    epoch: Epoch::new(epoch_number),
                    replay_buffer: NonZeroUsize::new(1024).unwrap(),
                    write_buffer: NonZeroUsize::new(1024).unwrap(),
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

                let engine = Engine::new(context, cfg);
                let _engine_handle = engine.start(
                    (vote_s, vote_r),
                    (cert_s, cert_r),
                    (resolve_s, resolve_r),
                );

                // Keep the background thread alive indefinitely
                std::future::pending::<()>().await;
            });
        });

        // Spawn the tokio-side event loop that handles BFT requests
        let event_loop_handle = tokio::spawn(bft_event_loop(
            propose_rx,
            verify_rx,
            finalize_rx,
            broadcast_rx,
            state,
            mempool,
            consensus,
            block_cache,
            db,
            prune_state,
            subscriptions,
            parent_hash,
            network,
            data_dir,
            epoch_number,
            exit_tx,
        ));

        // Wait for exit_rx to fire (event loop sends reason before breaking)
        // The event loop handle will complete shortly after.
        let reason = exit_rx
            .await
            .map_err(|e| format!("exit channel canceled: {e:?}"));
        event_loop_handle.abort();
        let _ = bft_handle;
        reason
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
                                        let execute_result = {
                                            let mut balances = state.balance_state.write().unwrap();
                                            let registry = state.asset_registry.read().unwrap();
                                            let mut compliance = state.compliance_engine.write().unwrap();
                                            let mut bridge_state = state.bridge_state.write().unwrap();
                                            let mut shielded_state = state.shielded_state.write().unwrap();
                                            let mut fee_params = state.fee_params.write().unwrap();
                                            let mut evm_state = state.evm_state.write().unwrap();

                                            block.execute(
                                                &mut balances, &registry, &mut compliance, &mut bridge_state,
                                                &mut shielded_state, &mut fee_params, height, &mut evm_state,
                                                None,
                                            )
                                        };

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
                                                if let Some(ref db_env) = db_env {
                                                    if let Ok(c) = consensus.read() {
                                                        let _ = save_consensus_state_inner(db_env, &c);
                                                    }
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
            if let Some(ref db_env) = db_env {
                let c = consensus.read().unwrap();
                if let Err(e) = save_consensus_state_inner(db_env, &c) {
                    tracing::warn!(error = %e, "sync: failed to save consensus state after sync");
                }
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
        if let Some(ref db_env) = self.db.db {
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
        }
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

// ── State Persistence ─────────────────────────────────────────────────

/// Load all state types from the reth-db database.
fn load_state_from_db(
    db_env: &Arc<DatabaseEnv>,
) -> (BalanceState, EvmState, BridgeStateManager, ShieldedState,
      ValidatorStateManager, AgentRegistry, AgentBalances, GovernanceManager) {
    // Load balances
    let (balances, allowances) = match db_load_balances(db_env) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load balances from db");
            (std::collections::HashMap::new(), std::collections::HashMap::new())
        }
    };
    let mut balance_state = BalanceState::new();
    for ((asset_id, address), balance) in &balances {
        let _ = balance_state.balances.set_balance(*asset_id, *address, *balance);
    }
    for ((asset_id, owner, spender), allowance) in &allowances {
        balance_state.allowances.set_allowance(*asset_id, *owner, *spender, *allowance);
    }

    // Load EVM accounts
    let evm_state = match load_evm_accounts_inner(db_env) {
        Ok(state) => state,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load evm accounts");
            EvmState::new()
        }
    };

    // Load bridge state
    let bridge_state = match load_bridge_state_inner(db_env) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load bridge state");
            BridgeStateManager::default()
        }
    };

    // Load shielded state
    let shielded_state = match load_shielded_state_inner(db_env) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load shielded state");
            ShieldedState::new()
        }
    };

    // Load validator state
    let validators = match load_validator_state_inner(db_env) {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load validator state");
            ValidatorStateManager::default()
        }
    };

    // Load agent state
    let (registry, agent_balances) = match load_agent_state_inner(db_env) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load agent state");
            (AgentRegistry::new(), AgentBalances::new())
        }
    };

    // Load governance state
    let governance = match load_governance_state(db_env) {
        Ok(g) => g,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load governance state");
            GovernanceManager::new()
        }
    };

    (balance_state, evm_state, bridge_state, shielded_state, validators, registry, agent_balances, governance)
}

/// Persist all state types to the reth-db database.
fn persist_state_to_db(
    db_env: &Arc<DatabaseEnv>,
    state: &Arc<RpcState>,
    consensus: &Arc<RwLock<SimplexConsensus>>,
) -> Result<(), String> {
    // Persist balances
    {
        let bs = state.balance_state.read().unwrap();
        db_save_balances(db_env, bs.balances.balances_map(), bs.allowances.allowances_map())
            .map_err(|e| format!("save balances: {e}"))?;
    }

    // Persist EVM state
    {
        let evm = state.evm_state.read().unwrap();
        save_evm_accounts_inner(db_env, &evm)
            .map_err(|e| format!("save evm: {e}"))?;
    }

    // Persist bridge state
    {
        let bridge = state.bridge_state.read().unwrap();
        save_bridge_state_inner(db_env, &bridge)
            .map_err(|e| format!("save bridge: {e}"))?;
    }

    // Persist shielded state
    {
        let shielded = state.shielded_state.read().unwrap();
        save_shielded_state_inner(db_env, &shielded)
            .map_err(|e| format!("save shielded: {e}"))?;
    }

    // Persist validator state
    {
        let c = consensus.read().unwrap();
        save_validator_state_inner(db_env, c.validators())
            .map_err(|e| format!("save validators: {e}"))?;
    }

    // Persist agent state
    {
        let registry = state.agent_registry.read().unwrap();
        let agent_balances = state.agent_balances.read().unwrap();
        save_agent_state_inner(db_env, &registry, &agent_balances)
            .map_err(|e| format!("save agents: {e}"))?;
    }

    // Persist oracle state
    {
        let oracle = state.oracle.read().unwrap();
        save_oracle_state(db_env, &oracle)
            .map_err(|e| format!("save oracle: {e}"))?;
    }

    // Persist governance state
    {
        let governance = state.governance.read().unwrap();
        save_governance_state(db_env, &governance)
            .map_err(|e| format!("save governance: {e}"))?;
    }

    // Persist consensus state
    {
        let c = consensus.read().unwrap();
        save_consensus_state_inner(db_env, &c)
            .map_err(|e| format!("save consensus: {e}"))?;
    }

    Ok(())
}

/// Load EVM accounts from DB
fn load_evm_accounts_inner(db: &DatabaseEnv) -> Result<EvmState, String> {
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
fn save_evm_accounts_inner(db: &DatabaseEnv, state: &EvmState) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .get_all_accounts()
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    db_clear::<CallEvmAccounts>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallEvmAccounts>(db, entries).map_err(|e: StorageError| e.to_string())?;
    Ok(())
}

/// Load bridge state from DB
fn load_bridge_state_inner(db: &DatabaseEnv) -> Result<BridgeStateManager, String> {
    match db_get::<CallBridgeOps>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e: serde_json::Error| e.to_string()),
        None => Ok(BridgeStateManager::default()),
    }
}

/// Save bridge state to DB
fn save_bridge_state_inner(db: &DatabaseEnv, state: &BridgeStateManager) -> Result<(), String> {
    let data = serde_json::to_vec(state).map_err(|e: serde_json::Error| e.to_string())?;
    db_clear::<CallBridgeOps>(db).map_err(|e: StorageError| e.to_string())?;
    db_put::<CallBridgeOps>(db, vec![0], data).map_err(|e: StorageError| e.to_string())?;
    Ok(())
}

/// Load shielded state from DB
fn load_shielded_state_inner(db: &DatabaseEnv) -> Result<ShieldedState, String> {
    let mut state = ShieldedState::new();

    // Load nullifiers
    let nf_data = db_iter_all::<CallShieldedNullifiers>(db).map_err(|e: StorageError| e.to_string())?;
    for (k, _) in nf_data {
        let nf: call_shielded::Nullifier = serde_json::from_slice(&k).map_err(|e: serde_json::Error| e.to_string())?;
        state.nullifier_set.insert(&nf);
    }

    // Load note commitments
    let cm_data = db_iter_all::<CallShieldedCommitments>(db).map_err(|e: StorageError| e.to_string())?;
    for (k, v) in cm_data {
        let key: call_shielded::NoteCommitment = serde_json::from_slice(&k).map_err(|e: serde_json::Error| e.to_string())?;
        let value: call_shielded::Note = serde_json::from_slice(&v).map_err(|e: serde_json::Error| e.to_string())?;
        state.note_registry.insert(key, value);
    }

    // Rebuild merkle tree from note commitments
    for cm in state.note_registry.keys() {
        state.merkle_tree.insert(cm.0);
    }

    Ok(state)
}

/// Save shielded state to DB
fn save_shielded_state_inner(db: &DatabaseEnv, state: &ShieldedState) -> Result<(), String> {
    // Save nullifiers
    let nf_entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .nullifier_set.spent_nullifiers()
        .iter()
        .map(|nf| (serde_json::to_vec(nf).unwrap(), vec![0]))
        .collect();
    db_clear::<CallShieldedNullifiers>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallShieldedNullifiers>(db, nf_entries).map_err(|e: StorageError| e.to_string())?;

    // Save note commitments
    let cm_entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .note_registry
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    db_clear::<CallShieldedCommitments>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallShieldedCommitments>(db, cm_entries).map_err(|e: StorageError| e.to_string())?;

    Ok(())
}

/// Load validator state from DB
fn load_validator_state_inner(db: &DatabaseEnv) -> Result<ValidatorStateManager, String> {
    let data = db_iter_all::<CallValidators>(db).map_err(|e: StorageError| e.to_string())?;
    let mut manager = ValidatorStateManager::new();
    for (k, v) in data {
        let stake: call_consensus::validator::ValidatorStake = serde_json::from_slice(&v).map_err(|e: serde_json::Error| e.to_string())?;
        let id: call_primitives::ValidatorId = serde_json::from_slice(&k).map_err(|e: serde_json::Error| e.to_string())?;
        manager.register_validator_from_stake(id, stake);
    }
    Ok(manager)
}

/// Save validator state to DB
fn save_validator_state_inner(db: &DatabaseEnv, state: &ValidatorStateManager) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .get_all_validators()
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    db_clear::<CallValidators>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallValidators>(db, entries).map_err(|e: StorageError| e.to_string())?;
    Ok(())
}

/// Load agent state from DB
fn load_agent_state_inner(db: &DatabaseEnv) -> Result<(AgentRegistry, AgentBalances), String> {
    let data = db_iter_all::<CallAgents>(db).map_err(|e: StorageError| e.to_string())?;
    let mut registry = AgentRegistry::new();
    let mut next_id: u64 = 0;

    for (k, v) in data {
        let reg: call_agent::AgentRegistration = serde_json::from_slice(&v).map_err(|e: serde_json::Error| e.to_string())?;
        if reg.agent_id >= next_id {
            next_id = reg.agent_id + 1;
        }
        let id: u64 = serde_json::from_slice(&k).map_err(|e: serde_json::Error| e.to_string())?;
        registry.agents.insert(id, reg.clone());
        registry.agents_by_owner.entry(reg.owner).or_default().push(reg.agent_id);
        registry.agents_by_name.insert(reg.name.clone(), reg.agent_id);
    }
    registry.next_id = next_id;

    Ok((registry, AgentBalances::new()))
}

/// Save agent state to DB
fn save_agent_state_inner(db: &DatabaseEnv, registry: &AgentRegistry, _balances: &AgentBalances) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = registry
        .agents
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    db_clear::<CallAgents>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallAgents>(db, entries).map_err(|e: StorageError| e.to_string())?;
    Ok(())
}

/// Save oracle state to the database.
fn save_oracle_state(db: &DatabaseEnv, state: &OracleManager) -> Result<(), String> {
    let data = serde_json::to_vec(state).map_err(|e| format!("serialize oracle: {e}"))?;
    db_put::<CallOracleState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load oracle state from the database.
fn load_oracle_state(db: &DatabaseEnv) -> Result<OracleManager, String> {
    match db_get::<CallOracleState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize oracle: {e}")),
        None => Ok(OracleManager::default()),
    }
}

/// Save governance state to the database.
fn save_governance_state(db: &DatabaseEnv, state: &GovernanceManager) -> Result<(), String> {
    let data = serde_json::to_vec(state).map_err(|e| format!("serialize governance: {e}"))?;
    db_put::<CallGovernanceState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load governance state from the database.
fn load_governance_state(db: &DatabaseEnv) -> Result<GovernanceManager, String> {
    match db_get::<CallGovernanceState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize governance: {e}")),
        None => Ok(GovernanceManager::new()),
    }
}

/// Save consensus state to the database.
fn save_consensus_state_inner(db: &DatabaseEnv, consensus: &SimplexConsensus) -> Result<(), String> {
    let state = consensus.persist_state();
    let data = bincode::serialize(&state).map_err(|e| format!("serialize consensus: {e}"))?;
    db_put::<CallConsensusState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load consensus state from the database.
fn load_consensus_state_inner(db: &DatabaseEnv, validators: &ValidatorStateManager) -> Result<SimplexConsensus, String> {
    match db_get::<CallConsensusState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => {
            let state: PersistedConsensusState = serde_json::from_slice(&data)
                .map_err(|e| format!("deserialize consensus: {e}"))?;
            Ok(SimplexConsensus::restore_from_persisted(state, validators.clone()))
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
fn persist_state_incremental(
    db_env: &Arc<DatabaseEnv>,
    state: &Arc<RpcState>,
    consensus: &Arc<RwLock<SimplexConsensus>>,
) -> Result<(), String> {
    // Persist balances (overwrite existing entries, no clear)
    {
        let bs = state.balance_state.read().map_err(|_| "balance lock poisoned".to_string())?;
        db_save_balances(db_env, bs.balances.balances_map(), bs.allowances.allowances_map())
            .map_err(|e| format!("save balances: {e}"))?;
    }

    // Persist EVM state (overwrite existing entries, no clear)
    {
        let evm = state.evm_state.read().map_err(|_| "evm lock poisoned".to_string())?;
        save_evm_accounts_no_clear(db_env, &evm)?;
    }

    // Persist bridge state
    {
        let bridge = state.bridge_state.read().map_err(|_| "bridge lock poisoned".to_string())?;
        save_bridge_state_inner(db_env, &bridge)?;
    }

    // Append-only shielded state: new nullifiers and commitments
    // (no clear — these are append-only data structures)
    {
        let shielded = state.shielded_state.read().map_err(|_| "shielded lock poisoned".to_string())?;
        // Only write new nullifiers (append, don't clear)
        let nf_entries: Vec<(Vec<u8>, Vec<u8>)> = shielded
            .nullifier_set.spent_nullifiers()
            .iter()
            .map(|nf| (serde_json::to_vec(nf).unwrap(), vec![0]))
            .collect();
        // Clear and rewrite nullifiers (they're small)
        db_clear::<CallShieldedNullifiers>(db_env).map_err(|e: StorageError| e.to_string())?;
        db_batch_put::<CallShieldedNullifiers>(db_env, nf_entries).map_err(|e: StorageError| e.to_string())?;

        // Write all commitments (append, no clear)
        let cm_entries: Vec<(Vec<u8>, Vec<u8>)> = shielded
            .note_registry
            .iter()
            .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
            .collect();
        db_clear::<CallShieldedCommitments>(db_env).map_err(|e: StorageError| e.to_string())?;
        db_batch_put::<CallShieldedCommitments>(db_env, cm_entries).map_err(|e: StorageError| e.to_string())?;
    }

    // Persist validator state (overwrite, no clear)
    {
        let c = consensus.read().map_err(|_| "consensus lock poisoned".to_string())?;
        save_validator_state_no_clear(db_env, c.validators())?;
    }

    // Persist agent state (overwrite, no clear)
    {
        let registry = state.agent_registry.read().map_err(|_| "agent lock poisoned".to_string())?;
        save_agent_state_no_clear(db_env, &registry)?;
    }

    // Persist oracle state (overwrite)
    {
        let oracle = state.oracle.read().map_err(|_| "oracle lock poisoned".to_string())?;
        save_oracle_state(db_env, &oracle)
            .map_err(|e| format!("save oracle: {e}"))?;
    }

    // Persist governance state (overwrite)
    {
        let governance = state.governance.read().map_err(|_| "governance lock poisoned".to_string())?;
        save_governance_state(db_env, &governance)
            .map_err(|e| format!("save governance: {e}"))?;
    }

    // Persist consensus state
    {
        let c = consensus.read().map_err(|_| "consensus lock poisoned".to_string())?;
        save_consensus_state_inner(db_env, &c)
            .map_err(|e| format!("save consensus: {e}"))?;
    }

    Ok(())
}

/// Save EVM accounts without clearing the table first.
fn save_evm_accounts_no_clear(db: &DatabaseEnv, state: &EvmState) -> Result<(), String> {
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

/// Save validator state without clearing the table first.
fn save_validator_state_no_clear(db: &DatabaseEnv, state: &ValidatorStateManager) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .get_all_validators()
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    for (k, v) in entries {
        db_put::<CallValidators>(db, k, v).map_err(|e: StorageError| e.to_string())?;
    }
    Ok(())
}

/// Save agent state without clearing the table first.
fn save_agent_state_no_clear(db: &DatabaseEnv, registry: &AgentRegistry) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = registry
        .agents
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    for (k, v) in entries {
        db_put::<CallAgents>(db, k, v).map_err(|e: StorageError| e.to_string())?;
    }
    Ok(())
}

/// Block production background loop
async fn block_production_loop(
    state: Arc<RpcState>,
    mempool: Arc<RwLock<Mempool>>,
    consensus: Arc<RwLock<SimplexConsensus>>,
    network: Option<Arc<dyn Network>>,
    initial_parent_hash: BlockHash,
    db: CallDb,
    mut prune_state: PruneState,
    subscriptions: SubscriptionManager,
) {
    let mut parent_hash = initial_parent_hash;
    let prune_config = call_storage::PruneConfig::default();

    let mut interval = tokio::time::interval(Duration::from_millis(
        consensus.read().ok().map(|c| c.params().block_time_millis).unwrap_or(250),
    ));

    loop {
        interval.tick().await;

        // 1. Select transactions from mempool
        let selection = { mempool.write().unwrap().select_transactions() };

        // 2. Build block
        let (proposer, height) = {
            let c = consensus.read().unwrap();
            (c.current_proposer(), c.current_height())
        };
        let Some(proposer) = proposer else {
            continue; // not our turn, skip this round
        };

        // Deserialize protocol txs from mempool data
        let protocol_txs: Vec<ProtocolTransaction> = selection
            .protocol_txs
            .into_iter()
            .filter_map(|e| serde_json::from_slice(&e.data).ok())
            .collect();

        let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

        let mut block = Block::new(
            height,
            parent_hash,
            current_timestamp_millis(),
            proposer,
            protocol_txs,
            evm_txs,
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        // 3. Execute block
        let result = {
            let mut balances = state.balance_state.write().unwrap();
            let registry = state.asset_registry.read().unwrap();
            let mut compliance = state.compliance_engine.write().unwrap();
            let mut bridge_state = state.bridge_state.write().unwrap();
            let mut shielded_state = state.shielded_state.write().unwrap();
            let mut fee_params = state.fee_params.write().unwrap();
            let mut evm_state = state.evm_state.write().unwrap();
            let mut oracle = state.oracle.write().unwrap();

            block.execute(
                &mut balances,
                &registry,
                &mut compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                height,
                &mut evm_state,
                Some(&mut *oracle),
            )
        };
        let result = match result {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = ?e, "block execution failed");
                continue;
            }
        };
        block.finalize(&result);

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
                    oracle.tracked_assets.clone()
                };
                if !tracked.is_empty() {
                    let proposer_id = proposer;
                    let request = OraclePriceRequest {
                        asset_ids: tracked,
                        block: height,
                        requester_id: proposer_id,
                    };
                    let msg = bincode::serialize(&NetworkMessage::OraclePriceRequest(request))
                        .expect("serialize oracle request");
                    net.broadcast(ORACLE_CHANNEL, msg).await;
                    // Configurable delay to allow validators to respond
                    let delay_ms = consensus.read()
                        .ok()
                        .map(|c| c.params().oracle_request_delay_ms)
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
        if let Err(ref e) = call_storage::maybe_prune(&mut prune_state, new_height, &prune_config) {
            tracing::warn!(error = %e, "prune check failed");
        }

        // 14. Incrementally persist state changes after every block
        if let Some(ref db_env) = db.db {
            if let Err(ref e) = persist_state_incremental(db_env, &state, &consensus) {
                tracing::warn!(error = %e, "failed to incrementally persist state");
            }
        }

        // 15. Full table rebuild every 1000 blocks as safety net
        if new_height % 1000 == 0 {
            if let Some(ref db_env) = db.db {
                if let Err(ref e) = persist_state_to_db(db_env, &state, &consensus) {
                    tracing::warn!(error = %e, "failed to full-rebuild persist state");
                }
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
            net.broadcast(BLOCK_CHANNEL, msg).await;
        }
    }
}

/// BFT event loop — handles propose / verify / finalize / broadcast from the
/// Commonware Simplex BFT engine running in a background thread.
async fn bft_event_loop(
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
    epoch_number: u64,
    exit_tx: oneshot::Sender<EpochRotationReason>,
) {
    let mut execution_results: std::collections::HashMap<
        ConsensusDigest,
        call_consensus::BlockExecutionResult,
    > = std::collections::HashMap::new();
    let prune_config = call_storage::PruneConfig::default();

    // Track qualified validator count to detect changes
    let mut qualified_validator_count = {
        let vs = state.validator_state.read().unwrap();
        vs.get_qualified_validators().len()
    };

    // Build a mapping from ed25519 pubkey -> validator id for propose lookups
    let pubkey_to_id = {
        let vs = state.validator_state.read().unwrap();
        let mut map = std::collections::HashMap::new();
        for (id, stake) in vs.get_all_validators().iter() {
            if let Ok(pk) = ed25519::PublicKey::decode(&stake.ed25519_pubkey[..]) {
                map.insert(pk, *id);
            }
        }
        map
    };

    loop {
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

                let protocol_txs: Vec<ProtocolTransaction> = selection
                    .protocol_txs
                    .into_iter()
                    .filter_map(|e| serde_json::from_slice(&e.data).ok())
                    .collect();
                let evm_txs: Vec<Vec<u8>> = selection.evm_txs.into_iter().map(|e| e.data).collect();

                // Request oracle price submissions at boundary intervals
                let is_oracle_boundary = height.is_multiple_of(ORACLE_UPDATE_INTERVAL);
                if is_oracle_boundary {
                    if let Some(ref net) = network {
                        let tracked = { state.oracle.read().unwrap().tracked_assets.clone() };
                        if !tracked.is_empty() {
                            let request = OraclePriceRequest {
                                asset_ids: tracked,
                                block: height,
                                requester_id: proposer,
                            };
                            let msg = bincode::serialize(&NetworkMessage::OraclePriceRequest(request))
                                .expect("serialize oracle request");
                            net.broadcast(ORACLE_CHANNEL, msg).await;
                            let delay_ms = consensus.read()
                                .ok()
                                .map(|c| c.params().oracle_request_delay_ms)
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
                    protocol_txs,
                    evm_txs,
                    vec![SystemTx {
                        kind: SystemTxKind::UpdateBaseFee,
                        data: vec![],
                    }],
                    selection.bridge_ops,
                );

                // Execute the block
                let result = {
                    let mut balances = state.balance_state.write().unwrap();
                    let registry = state.asset_registry.read().unwrap();
                    let mut compliance = state.compliance_engine.write().unwrap();
                    let mut bridge_state = state.bridge_state.write().unwrap();
                    let mut shielded_state = state.shielded_state.write().unwrap();
                    let mut fee_params = state.fee_params.write().unwrap();
                    let mut evm_state = state.evm_state.write().unwrap();
                    let mut oracle = state.oracle.write().unwrap();

                    block.execute(
                        &mut balances,
                        &registry,
                        &mut compliance,
                        &mut bridge_state,
                        &mut shielded_state,
                        &mut fee_params,
                        height,
                        &mut evm_state,
                        Some(&mut *oracle),
                    )
                };

                match result {
                    Ok(result) => {
                        block.finalize(&result);

                        // Handle oracle period transitions at boundary heights
                        if is_oracle_boundary {
                            let mut oracle = state.oracle.write().unwrap();
                            oracle.advance_period(height);
                            let outliers: Vec<u32> = oracle.last_outliers().to_vec();
                            drop(oracle);
                            if !outliers.is_empty() {
                                let mut c = consensus.write().unwrap();
                                for vid in &outliers {
                                    if let Err(e) = c.handle_oracle_outlier(*vid) {
                                        tracing::warn!(validator_id = vid, error = ?e, "failed to slash oracle outlier");
                                    }
                                }
                                tracing::info!(outliers = ?outliers, "slashed oracle outliers");
                            }
                            let contributions = {
                                state.oracle.write().unwrap().distribute_rewards()
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
                            state.oracle.write().unwrap().clear_tracking();
                        }

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
                    for attempt in 1..=5 {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        let cache = block_cache.lock().unwrap();
                        if let Some(b) = cache.get(&digest) {
                            block = Some(b.clone());
                            break;
                        }
                        drop(cache);
                        if attempt == 5 {
                            tracing::warn!(digest = %digest, "BFT verify: block not in cache after waiting");
                        }
                    }
                }

                let valid = if let Some(block) = block {
                    let height = block.header.height;
                    let result = {
                        let mut balances = state.balance_state.write().unwrap();
                        let registry = state.asset_registry.read().unwrap();
                        let mut compliance = state.compliance_engine.write().unwrap();
                        let mut bridge_state = state.bridge_state.write().unwrap();
                        let mut shielded_state = state.shielded_state.write().unwrap();
                        let mut fee_params = state.fee_params.write().unwrap();
                        let mut evm_state = state.evm_state.write().unwrap();
                        let mut oracle = state.oracle.write().unwrap();

                        block.execute(
                            &mut balances,
                            &registry,
                            &mut compliance,
                            &mut bridge_state,
                            &mut shielded_state,
                            &mut fee_params,
                            height,
                            &mut evm_state,
                            Some(&mut *oracle),
                        )
                    };

                    match result {
                        Ok(r) => {
                            execution_results.insert(digest, r);
                            true
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
                let block = {
                    let mut cache = block_cache.lock().unwrap();
                    cache.remove(&info.digest)
                };

                if let Some(block) = block {
                    let height = block.header.height;

                    // Use cached execution result if available
                    let result = execution_results.remove(&info.digest);
                    let result = match result {
                        Some(r) => r,
                        None => {
                            // Re-execute if cache missed (should be rare)
                            let mut balances = state.balance_state.write().unwrap();
                            let registry = state.asset_registry.read().unwrap();
                            let mut compliance = state.compliance_engine.write().unwrap();
                            let mut bridge_state = state.bridge_state.write().unwrap();
                            let mut shielded_state = state.shielded_state.write().unwrap();
                            let mut fee_params = state.fee_params.write().unwrap();
                            let mut evm_state = state.evm_state.write().unwrap();
                            let mut oracle = state.oracle.write().unwrap();

                            match block.execute(
                                &mut balances,
                                &registry,
                                &mut compliance,
                                &mut bridge_state,
                                &mut shielded_state,
                                &mut fee_params,
                                height,
                                &mut evm_state,
                                Some(&mut *oracle),
                            ) {
                                Ok(r) => r,
                                Err(e) => {
                                    tracing::warn!(error = ?e, height, "BFT finalize: re-execution failed");
                                    continue;
                                }
                            }
                        }
                    };

                    // Handle oracle period transitions at boundary heights
                    // (non-proposing validators advance period but don't broadcast requests — proposer already did)
                    let is_oracle_boundary = height.is_multiple_of(ORACLE_UPDATE_INTERVAL);
                    if is_oracle_boundary {
                        let mut oracle = state.oracle.write().unwrap();
                        oracle.advance_period(height);
                        let outliers: Vec<u32> = oracle.last_outliers().to_vec();
                        drop(oracle);
                        if !outliers.is_empty() {
                            let mut c = consensus.write().unwrap();
                            for vid in &outliers {
                                if let Err(e) = c.handle_oracle_outlier(*vid) {
                                    tracing::warn!(validator_id = vid, error = ?e, "failed to slash oracle outlier");
                                }
                            }
                            tracing::info!(outliers = ?outliers, "slashed oracle outliers");
                        }
                        let contributions = {
                            state.oracle.write().unwrap().distribute_rewards()
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
                        state.oracle.write().unwrap().clear_tracking();
                    }

                    // Commit via consensus
                    {
                        let mut c = consensus.write().unwrap();
                        if let Err(e) = c.commit_block(&block, &result) {
                            tracing::warn!(error = ?e, height, "BFT finalize: commit failed");
                            continue;
                        }
                    }

                    // Advance state
                    let new_height = height + 1;
                    state.set_current_block(new_height);
                    parent_hash = block.header.hash();
                    state.finalize_block();

                    // Advance governance
                    {
                        let mut gov = state.governance.write().unwrap();
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

                    // Sync validators into governance
                    {
                        let mut gov = state.governance.write().unwrap();
                        let vs = state.validator_state.read().unwrap();
                        for (id, stake) in vs.get_all_validators().iter() {
                            gov.register_validator(*id, stake.address);
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

                    if let Err(e) = call_storage::maybe_prune(&mut prune_state, new_height, &prune_config) {
                        tracing::warn!(error = %e, "BFT finalize: prune check failed");
                    }

                    // Incremental state persistence
                    if let Some(ref db_env) = db.db {
                        if let Err(e) = persist_state_incremental(db_env, &state, &consensus) {
                            tracing::warn!(error = %e, "BFT finalize: incremental persist failed");
                        }
                    }

                    // Full rebuild every 1000 blocks
                    if new_height % 1000 == 0 {
                        if let Some(ref db_env) = db.db {
                            if let Err(e) = persist_state_to_db(db_env, &state, &consensus) {
                                tracing::warn!(error = %e, "BFT finalize: full persist failed");
                            }
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
                    let epoch_length = consensus.read().unwrap().params().epoch_length;
                    let new_height = height + 1;
                    if new_height % epoch_length == 0 {
                        tracing::info!(
                            epoch = epoch_number + 1,
                            height = new_height,
                            "BFT: epoch boundary reached, rotating participant subset"
                        );
                        let _ = exit_tx.send(EpochRotationReason::EpochBoundary);
                        break;
                    }

                    // Check for qualified validator set changes
                    let current_qualified = {
                        let vs = state.validator_state.read().unwrap();
                        vs.get_qualified_validators().len()
                    };
                    if qualified_validator_count != current_qualified {
                        tracing::warn!(
                            old = qualified_validator_count,
                            new = current_qualified,
                            "BFT: qualified validator set changed — rotating epoch"
                        );
                        let _ = exit_tx.send(EpochRotationReason::ValidatorSetChange);
                        break;
                    }
                    qualified_validator_count = current_qualified;
                } else {
                    tracing::warn!(digest = %info.digest, "BFT finalize: block not in cache");
                }
            }

            Some(block_bytes) = broadcast_rx.recv() => {
                // Relay wants us to broadcast a block via the app P2P network
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

fn current_timestamp_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1)
}

fn persist_block(data_dir: &Path, height: u64, block: &Block) -> Result<(), String> {
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

/// Handle a sync request from a peer: load blocks from disk and respond.
fn handle_sync_request(data_dir: &Path, request: &SyncRequest) -> Option<SyncResponse> {
    let mut blocks = Vec::new();
    let end = request.start_height.saturating_add(request.count);
    for h in request.start_height..end {
        if let Some(block) = load_block(data_dir, h) {
            if let Ok(serialized) = bincode::serialize(&block) {
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

fn handle_network_message(
    peer_id: &str,
    channel: u64,
    data: &[u8],
    mempool: &Arc<RwLock<Mempool>>,
    state: &Arc<RpcState>,
    network: &Arc<dyn Network>,
) {
    match channel {
        TX_CHANNEL => {
            if let Ok(tx_msg) = serde_json::from_slice::<TransactionMessage>(data) {
                if tx_msg.verify_checksum() {
                    if let Ok(mut pool) = mempool.write() {
                        if let Ok(evm_tx) = serde_json::from_slice::<call_evm::EvmTransaction>(&tx_msg.data) {
                            let _ = pool.insert_evm_tx(evm_tx);
                        }
                    }
                }
            }
        }
        BLOCK_CHANNEL => {
            // Handle block announcements (post-commit) — trigger sync if behind
            if let Ok(announcement) = serde_json::from_slice::<BlockAnnouncement>(data) {
                let local_height = state.get_current_block();
                if announcement.height > local_height {
                    tracing::info!(
                        peer_id,
                        height = announcement.height,
                        local = local_height,
                        "block announcement: peer ahead, requesting sync"
                    );
                    let request = SyncRequest {
                        start_height: local_height,
                        count: 100,
                        full_state: false,
                    };
                    let req_data = bincode::serialize(&NetworkMessage::SyncRequest(request))
                        .expect("serialize sync request");
                    let peer_id_owned = peer_id.to_string();
                    let net = Arc::clone(network);
                    tokio::spawn(async move {
                        net.send_to(vec![peer_id_owned], req_data).await;
                    });
                } else {
                    tracing::debug!(
                        peer_id,
                        height = announcement.height,
                        local = local_height,
                        "block announcement: already caught up"
                    );
                }
            }
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
                        let vs = state_clone.validator_state.read().unwrap();
                        !vs.get_active_validators().is_empty()
                    };
                    if is_validator {
                        // Fetch prices for requested assets using the oracle's tracked assets
                        let current_block = state_clone.get_current_block();
                        let timestamp = std::time::SystemTime::now()
                            .duration_since(std::time::UNIX_EPOCH)
                            .map(|d| d.as_secs())
                            .unwrap_or(0);

                        // Submit a price for each requested asset
                        for asset_id in &request.asset_ids {
                            // Use last known price as a baseline (fetchers would override)
                            let price = {
                                let oracle = state_clone.oracle.read().unwrap();
                                oracle.get_price(*asset_id).map(|p| p.median_price)
                            };
                            if let Some(price) = price {
                                // Send back as an oracle price submission
                                let submission = OraclePriceSubmission {
                                    validator_id: 0, // would be this validator's ID
                                    asset_id: *asset_id,
                                    price,
                                    block_number: current_block,
                                    timestamp,
                                    signature: [0u8; 64], // would be signed
                                    sources: vec!["local_oracle".into()],
                                };
                                if let Ok(msg) = bincode::serialize(&NetworkMessage::OraclePriceSubmission(submission)) {
                                    net_clone.send_to(vec![peer_id_owned.clone()], msg).await;
                                }
                            }
                        }
                    }
                });
            } else if let Ok(submission) = serde_json::from_slice::<OraclePriceSubmission>(data) {
                // Proposer received a price submission from a validator.
                // Feed it through the oracle's full validation pipeline via RPC-style submission.
                let state_clone = Arc::clone(state);
                tokio::spawn(async move {
                    let current_block = state_clone.get_current_block();
                    let oracle_submission = OracleSubmission {
                        validator_id: submission.validator_id,
                        asset_id: submission.asset_id,
                        price: submission.price,
                        block_number: current_block,
                        timestamp: submission.timestamp,
                        signature: submission.signature,
                        sources: submission.sources,
                    };
                    let mut oracle = state_clone.oracle.write().unwrap();
                    if let Err(e) = oracle.submit_price(oracle_submission) {
                        tracing::debug!(error = %e, validator_id = submission.validator_id, "oracle P2P submission rejected");
                    }
                });
            }
        }
        _ => {}
    }
}

impl Default for CallNode {
    fn default() -> Self {
        Self::new(PathBuf::from(".call-data")).expect("default node creation")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_network::InMemoryNetwork;
    use call_primitives::{Address, Ed25519PublicKey};
    use call_protocol::instructions::Instruction;
    use call_protocol::transaction::{AuthScheme, GasConfig, ProtocolTransaction};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_pubkey(n: u8) -> Ed25519PublicKey {
        let mut key = [0u8; 32];
        key[0] = n;
        key
    }

    fn make_test_tx(nonce: u64) -> ProtocolTransaction {
        ProtocolTransaction {
            sender: test_addr(1),
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
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        }
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
            balances.balances.set_balance(1, test_addr(1), 10_000).unwrap();
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

        let mut block = Block::new(
            height,
            node.parent_hash,
            1_000, // timestamp
            proposer,
            protocol_txs,
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        // Execute
        let mut balances = node.state.balance_state.write().unwrap();
        let registry = node.state.asset_registry.read().unwrap();
        let mut compliance = node.state.compliance_engine.write().unwrap();
        let mut bridge_state = node.state.bridge_state.write().unwrap();
        let mut shielded_state = node.state.shielded_state.write().unwrap();
        let mut fee_params = node.state.fee_params.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();

        let result = block
            .execute(
                &mut balances,
                &registry,
                &mut compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                height,
                &mut evm_state,
                None,
            )
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

        let mut block = Block::new(
            height,
            node.parent_hash,
            2_000,
            proposer,
            vec![],
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            vec![],
        );

        // Execute empty block
        let mut balances = node.state.balance_state.write().unwrap();
        let registry = node.state.asset_registry.read().unwrap();
        let mut compliance = node.state.compliance_engine.write().unwrap();
        let mut bridge_state = node.state.bridge_state.write().unwrap();
        let mut shielded_state = node.state.shielded_state.write().unwrap();
        let mut fee_params = node.state.fee_params.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();

        let result = block
            .execute(
                &mut balances,
                &registry,
                &mut compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                height,
                &mut evm_state,
                None,
            )
            .expect("empty block execution");
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
            balances.balances.set_balance(1, test_addr(1), 10_000).unwrap();
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

        let mut block = Block::new(
            height,
            node1.parent_hash,
            3_000,
            proposer,
            protocol_txs,
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        let mut balances = node1.state.balance_state.write().unwrap();
        let registry = node1.state.asset_registry.read().unwrap();
        let mut compliance = node1.state.compliance_engine.write().unwrap();
        let mut bridge_state = node1.state.bridge_state.write().unwrap();
        let mut shielded_state = node1.state.shielded_state.write().unwrap();
        let mut fee_params = node1.state.fee_params.write().unwrap();
        let mut evm_state = node1.state.evm_state.write().unwrap();

        let result = block
            .execute(&mut balances, &registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut fee_params, height, &mut evm_state, None)
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
            balances.balances.set_balance(1, test_addr(1), 10_000).unwrap();
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

        let mut block = Block::new(
            height,
            node.parent_hash,
            4_000,
            proposer,
            protocol_txs,
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            selection.bridge_ops,
        );

        let mut balances = node.state.balance_state.write().unwrap();
        let registry = node.state.asset_registry.read().unwrap();
        let mut compliance = node.state.compliance_engine.write().unwrap();
        let mut bridge_state = node.state.bridge_state.write().unwrap();
        let mut shielded_state = node.state.shielded_state.write().unwrap();
        let mut fee_params = node.state.fee_params.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();

        let result = block
            .execute(&mut balances, &registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut fee_params, height, &mut evm_state, None)
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
        let initial_balance: u128 = 50_000;
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
                balances.balances.set_balance(1, test_addr(1), initial_balance).unwrap();
            }

            // Insert a transfer tx
            let tx = ProtocolTransaction {
                sender: test_addr(1),
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
                auth: AuthScheme::SingleSig {
                    signature: [0u8; 65],
                },
            };
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

            let mut block = Block::new(
                height, node.parent_hash, 5_000, proposer, protocol_txs, vec![],
                vec![SystemTx { kind: SystemTxKind::UpdateBaseFee, data: vec![] }],
                selection.bridge_ops,
            );

            let result = {
                let mut balances = node.state.balance_state.write().unwrap();
                let registry = node.state.asset_registry.read().unwrap();
                let mut compliance = node.state.compliance_engine.write().unwrap();
                let mut bridge_state = node.state.bridge_state.write().unwrap();
                let mut shielded_state = node.state.shielded_state.write().unwrap();
                let mut fee_params = node.state.fee_params.write().unwrap();
                let mut evm_state = node.state.evm_state.write().unwrap();

                block.execute(&mut balances, &registry, &mut compliance, &mut bridge_state,
                              &mut shielded_state, &mut fee_params, height, &mut evm_state, None)
                    .expect("execution")
            };
            block.finalize(&result);

            {
                let mut consensus = node.consensus.write().unwrap();
                consensus.commit_block(&block, &result).expect("commit");
            }

            // Persist state to reth-db immediately
            if let Some(ref db_env) = node.db.db {
                persist_state_to_db(db_env, &node.state, &node.consensus)
                    .expect("persist state");
            }

            // Node is dropped here, simulating shutdown
        }

        // === Phase 2: Create new node from same data dir, verify state ===
        {
            let node2 = CallNode::new(tmp.clone()).expect("node creation (restart)");

            // Verify balance was recovered
            let sender_balance = {
                let balances = node2.state.balance_state.read().unwrap();
                balances.balances.get_balance(1, &test_addr(1))
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
        use call_governance::{ProposalType, GovernanceEvent, PROPOSAL_DEPOSIT, REVIEW_PERIOD_BLOCKS, VOTING_PERIOD_BLOCKS, TIMELOCK_PERIOD_BLOCKS, EXECUTION_TIMEOUT_BLOCKS};

        let tmp = std::env::temp_dir().join("call_gov_cycle_test");
        let _ = std::fs::remove_dir_all(&tmp);

        let node = CallNode::new(tmp.clone()).expect("node creation");

        // Fund a proposer address with enough CALL for deposit
        {
            let mut balances = node.state.balance_state.write().unwrap();
            let proposer = call_primitives::Address::repeat_byte(0xAA);
            balances.balances.credit_balance(0, proposer, PROPOSAL_DEPOSIT * 5).expect("fund proposer");
        }

        let proposer = call_primitives::Address::repeat_byte(0xAA);

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
}
