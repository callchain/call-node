//! RpcState struct and its methods.

use call_protocol::{AccountState, AssetRegistry, ComplianceEngine, ProtocolReceipt, FeeParams, FeeCurrencyRegistry};
use call_protocol::security::MempoolDefense;
use call_evm::{EvmState, EvmExecutor, EvmTransaction, EvmExecutionResult};
use call_bridge::BridgeStateManager;
use call_consensus::{ValidatorStateManager, ForkManager, RollbackPlan, ConsensusParams};
use call_agent::{AgentRegistry, AgentBalances};
use call_shielded::ShieldedState;
use call_primitives::{Address, AssetId, Balance, TxHash, Hash};
use call_crypto::SignerRef;
use call_transaction_pool::Mempool;
use call_oracle::OracleManager;
use call_governance::GovernanceManager;
use alloy_consensus::{TxEnvelope, Transaction as _, transaction::SignerRecoverable};
use alloy_primitives::Bytes;
use alloy_rlp::Decodable;
use crate::ws::SubscriptionManager;
use crate::handlers::helpers::{AssetInfoResponse, AgentInfoResponse, ShieldedTreeStateResponse};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::collections::{HashMap, VecDeque, HashSet};
use std::sync::{Arc, RwLock};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// Per-block fee data for eth_feeHistory queries.
#[derive(Debug, Clone)]
pub struct BlockFeeEntry {
    pub base_fee: u128,
    pub gas_used_ratio: f64,
    pub priority_fee_rewards: Vec<u128>,
}

// ── Filter API ───────────────────────────────────────────────────────

/// Filter variant for Ethereum-compatible filter API.
#[derive(Debug, Clone)]
pub enum Filter {
    /// Log filter (eth_newFilter)
    Log {
        from_block: u64,
        to_block: u64,
        addresses: Vec<Address>,
        topics: Vec<Option<Vec<Hash>>>,
        /// Last block number scanned for changes.
        last_block: u64,
    },
    /// Block filter (eth_newBlockFilter)
    Block {
        /// Last block height seen.
        last_height: u64,
    },
    /// Pending transaction filter (eth_newPendingTransactionFilter)
    PendingTransaction {
        /// Tx hashes already seen (dedup).
        seen: HashSet<TxHash>,
    },
}

/// In-memory filter manager for eth_newFilter / eth_getFilterChanges etc.
pub struct FilterManager {
    next_id: AtomicU64,
    filters: RwLock<HashMap<u64, Filter>>,
}

impl FilterManager {
    pub fn new() -> Self {
        Self {
            next_id: AtomicU64::new(1),
            filters: RwLock::new(HashMap::new()),
        }
    }

    pub fn create_filter(&self, filter: Filter) -> u64 {
        let id = self.next_id.fetch_add(1, Ordering::SeqCst);
        if let Ok(mut f) = self.filters.write() {
            f.insert(id, filter);
        }
        id
    }

    pub fn get_filter(&self, id: u64) -> Option<Filter> {
        self.filters.read().ok().and_then(|f| f.get(&id).cloned())
    }

    pub fn remove_filter(&self, id: u64) -> bool {
        if let Ok(mut f) = self.filters.write() {
            f.remove(&id).is_some()
        } else {
            false
        }
    }

    pub fn update_filter(&self, id: u64, filter: Filter) {
        if let Ok(mut f) = self.filters.write() {
            f.insert(id, filter);
        }
    }

    /// Remove filters older than `max_age_seconds`.
    pub fn prune_old_filters(&self, max_age_seconds: u64) {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if let Ok(mut f) = self.filters.write() {
            // Note: Filter doesn't store creation time, so we skip time-based pruning.
            // In a production system, wrap Filter with (created_at, filter).
            let _ = (now, max_age_seconds, &mut *f);
        }
    }
}

/// Sync progress data for eth_syncing.
#[derive(Debug, Clone, serde::Serialize)]
pub struct SyncProgress {
    pub starting_block: u64,
    pub current_block: u64,
    pub highest_block: u64,
}

/// Shared RPC state — all handlers read from this.
pub struct RpcState {
    pub balance_state: RwLock<AccountState>,
    pub asset_registry: RwLock<AssetRegistry>,
    pub compliance_engine: RwLock<ComplianceEngine>,
    pub evm_state: RwLock<EvmState>,
    pub bridge_state: RwLock<BridgeStateManager>,
    pub validator_state: RwLock<ValidatorStateManager>,
    pub agent_registry: RwLock<AgentRegistry>,
    pub agent_balances: RwLock<AgentBalances>,
    pub agent_nonces: RwLock<call_agent::AgentNonces>,
    pub shielded_state: RwLock<ShieldedState>,
    pub receipts: RwLock<HashMap<TxHash, ProtocolReceipt>>,
    pub current_block: RwLock<u64>,
    pub fee_params: RwLock<FeeParams>,
    pub consensus_params: RwLock<ConsensusParams>,
    pub mempool: Arc<RwLock<Mempool>>,
    pub mempool_defense: RwLock<MempoolDefense>,
    pub chain_id: u64,
    pub subscriptions: SubscriptionManager,
    pub governance: RwLock<GovernanceManager>,
    pub oracle: Arc<RwLock<OracleManager>>,
    pub fork_manager: RwLock<ForkManager>,
    pub fee_currency_registry: RwLock<FeeCurrencyRegistry>,
    /// When true, governance RPC methods require valid secp256k1 signatures.
    /// When false (default, devnet), unsigned calls are allowed.
    pub require_governance_auth: AtomicBool,
    /// Block signing key — None for full nodes, Some(signer) for validators.
    pub signer: RwLock<Option<SignerRef>>,
    /// BLS12-381 secret key for aggregated vote signing — None for full nodes.
    pub bls_secret_key: RwLock<Option<call_crypto::BlsSecretKey>>,
    #[cfg(feature = "light-client-bridge")]
    pub light_client: RwLock<Option<call_light_client::EthLightClient>>,
    /// Pending rollback plan to be applied by the node loop
    pub pending_rollback: RwLock<Option<RollbackPlan>>,
    /// Address → (block, tx_hash, log_index) for efficient eth_getLogs queries.
    pub log_index: RwLock<HashMap<Address, Vec<(u64, TxHash, usize)>>>,
    /// Data directory for loading persisted blocks from disk.
    pub data_dir: RwLock<Option<PathBuf>>,
    /// Peer → height tracking for epoch boundary quorum waiting.
    /// Key is peer_id hex string, value is the highest finalized height reported.
    pub peer_heights: Arc<RwLock<HashMap<String, u64>>>,
    /// Set to true by the network layer when sync crosses an epoch boundary,
    /// signaling the BFT event loop to restart the engine.
    pub engine_restart_signal: AtomicBool,
    /// P2P network handle — set after `start_network` is called.
    /// Used to gossip protocol transactions submitted via RPC.
    pub network: Arc<RwLock<Option<Arc<dyn call_network::Network>>>>,
    /// Ring buffer of per-block fee data for eth_feeHistory (max 1024 entries).
    pub fee_history: RwLock<VecDeque<(u64, BlockFeeEntry)>>,
    /// Block hash -> height index for eth_getBlockByHash.
    pub block_hash_index: RwLock<HashMap<Hash, u64>>,
    /// In-memory filter manager for eth_newFilter / eth_getFilterChanges etc.
    pub filter_manager: FilterManager,
    /// Sync progress for eth_syncing. None when fully synced.
    pub sync_progress: Arc<RwLock<Option<SyncProgress>>>,
}

impl RpcState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        balance_state: AccountState,
        asset_registry: AssetRegistry,
        compliance_engine: ComplianceEngine,
        evm_state: EvmState,
        bridge_state: BridgeStateManager,
        validator_state: ValidatorStateManager,
        agent_registry: AgentRegistry,
        agent_balances: AgentBalances,
        agent_nonces: call_agent::AgentNonces,
        shielded_state: ShieldedState,
        mempool: Arc<RwLock<Mempool>>,
        chain_id: u64,
        oracle: OracleManager,
    ) -> Self {
        let total_validators = validator_state.get_all_validators().len() as u32;
        Self {
            balance_state: RwLock::new(balance_state),
            asset_registry: RwLock::new(asset_registry),
            compliance_engine: RwLock::new(compliance_engine),
            evm_state: RwLock::new(evm_state),
            bridge_state: RwLock::new(bridge_state),
            validator_state: RwLock::new(validator_state),
            agent_registry: RwLock::new(agent_registry),
            agent_balances: RwLock::new(agent_balances),
            agent_nonces: RwLock::new(agent_nonces),
            shielded_state: RwLock::new(shielded_state),
            receipts: RwLock::new(HashMap::new()),
            current_block: RwLock::new(0),
            fee_params: RwLock::new(FeeParams::default()),
            consensus_params: RwLock::new(ConsensusParams::default()),
            mempool,
            mempool_defense: RwLock::new(MempoolDefense::new(1000, 1000, 10000, 100)),
            chain_id,
            subscriptions: SubscriptionManager::new(),
            governance: RwLock::new(GovernanceManager::new()),
            oracle: Arc::new(RwLock::new(oracle)),
            fork_manager: RwLock::new(ForkManager::new(
                call_primitives::ProtocolVersion::new(1, 0, 0),
                total_validators.max(1),
            )),
            fee_currency_registry: RwLock::new(FeeCurrencyRegistry::new()),
            require_governance_auth: AtomicBool::new(false),
            signer: RwLock::new(None),
            bls_secret_key: RwLock::new(None),
            #[cfg(feature = "light-client-bridge")]
            light_client: RwLock::new(None),
            pending_rollback: RwLock::new(None),
            log_index: RwLock::new(HashMap::new()),
            data_dir: RwLock::new(None),
            peer_heights: Arc::new(RwLock::new(HashMap::new())),
            engine_restart_signal: AtomicBool::new(false),
            network: Arc::new(RwLock::new(None)),
            fee_history: RwLock::new(VecDeque::new()),
            block_hash_index: RwLock::new(HashMap::new()),
            filter_manager: FilterManager::new(),
            sync_progress: Arc::new(RwLock::new(None)),
        }
    }

    pub fn get_balance(&self, asset_id: AssetId, address: &Address) -> Balance {
        self.balance_state.read().map(|s| s.get_balance(asset_id, address)).unwrap_or(0)
    }

    pub fn get_nonce(&self, address: &Address) -> u64 {
        self.balance_state.read().map(|s| s.get_nonce(address)).unwrap_or(0)
    }

    pub fn get_total_balance(&self, asset_id: AssetId) -> Balance {
        self.balance_state
            .read()
            .map(|s| s.balances.iter().filter(|((aid, _), _)| *aid == asset_id).map(|(_, b)| *b).sum())
            .unwrap_or(0)
    }

    pub fn get_asset_info(&self, asset_id: AssetId) -> Option<AssetInfoResponse> {
        self.asset_registry.read().ok().and_then(|r| {
            r.get_asset(asset_id).map(|a| AssetInfoResponse {
                id: a.id,
                symbol: a.symbol.clone(),
                name: a.name.clone(),
                decimals: a.decimals,
                issuer: a.issuer,
                protocol_supply: a.protocol_supply,
                evm_supply: a.evm_supply,
                all_supply: a.all_supply(),
                max_supply: a.max_supply,
                status: format!("{:?}", a.status),
                compliance_policy: a.compliance_policy,
                registered_at: a.registered_at,
            })
        })
    }

    pub fn get_evm_balance(&self, address: &Address) -> alloy_primitives::U256 {
        self.evm_state.read().map(|s| s.get_balance(address)).unwrap_or(alloy_primitives::U256::ZERO)
    }

    pub fn get_receipt(&self, tx_hash: &TxHash) -> Option<ProtocolReceipt> {
        self.receipts.read().ok().and_then(|r| r.get(tx_hash).cloned())
    }

    pub fn store_receipt(&self, tx_hash: TxHash, receipt: ProtocolReceipt) {
        // Index logs by address for efficient eth_getLogs queries
        for (idx, log) in receipt.logs.iter().enumerate() {
            if let Ok(mut index) = self.log_index.write() {
                index
                    .entry(log.address)
                    .or_insert_with(Vec::new)
                    .push((receipt.block_number, tx_hash, idx));
            }
        }
        // Index block hash -> height for eth_getBlockByHash
        if receipt.block_hash != Hash::ZERO {
            if let Ok(mut index) = self.block_hash_index.write() {
                index.insert(receipt.block_hash, receipt.block_number);
            }
        }
        if let Ok(mut receipts) = self.receipts.write() {
            receipts.insert(tx_hash, receipt);
        }
    }

    /// Look up logs by address filter. Returns (block, tx_hash, log_index) tuples.
    /// When no address filter is given, returns None to signal caller to fall back
    /// to full receipt scan.
    pub fn lookup_logs_by_address(
        &self,
        addresses: &[Address],
    ) -> Option<Vec<(u64, TxHash, usize)>> {
        if addresses.is_empty() {
            return None;
        }
        let index = match self.log_index.read() {
            Ok(idx) => idx,
            Err(_) => return None,
        };
        let mut result = Vec::new();
        for addr in addresses {
            if let Some(entries) = index.get(addr) {
                result.extend(entries.iter().cloned());
            }
        }
        Some(result)
    }

    /// Prune receipts older than the given block number.
    /// Called periodically to bound memory usage.
    pub fn prune_receipts(&self, max_age_blocks: u64) {
        let current = self.get_current_block();
        if current <= max_age_blocks {
            return;
        }
        let cutoff = current - max_age_blocks;
        if let Ok(mut receipts) = self.receipts.write() {
            receipts.retain(|_, r| r.block_number >= cutoff);
        }
    }

    pub fn get_receipts_by_block(&self, block: u64) -> Vec<ProtocolReceipt> {
        self.receipts
            .read()
            .map(|r| {
                r.values()
                    .filter(|receipt| receipt.block_number == block)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn get_all_receipts(&self) -> Vec<ProtocolReceipt> {
        self.receipts
            .read()
            .map(|r| r.values().cloned().collect())
            .unwrap_or_default()
    }

    pub fn get_current_block(&self) -> u64 {
        self.current_block.read().map(|b| *b).unwrap_or(0)
    }

    pub fn set_current_block(&self, block: u64) {
        if let Ok(mut b) = self.current_block.write() {
            *b = block;
        }
    }

    /// Set the data directory for loading persisted blocks from disk.
    pub fn set_data_dir(&self, dir: PathBuf) {
        if let Ok(mut d) = self.data_dir.write() {
            *d = Some(dir);
        }
    }

    /// Load a block from disk by height. Returns None if the file is missing
    /// or cannot be deserialized.
    pub fn load_block(&self,
        height: u64,
    ) -> Option<call_consensus::Block> {
        let dir = self.data_dir.read().ok()?.clone()?;
        let path = dir.join("blocks").join(format!("{height:012}.json"));
        let data = std::fs::read(&path).ok()?;
        serde_json::from_slice(&data).ok()
    }

    /// Load a block from disk by hash. Uses the in-memory hash index.
    pub fn load_block_by_hash(&self, hash: &Hash) -> Option<call_consensus::Block> {
        let height = self.block_hash_index.read().ok().and_then(|idx| idx.get(hash).copied())?;
        self.load_block(height)
    }

    /// Enable or disable governance signature requirements.
    /// When true, governance RPC methods require valid secp256k1 signatures.
    pub fn set_governance_auth(&self, require_auth: bool) {
        self.require_governance_auth.store(require_auth, Ordering::SeqCst);
    }

    pub fn register_agent(
        &self,
        owner: Address,
        pubkey: call_primitives::PublicKey,
        name: String,
        url: String,
        metadata_hash: [u8; 32],
    ) -> Result<u64, String> {
        let current_block = self.get_current_block();
        let mut registry = self.agent_registry.write().map_err(|_| "lock poisoned".to_string())?;
        registry
            .register_agent(owner, pubkey, name, url, metadata_hash, None, current_block, None)
            .map_err(|e| e.to_string())
    }

    pub fn get_agent_info(&self, agent_id: u64) -> Option<AgentInfoResponse> {
        self.agent_registry.read().ok().and_then(|r| {
            r.get_agent(agent_id).map(|a| AgentInfoResponse {
                agent_id: a.agent_id,
                owner: a.owner,
                name: a.name.clone(),
                url: a.url.clone(),
                domain_verified: a.domain_verified,
                registered_at: a.registered_at,
            })
        })
    }

    pub fn get_agent_total_balance(&self, agent_id: u64) -> Balance {
        let registry = match self.agent_registry.read() {
            Ok(r) => r,
            Err(_) => return 0,
        };
        let agent = match registry.get_agent(agent_id) {
            Some(a) => a,
            None => return 0,
        };
        self.agent_balances.read().map(|b| b.get_total_balance(agent.owner, agent_id)).unwrap_or(0)
    }

    pub fn grant_agent_balance(&self, agent_id: u64, asset_id: AssetId, amount: Balance) -> Result<(), String> {
        let registry = self.agent_registry.read().map_err(|_| "lock poisoned".to_string())?;
        let agent = registry.get_agent(agent_id).ok_or("agent not found".to_string())?;
        let owner = agent.owner;
        drop(registry);
        let mut balances = self.balance_state.write().map_err(|_| "lock poisoned".to_string())?;
        self.agent_balances.write().map_err(|_| "lock poisoned".to_string())?.grant_funds(owner, agent_id, asset_id, amount, &mut *balances)
            .map_err(|e| format!("{:?}", e))?;
        Ok(())
    }

    pub fn revoke_agent_balance(&self, agent_id: u64, asset_id: AssetId) -> Result<(), String> {
        let registry = self.agent_registry.read().map_err(|_| "lock poisoned".to_string())?;
        let agent = registry.get_agent(agent_id).ok_or("agent not found".to_string())?;
        let owner = agent.owner;
        drop(registry);
        self.agent_balances.write().map_err(|_| "lock poisoned".to_string())?.revoke_funds(owner, agent_id, asset_id);
        Ok(())
    }

    pub fn get_compliance_policy(&self, asset_id: AssetId) -> u8 {
        self.asset_registry.read().ok().and_then(|r| {
            r.get_asset(asset_id).map(|a| a.compliance_policy)
        }).unwrap_or(0)
    }

    pub fn get_shielded_tree_state(&self) -> ShieldedTreeStateResponse {
        self.shielded_state.read().map(|s| ShieldedTreeStateResponse {
            merkle_root: s.merkle_root(),
            leaf_count: s.merkle_tree.leaf_count() as u64,
            nullifier_count: s.nullifier_set.len(),
        }).unwrap_or(ShieldedTreeStateResponse {
            merkle_root: Hash::ZERO,
            leaf_count: 0,
            nullifier_count: 0,
        })
    }

    // ── EVM execution ──────────────────────────────────────────────────

    /// Execute an EVM call (read-only, state changes are rolled back)
    pub fn execute_evm_call(
        &self,
        caller: Address,
        to: Option<Address>,
        value: alloy_primitives::U256,
        data: Bytes,
        gas_limit: u64,
        gas_price: u128,
    ) -> Result<EvmExecutionResult, String> {
        let executor = EvmExecutor::new(self.chain_id);
        let mut state = self.evm_state.read().map_err(|_| "lock poisoned".to_string())?.clone();
        let nonce = state.get_nonce(&caller);

        let tx = EvmTransaction {
            caller,
            nonce,
            gas_limit,
            gas_price,
            to,
            value,
            data,
            chain_id: self.chain_id,
        };

        executor.execute_tx(tx, &mut state).map_err(|e| format!("{e}"))
    }

    // ── EVM submission (inserts into mempool + executes) ──────────────

    /// Submit a signed EVM transaction: validates, inserts into mempool, and executes
    pub fn submit_evm_tx(
        &self,
        raw_tx: &[u8],
    ) -> Result<TxHash, String> {
        let mut cursor = raw_tx;
        let envelope = TxEnvelope::decode(&mut cursor)
            .map_err(|e| format!("invalid RLP: {e}"))?;

        // Recover signer address from the signed transaction
        let signer = envelope
            .recover_signer()
            .map_err(|_| "sig recovery failed")?;
        let caller = signer;

        // Extract transaction fields for validation
        let gas_price = match &envelope {
            TxEnvelope::Legacy(signed) => signed.tx().gas_price().unwrap_or(0),
            TxEnvelope::Eip1559(signed) => signed.tx().max_fee_per_gas(),
            TxEnvelope::Eip2930(signed) => signed.tx().gas_price().unwrap_or(0),
            TxEnvelope::Eip7702(signed) => signed.tx().max_fee_per_gas(),
            TxEnvelope::Eip4844(signed) => signed.tx().max_fee_per_gas(),
        };
        let nonce = match &envelope {
            TxEnvelope::Legacy(signed) => signed.tx().nonce(),
            TxEnvelope::Eip1559(signed) => signed.tx().nonce(),
            TxEnvelope::Eip2930(signed) => signed.tx().nonce(),
            TxEnvelope::Eip7702(signed) => signed.tx().nonce(),
            TxEnvelope::Eip4844(signed) => signed.tx().nonce(),
        };
        let gas_limit = match &envelope {
            TxEnvelope::Legacy(signed) => signed.tx().gas_limit(),
            TxEnvelope::Eip1559(signed) => signed.tx().gas_limit(),
            TxEnvelope::Eip2930(signed) => signed.tx().gas_limit(),
            TxEnvelope::Eip7702(signed) => signed.tx().gas_limit(),
            TxEnvelope::Eip4844(signed) => signed.tx().gas_limit(),
        };

        // Validate nonce and balance
        {
            let state = self.evm_state.read().map_err(|_| "lock poisoned".to_string())?;
            let committed_nonce = state.get_nonce(&caller);
            let expected_nonce = {
                let mempool = self.mempool.read().map_err(|_| "lock poisoned".to_string())?;
                mempool.get_expected_evm_nonce(caller, committed_nonce)
            };
            if nonce != expected_nonce {
                return Err(format!("invalid nonce: expected {expected_nonce}, got {nonce}"));
            }
            let balance = state.get_balance(&caller);
            let max_cost = gas_price.saturating_mul(gas_limit as u128);
            if balance < alloy_primitives::U256::from(max_cost) {
                return Err("insufficient balance for gas".into());
            }
        }

        // Compute tx hash
        let tx_hash = TxHash::from_slice(&call_crypto::keccak256(raw_tx).0);

        // Extract to and value from envelope
        let (to_addr, value) = match &envelope {
            TxEnvelope::Legacy(signed) => (signed.tx().to().map(|a| Address::from(*a)), signed.tx().value()),
            TxEnvelope::Eip1559(signed) => (signed.tx().to().map(|a| Address::from(*a)), signed.tx().value()),
            TxEnvelope::Eip2930(signed) => (signed.tx().to().map(|a| Address::from(*a)), signed.tx().value()),
            TxEnvelope::Eip7702(signed) => (signed.tx().to().map(|a| Address::from(*a)), signed.tx().value()),
            TxEnvelope::Eip4844(signed) => (signed.tx().to().map(|a| Address::from(*a)), signed.tx().value()),
        };

        // Mempool defense: rate limit, replay protection, address saturation
        {
            let now_ms = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_millis() as u64;
            let mut defense = self.mempool_defense.write().map_err(|_| "lock poisoned".to_string())?;
            defense.validate_tx_submission(caller, tx_hash, now_ms)
                .map_err(|e| format!("mempool defense: {e}"))?;
        }

        // Insert into mempool (for tracking/dedup)
        {
            let mut mempool = self.mempool.write().map_err(|_| "lock poisoned".to_string())?;
            let evm_tx = call_evm::EvmTransaction {
                caller,
                nonce,
                gas_limit,
                gas_price,
                to: to_addr,
                value,
                data: alloy_primitives::Bytes::from(raw_tx.to_vec()),
                chain_id: self.chain_id,
            };
            if let Err(e) = mempool.insert_evm_tx(evm_tx) {
                // Rollback defense state so the tx can be re-submitted later
                let mut defense = self.mempool_defense.write().map_err(|_| "lock poisoned".to_string())?;
                defense.rollback_submission(caller, tx_hash);
                return Err(format!("mempool insertion failed: {e}"));
            }
        }

        // Transaction is now in the mempool and will be picked up by block production.
        // Do NOT execute immediately — that would cause double-execution when the
        // consensus layer includes this tx in a block.

        // Broadcast pending tx for eth_subscribe("newPendingTransactions")
        self.subscriptions.broadcast_eth_pending_tx(format!("0x{}", hex::encode(tx_hash.as_slice())));

        Ok(tx_hash)
    }

    // ── Protocol transaction submission (inserts into mempool + executes) ────────

    /// Submit a protocol payment transaction: validates and inserts into mempool only.
    /// Execution is deferred to block production via Block::execute.
    #[allow(dead_code)]
    pub fn submit_payment(
        &self,
        sender: Address,
        nonce: u64,
        asset_id: AssetId,
        to: Address,
        amount: Balance,
        memo: Option<String>,
        gas_limit: u64,
        max_fee: u128,
        signature: Option<[u8; 65]>,
    ) -> Result<TxHash, String> {
        use call_protocol::{
            Instruction, PaymentMemo,
        };

        // Build instructions
        let instructions = vec![Instruction::Transfer {
            asset_id,
            to,
            amount,
            memo: memo.map(|m| PaymentMemo {
                message: m,
                reference: None,
                metadata: None,
            }),
        }];

        // Build protocol transaction
        let sig = signature.ok_or_else(|| "signature required for payment".to_string())?;
        let tx = call_protocol::transaction::ProtocolTransaction {
            sender,
            nonce,
            instructions,
            gas_config: call_protocol::transaction::GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit,
            max_fee,
            max_priority_fee: call_protocol::transaction::MIN_PRIORITY_FEE_PER_GAS,
            expires_at: 0,
            auth: call_protocol::transaction::AuthScheme::SingleSig {
                signature: sig,
            },
        };

        // Verify signature and insert into mempool (execution deferred to consensus)
        self.insert_protocol_tx(tx)
    }

    /// Insert a pre-built protocol transaction into the mempool without executing.
    /// Used for governance, bridge, and other consensus-ordered operations.
    pub fn insert_protocol_tx(
        &self,
        tx: call_protocol::transaction::ProtocolTransaction,
    ) -> Result<call_primitives::TxHash, String> {
        let tx_hash = call_primitives::TxHash::from_slice(&tx.compute_tx_hash());

        tx.verify_signature()
            .map_err(|e| format!("signature verification failed: {e}"))?;

        // Pre-validate validator instructions to prevent obviously-invalid txs
        // from entering the mempool and poisoning block proposals.
        for instr in &tx.instructions {
            match instr {
                call_protocol::Instruction::ValidatorUnstake { validator_id } => {
                    let vs = self.validator_state.read()
                        .map_err(|_| "validator lock poisoned".to_string())?;
                    if let Some(stake) = vs.get_validator_stake(*validator_id) {
                        if stake.address != tx.sender {
                            return Err("unstake: sender must be the validator owner".into());
                        }
                    }
                }
                call_protocol::Instruction::ValidatorClaimUnbonded { validator_id } => {
                    let vs = self.validator_state.read()
                        .map_err(|_| "validator lock poisoned".to_string())?;
                    if let Some(stake) = vs.get_validator_stake(*validator_id) {
                        if stake.address != tx.sender {
                            return Err("claim: sender must be the validator owner".into());
                        }
                    }
                }
                _ => {}
            }
        }

        let mut mempool = self.mempool.write().map_err(|_| "lock poisoned".to_string())?;
        if let Err(e) = mempool.insert_protocol_tx(tx.clone()) {
            return Err(format!("mempool insertion failed: {e}"));
        }
        drop(mempool);

        // Gossip to peers so non-proposer validators also see the tx
        if let Ok(net_guard) = self.network.read() {
            if let Some(ref net) = *net_guard {
                let data = match serde_json::to_vec(&tx) {
                    Ok(d) => d,
                    Err(e) => {
                        tracing::warn!(error = %e, "failed to serialize protocol tx for gossip");
                        return Ok(tx_hash);
                    }
                };
                let msg = call_network::TransactionMessage::new(data, tx_hash);
                let net_clone = Arc::clone(net);
                tokio::spawn(async move {
                    let payload = match serde_json::to_vec(&msg) {
                        Ok(p) => p,
                        Err(e) => {
                            tracing::warn!(error = %e, "failed to serialize TransactionMessage");
                            return;
                        }
                    };
                    net_clone.broadcast(1, payload).await;
                });
            }
        }

        // Broadcast pending tx for eth_subscribe("newPendingTransactions")
        self.subscriptions.broadcast_eth_pending_tx(format!("0x{}", hex::encode(tx_hash.as_slice())));

        Ok(tx_hash)
    }

    /// Prune expired transactions from the mempool and confirm included ones.
    /// Called after each block.
    pub fn finalize_block(&self) {
        if let Ok(mut mempool) = self.mempool.write() {
            mempool.prune_expired();
        }
        // Prune receipts older than 1000 blocks to bound memory growth
        self.prune_receipts(1000);
    }
}
