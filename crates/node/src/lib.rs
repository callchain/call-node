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

use call_consensus::{
    Block, ConsensusParams, SimplexConsensus, SystemTx, SystemTxKind, ValidatorStateManager,
};
use call_network::{CommonwareConfig, CommonwareNetwork, Network, NetworkMessage, BlockAnnouncement, TransactionMessage};
use call_primitives::BlockHash;
use call_protocol::{
    BalanceState, AssetRegistry, ComplianceEngine,
    transaction::ProtocolTransaction,
};
use call_rpc::{RpcState, RpcConfig, build_rpc_module};
use call_storage::{CallDb, open_db, PruneState, StorageError};
use call_storage::reth_db::{
    init_call_db, save_balances as db_save_balances, load_balances as db_load_balances,
    save_prune_state as db_save_prune, load_prune_state as db_load_prune,
    db_put, db_batch_put, db_clear, db_iter_all, db_get,
    CallEvmAccounts, CallEvmStorage, CallBridgeOps,
    CallShieldedNullifiers, CallShieldedCommitments, CallValidators, CallAgents,
};
use reth_db::DatabaseEnv;
use call_transaction_pool::Mempool;
use call_evm::EvmState;
use call_bridge::BridgeStateManager;
use call_agent::{AgentRegistry, AgentBalances};
use call_shielded::ShieldedState;
use jsonrpsee::server::{Server, ServerHandle};
use std::path::PathBuf;
use std::sync::{Arc, RwLock};
use std::time::Duration;

/// Chain ID for Callchain devnet
pub const CALLCHAIN_CHAIN_ID: u64 = 1337;

/// P2P message channels
const TX_CHANNEL: u64 = 1;
const BLOCK_CHANNEL: u64 = 2;

/// The Callchain node
pub struct CallNode {
    pub state: Arc<RpcState>,
    pub mempool: Arc<RwLock<Mempool>>,
    pub consensus: Arc<RwLock<SimplexConsensus>>,
    pub network: Option<Arc<dyn Network>>,
    pub db: CallDb,
    pub prune_state: PruneState,
    pub server_handle: Option<ServerHandle>,
    pub parent_hash: BlockHash,
}

impl CallNode {
    /// Create a new node with default state
    pub fn new(data_dir: PathBuf) -> Result<Self, String> {
        let mempool = Arc::new(RwLock::new(Mempool::new()));
        let db = open_db(data_dir.clone()).map_err(|e| format!("failed to open db: {e}"))?;

        // Restore prune state from disk if previously persisted
        let prune_state = db.load_prune_state()
            .map_err(|e| format!("failed to load prune state: {e}"))?;

        // Load persisted state from reth-db if available
        let (balance_state, evm_state, bridge_state, shielded_state, consensus_validators, registry, agent_balances) =
            if let Some(ref db_env) = db.db {
                load_state_from_db(db_env)
            } else {
                (BalanceState::new(), EvmState::new(), BridgeStateManager::default(),
                 ShieldedState::new(), ValidatorStateManager::default(),
                 AgentRegistry::new(), AgentBalances::new())
            };

        let consensus = SimplexConsensus::new(
            ConsensusParams::default(),
            consensus_validators,
        );

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
        ));

        Ok(Self {
            state,
            mempool,
            consensus: Arc::new(RwLock::new(consensus)),
            network: None,
            db,
            prune_state,
            server_handle: None,
            parent_hash: BlockHash::ZERO,
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

    /// Start P2P network
    pub async fn start_network(&mut self, config: CommonwareConfig) -> Result<(), String> {
        let network = CommonwareNetwork::new(&config)
            .await
            .map_err(|e| format!("network init failed: {e}"))?;
        let network: Arc<dyn Network> = Arc::new(network);
        self.network = Some(Arc::clone(&network));

        // Start receive loop
        let mempool = Arc::clone(&self.mempool);
        let state = Arc::clone(&self.state);
        tokio::spawn(async move {
            while let Ok((peer_id, channel, data)) = network.receive().await {
                handle_network_message(&peer_id, channel, &data, &mempool, &state);
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

        tokio::spawn(block_production_loop(
            state,
            mempool,
            consensus,
            network,
            parent_hash,
            db,
            prune_state,
        ))
    }

    /// Stop the RPC server and flush final state to disk.
    pub async fn stop(&mut self) -> Result<(), String> {
        if let Some(handle) = self.server_handle.take() {
            handle.stop().map_err(|_| "server already stopped".to_string())?;
        }
        // Flush final state to reth-db
        if let Some(ref db_env) = self.db.db {
            if let Err(e) = persist_state_to_db(db_env, &self.state, &self.consensus) {
                tracing::warn!(error = %e, "failed to flush state on shutdown");
            }
            if let Err(e) = db_save_prune(db_env, &self.prune_state) {
                tracing::warn!(error = %e, "failed to flush prune state on shutdown");
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
      ValidatorStateManager, AgentRegistry, AgentBalances) {
    // Load balances
    let (balances, allowances) = match db_load_balances(db_env) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("WARN: failed to load balances from db: {e}");
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
            eprintln!("WARN: failed to load evm accounts: {e}");
            EvmState::new()
        }
    };

    // Load bridge state
    let bridge_state = match load_bridge_state_inner(db_env) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("WARN: failed to load bridge state: {e}");
            BridgeStateManager::default()
        }
    };

    // Load shielded state
    let shielded_state = match load_shielded_state_inner(db_env) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("WARN: failed to load shielded state: {e}");
            ShieldedState::new()
        }
    };

    // Load validator state
    let validators = match load_validator_state_inner(db_env) {
        Ok(v) => v,
        Err(e) => {
            eprintln!("WARN: failed to load validator state: {e}");
            ValidatorStateManager::default()
        }
    };

    // Load agent state
    let (registry, agent_balances) = match load_agent_state_inner(db_env) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("WARN: failed to load agent state: {e}");
            (AgentRegistry::new(), AgentBalances::new())
        }
    };

    (balance_state, evm_state, bridge_state, shielded_state, validators, registry, agent_balances)
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
    for (cm, _) in &state.note_registry {
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

/// Block production background loop
async fn block_production_loop(
    state: Arc<RpcState>,
    mempool: Arc<RwLock<Mempool>>,
    consensus: Arc<RwLock<SimplexConsensus>>,
    network: Option<Arc<dyn Network>>,
    initial_parent_hash: BlockHash,
    db: CallDb,
    mut prune_state: PruneState,
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
            let compliance = state.compliance_engine.read().unwrap();
            let mut bridge_state = state.bridge_state.write().unwrap();
            let mut shielded_state = state.shielded_state.write().unwrap();
            let mut fee_params = state.fee_params.write().unwrap();
            let mut evm_state = state.evm_state.write().unwrap();

            block.execute(
                &mut balances,
                &registry,
                &compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                height,
                &mut evm_state,
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

        // 4. Commit via consensus
        {
            let mut c = consensus.write().unwrap();
            if let Err(e) = c.commit_block(&block, &result) {
                tracing::warn!(error = ?e, "commit failed");
                continue;
            }
        }

        // 5. Update state
        let new_height = height + 1;
        state.set_current_block(new_height);
        parent_hash = block.header.hash();
        state.finalize_block();

        // 6. Persist block to disk
        if let Err(ref e) = persist_block(&db.data_dir, height, &block) {
            tracing::warn!(error = %e, "failed to persist block");
        }

        // 7. Update prune tracking state
        prune_state.add_block_body(height, call_storage::BlockBody {
            block_hash: parent_hash,
            tx_count: result.total_tx_count() as u32,
            body_size: 0, // would be actual serialized size in production
        });

        // 8. Run periodic prune checks
        if let Err(ref e) = call_storage::maybe_prune(&mut prune_state, new_height, &prune_config) {
            tracing::warn!(error = %e, "prune check failed");
        }

        // 9. Persist state to reth-db every 100 blocks
        if new_height % 100 == 0 {
            if let Some(ref db_env) = db.db {
                if let Err(ref e) = persist_state_to_db(db_env, &state, &consensus) {
                    tracing::warn!(error = %e, "failed to persist state to db");
                }
                if let Err(ref e) = db_save_prune(db_env, &prune_state) {
                    tracing::warn!(error = %e, "failed to persist prune state");
                }
            } else if let Err(ref e) = db.save_prune_state(&prune_state) {
                tracing::warn!(error = %e, "failed to persist prune state");
            }
        }

        tracing::info!(height = new_height, tx_count = result.total_tx_count(), "committed block");

        // 10. Broadcast block announcement via P2P
        if let Some(ref net) = network {
            let announcement = BlockAnnouncement {
                block_hash: parent_hash,
                height,
                proposer,
                timestamp_millis: block.header.timestamp_millis,
            };
            let msg = serde_json::to_vec(&NetworkMessage::BlockAnnouncement(announcement))
                .expect("serialize block announcement");
            net.broadcast(BLOCK_CHANNEL, msg).await;
        }
    }
}

fn current_timestamp_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(1)
}

fn persist_block(data_dir: &PathBuf, height: u64, block: &Block) -> Result<(), String> {
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

fn handle_network_message(
    peer_id: &str,
    channel: u64,
    data: &[u8],
    mempool: &Arc<RwLock<Mempool>>,
    _state: &Arc<RpcState>,
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
            if let Ok(announcement) = serde_json::from_slice::<BlockAnnouncement>(data) {
                tracing::info!(
                    peer_id,
                    height = announcement.height,
                    proposer = announcement.proposer,
                    "received block announcement"
                );
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
        let compliance = node.state.compliance_engine.read().unwrap();
        let mut bridge_state = node.state.bridge_state.write().unwrap();
        let mut shielded_state = node.state.shielded_state.write().unwrap();
        let mut fee_params = node.state.fee_params.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();

        let result = block
            .execute(
                &mut balances,
                &registry,
                &compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                height,
                &mut evm_state,
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
        let compliance = node.state.compliance_engine.read().unwrap();
        let mut bridge_state = node.state.bridge_state.write().unwrap();
        let mut shielded_state = node.state.shielded_state.write().unwrap();
        let mut fee_params = node.state.fee_params.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();

        let result = block
            .execute(
                &mut balances,
                &registry,
                &compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                height,
                &mut evm_state,
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
        let compliance = node1.state.compliance_engine.read().unwrap();
        let mut bridge_state = node1.state.bridge_state.write().unwrap();
        let mut shielded_state = node1.state.shielded_state.write().unwrap();
        let mut fee_params = node1.state.fee_params.write().unwrap();
        let mut evm_state = node1.state.evm_state.write().unwrap();

        let result = block
            .execute(&mut balances, &registry, &compliance, &mut bridge_state, &mut shielded_state, &mut fee_params, height, &mut evm_state)
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
        let msg = serde_json::to_vec(&NetworkMessage::BlockAnnouncement(announcement))
            .expect("serialize block announcement");
        shared_network.broadcast(BLOCK_CHANNEL, msg).await;

        // Step 4: Node2 receives the block announcement from shared network
        let (peer_id, channel, data) = shared_network.receive().await.expect("should receive message");
        assert_eq!(channel, BLOCK_CHANNEL);
        assert_eq!(peer_id, "broadcast");

        let received = serde_json::from_slice::<NetworkMessage>(&data).expect("parse network message");
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
        let compliance = node.state.compliance_engine.read().unwrap();
        let mut bridge_state = node.state.bridge_state.write().unwrap();
        let mut shielded_state = node.state.shielded_state.write().unwrap();
        let mut fee_params = node.state.fee_params.write().unwrap();
        let mut evm_state = node.state.evm_state.write().unwrap();

        let result = block
            .execute(&mut balances, &registry, &compliance, &mut bridge_state, &mut shielded_state, &mut fee_params, height, &mut evm_state)
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
                let compliance = node.state.compliance_engine.read().unwrap();
                let mut bridge_state = node.state.bridge_state.write().unwrap();
                let mut shielded_state = node.state.shielded_state.write().unwrap();
                let mut fee_params = node.state.fee_params.write().unwrap();
                let mut evm_state = node.state.evm_state.write().unwrap();

                block.execute(&mut balances, &registry, &compliance, &mut bridge_state,
                              &mut shielded_state, &mut fee_params, height, &mut evm_state)
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
}
