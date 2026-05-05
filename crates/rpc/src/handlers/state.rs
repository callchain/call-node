//! RpcState struct and its methods.

use call_protocol::{ProtocolReceipt, FeeParams};
use call_protocol::security::MempoolDefense;
use call_evm::{EvmExecutor, EvmTransaction, EvmExecutionResult};
use call_consensus::{ForkManager, RollbackPlan, ConsensusParams};
use call_consensus::exec::state_accessors;
use call_primitives::{Address, AssetId, Balance, TxHash, Hash};
use call_crypto::SignerRef;
use call_mempool::Mempool;
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
use reth_db::DatabaseEnv;

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
///
/// State is backed by MDBX (`db_env`).  There is no long-lived in-memory
/// overlay; state is loaded on demand for each operation.
pub struct RpcState {
    pub receipts: RwLock<HashMap<TxHash, ProtocolReceipt>>,
    pub current_block: RwLock<u64>,
    pub fee_params: RwLock<FeeParams>,
    pub consensus_params: RwLock<ConsensusParams>,
    pub mempool: Arc<RwLock<Mempool>>,
    pub mempool_defense: RwLock<MempoolDefense>,
    pub chain_id: u64,
    pub subscriptions: SubscriptionManager,
    pub fork_manager: RwLock<ForkManager>,
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
    /// MDBX database environment — the single source of truth for all state.
    pub db_env: Arc<DatabaseEnv>,
}

impl RpcState {
    pub fn new(
        db_env: Arc<DatabaseEnv>,
        mempool: Arc<RwLock<Mempool>>,
        chain_id: u64,
    ) -> Self {
        let total_validators = {
            let provider = call_evm::provider::InMemoryStateProvider::from_db(&db_env).ok();
            let count = provider
                .map(|p| call_consensus::exec::state_accessors::read_validator_count(&p))
                .unwrap_or(0);
            count as u32
        };
        Self {
            receipts: RwLock::new(HashMap::new()),
            current_block: RwLock::new(0),
            fee_params: RwLock::new(FeeParams::default()),
            consensus_params: RwLock::new(ConsensusParams::default()),
            mempool,
            mempool_defense: RwLock::new(MempoolDefense::new(1000, 1000, 10000, 2000)),
            chain_id,
            subscriptions: SubscriptionManager::new(),
            fork_manager: RwLock::new(ForkManager::new(
                call_primitives::ProtocolVersion::new(1, 0, 0),
                total_validators.max(1),
            )),
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
            db_env,
        }
    }

    // ── Internal helpers ───────────────────────────────────────────────

    /// Load an [`InMemoryStateProvider`] from MDBX.
    fn load_provider(&self) -> Result<call_evm::provider::InMemoryStateProvider, String> {
        call_evm::provider::InMemoryStateProvider::from_db(&self.db_env)
            .map_err(|e| format!("db load error: {e}"))
    }

    /// Load provider, apply a mutation, and save back to MDBX.
    fn with_provider_mut<F, T>(&self, mut f: F) -> Result<T, String>
    where
        F: FnMut(&mut call_evm::provider::InMemoryStateProvider) -> T,
    {
        let mut provider = self.load_provider()?;
        let result = f(&mut provider);
        provider
            .state()
            .save_to_db(&self.db_env)
            .map_err(|e| format!("db save error: {e}"))?;
        Ok(result)
    }

    pub fn get_balance(&self, asset_id: AssetId, address: &Address) -> Balance {
        self.load_provider()
            .map(|p| state_accessors::read_balance(&p, asset_id, *address))
            .unwrap_or(0)
    }

    pub fn get_nonce(&self, address: &Address) -> u64 {
        self.load_provider()
            .map(|p| p.state().get_nonce(address))
            .unwrap_or(0)
    }

    pub fn get_total_balance(&self, asset_id: AssetId) -> Balance {
        self.load_provider()
            .map(|p| state_accessors::read_asset_supply(&p, asset_id))
            .unwrap_or(0)
    }

    pub fn get_asset_info(&self, asset_id: AssetId) -> Option<AssetInfoResponse> {
        let provider = self.load_provider().ok()?;
        let symbol = state_accessors::read_asset_symbol(&provider, asset_id);
        if symbol.is_empty() {
            return None;
        }
        let supply = state_accessors::read_asset_supply(&provider, asset_id);
        Some(AssetInfoResponse {
            id: asset_id,
            symbol,
            name: state_accessors::read_asset_name(&provider, asset_id),
            decimals: state_accessors::read_asset_decimals(&provider, asset_id),
            issuer: state_accessors::read_asset_issuer(&provider, asset_id),
            protocol_supply: supply,
            evm_supply: supply,
            all_supply: supply,
            max_supply: state_accessors::read_asset_max_supply(&provider, asset_id),
            status: match state_accessors::read_asset_status(&provider, asset_id) {
                0 => "Active".to_string(),
                1 => "Frozen".to_string(),
                2 => "Delisted".to_string(),
                _ => "Unknown".to_string(),
            },
            compliance_policy: state_accessors::read_asset_compliance(&provider, asset_id),
            registered_at: state_accessors::read_asset_registered_at(&provider, asset_id),
        })
    }

    pub fn get_evm_balance(&self, address: &Address) -> alloy_primitives::U256 {
        self.load_provider()
            .map(|p| p.state().get_balance(address))
            .unwrap_or(alloy_primitives::U256::ZERO)
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
        _metadata_hash: [u8; 32],
    ) -> Result<u64, String> {
        let current_block = self.get_current_block();
        self.with_provider_mut(|provider| {
            let count = call_consensus::exec::state_accessors::read_agent_count(provider);
            call_consensus::exec::state_accessors::seed_agent(
                provider, count, owner, &name, &url, current_block,
            );
            let pubkey_hash: [u8; 32] = if pubkey.len() >= 32 {
                pubkey[..32].try_into().unwrap()
            } else {
                let mut buf = [0u8; 32];
                buf[..pubkey.len()].copy_from_slice(&pubkey);
                buf
            };
            call_consensus::exec::state_accessors::agent_set_pubkey(provider, count, &pubkey_hash);
            count
        })
    }

    pub fn get_agent_info(&self, agent_id: u64) -> Option<AgentInfoResponse> {
        let provider = self.load_provider().ok()?;
        if !state_accessors::agent_exists(&provider, agent_id) {
            return None;
        }
        Some(AgentInfoResponse {
            agent_id,
            owner: state_accessors::agent_get_owner(&provider, agent_id),
            name: state_accessors::agent_get_name(&provider, agent_id),
            url: state_accessors::agent_get_url(&provider, agent_id),
            domain_verified: false,
            registered_at: state_accessors::agent_get_registered_at(&provider, agent_id),
        })
    }

    pub fn get_agent_total_balance(&self, agent_id: u64) -> Balance {
        self.load_provider()
            .map(|p| state_accessors::agent_get_balance(&p, agent_id, call_protocol::CALL_ASSET_ID))
            .unwrap_or(0)
    }

    pub fn grant_agent_balance(&self, agent_id: u64, asset_id: AssetId, amount: Balance) -> Result<(), String> {
        self.with_provider_mut(|provider| {
            let owner = call_consensus::exec::state_accessors::agent_get_owner(provider, agent_id);
            if owner == call_primitives::Address::ZERO {
                return Err("agent not found".into());
            }
            let owner_balance = call_consensus::exec::state_accessors::read_balance(provider, asset_id, owner);
            if owner_balance < amount {
                return Err("insufficient owner balance for grant".into());
            }
            call_consensus::exec::state_accessors::seed_balance(provider, asset_id, owner, owner_balance - amount);
            let agent_balance = call_consensus::exec::state_accessors::agent_get_balance(provider, agent_id, asset_id);
            call_consensus::exec::state_accessors::agent_set_balance(provider, agent_id, asset_id, agent_balance + amount);
            Ok(())
        })?
    }

    pub fn revoke_agent_balance(&self, agent_id: u64, asset_id: AssetId) -> Result<(), String> {
        self.with_provider_mut(|provider| {
            let owner = call_consensus::exec::state_accessors::agent_get_owner(provider, agent_id);
            if owner == call_primitives::Address::ZERO {
                return Err("agent not found".into());
            }
            call_consensus::exec::state_accessors::agent_set_balance(provider, agent_id, asset_id, 0);
            Ok(())
        })?
    }

    pub fn get_compliance_policy(&self, asset_id: AssetId) -> u8 {
        self.load_provider()
            .map(|p| state_accessors::read_asset_compliance(&p, asset_id))
            .unwrap_or(0)
    }

    pub fn get_shielded_tree_state(&self) -> ShieldedTreeStateResponse {
        self.load_provider()
            .map(|p| ShieldedTreeStateResponse {
                merkle_root: call_consensus::exec::state_accessors::read_shielded_merkle_root(&p),
                leaf_count: call_consensus::exec::state_accessors::read_shielded_commitment_count(&p),
                nullifier_count: 0,
            })
            .unwrap_or(ShieldedTreeStateResponse {
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
        let state = self.load_provider()?;
        let nonce = state.state().get_nonce(&caller);

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
        let base_fee = self.fee_params.read().map_err(|_| "lock poisoned".to_string())?.base_fee;

        let (result, _delta) = executor.execute_tx_provider(tx, state, 0, base_fee).map_err(|e| format!("{e}"))?;
        Ok(result)
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
            let provider = self.load_provider().map_err(|e| e.to_string())?;
            let committed_nonce = provider.state().get_nonce(&caller);
            let expected_nonce = {
                let mempool = self.mempool.read().map_err(|_| "lock poisoned".to_string())?;
                mempool.get_expected_evm_nonce(caller, committed_nonce)
            };
            if nonce != expected_nonce {
                return Err(format!("invalid nonce: expected {expected_nonce}, got {nonce}"));
            }
            let balance = provider.state().get_balance(&caller);
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

    // ── Protocol transaction submission (EVM-only mempool) ───────────

    /// Submit a protocol payment transaction.
    /// DEPRECATED: The mempool is now EVM-only. Protocol transactions must be
    /// submitted as EVM transactions via `submit_evm_tx`.
    #[allow(dead_code)]
    pub fn submit_payment(
        &self,
        _sender: Address,
        _nonce: u64,
        _asset_id: AssetId,
        _to: Address,
        _amount: Balance,
        _memo: Option<String>,
        _gas_limit: u64,
        _max_fee: u128,
        _signature: Option<[u8; 65]>,
    ) -> Result<TxHash, String> {
        Err("protocol transactions are no longer accepted directly; submit via eth_sendRawTransaction".into())
    }

    /// Insert a pre-built protocol transaction into the mempool.
    /// DEPRECATED: The mempool is now EVM-only. Returns an error.
    pub fn insert_protocol_tx(
        &self,
        _tx: Vec<u8>,
    ) -> Result<call_primitives::TxHash, String> {
        Err("protocol transactions are no longer accepted directly; submit via eth_sendRawTransaction".into())
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
