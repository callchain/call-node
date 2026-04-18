//! Core handler trait and RPC state management.

use call_protocol::{BalanceState, AssetRegistry, ComplianceEngine, ProtocolReceipt, InstructionExecResult, FeeParams, FeeCurrencyRegistry};
use call_governance::{GovernanceManager, ProposalExecutor, Proposal};
use call_oracle::OracleManager;
use call_evm::{EvmState, EvmExecutor, EvmTransaction, EvmExecutionResult};
use call_bridge::BridgeStateManager;
use call_consensus::{ValidatorStateManager, ForkManager, ForkError};
use call_agent::{AgentRegistry, AgentBalances};
use call_shielded::ShieldedState;
use call_primitives::{Address, AssetId, Balance, TxHash, Hash, PublicKey};
use call_crypto::SignerRef;
use call_transaction_pool::Mempool;
use alloy_consensus::{TxEnvelope, Transaction as _, transaction::SignerRecoverable};
use alloy_primitives::Bytes;
use alloy_rlp::Decodable;
use crate::ws::SubscriptionManager;
use jsonrpsee::types::ErrorObjectOwned;
use std::sync::atomic::{AtomicBool, Ordering};
use std::collections::HashMap;
use std::sync::{Arc, RwLock};

/// Shared RPC state — all handlers read from this.
pub struct RpcState {
    pub balance_state: RwLock<BalanceState>,
    pub asset_registry: RwLock<AssetRegistry>,
    pub compliance_engine: RwLock<ComplianceEngine>,
    pub evm_state: RwLock<EvmState>,
    pub bridge_state: RwLock<BridgeStateManager>,
    pub validator_state: RwLock<ValidatorStateManager>,
    pub agent_registry: RwLock<AgentRegistry>,
    pub agent_balances: RwLock<AgentBalances>,
    pub shielded_state: RwLock<ShieldedState>,
    pub receipts: RwLock<HashMap<TxHash, ProtocolReceipt>>,
    pub current_block: RwLock<u64>,
    pub fee_params: RwLock<FeeParams>,
    pub mempool: Arc<RwLock<Mempool>>,
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
    #[cfg(feature = "light-client-bridge")]
    pub light_client: RwLock<Option<call_light_client::EthLightClient>>,
}

impl RpcState {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        balance_state: BalanceState,
        asset_registry: AssetRegistry,
        compliance_engine: ComplianceEngine,
        evm_state: EvmState,
        bridge_state: BridgeStateManager,
        validator_state: ValidatorStateManager,
        agent_registry: AgentRegistry,
        agent_balances: AgentBalances,
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
            shielded_state: RwLock::new(shielded_state),
            receipts: RwLock::new(HashMap::new()),
            current_block: RwLock::new(0),
            fee_params: RwLock::new(FeeParams::default()),
            mempool,
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
            #[cfg(feature = "light-client-bridge")]
            light_client: RwLock::new(None),
        }
    }

    pub fn get_balance(&self, asset_id: AssetId, address: &Address) -> Balance {
        self.balance_state.read().map(|s| s.get_balance(asset_id, address)).unwrap_or(0)
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
                total_supply: a.total_supply,
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
        if let Ok(mut receipts) = self.receipts.write() {
            receipts.insert(tx_hash, receipt);
        }
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

    /// Enable or disable governance signature requirements.
    /// When true, governance RPC methods require valid secp256k1 signatures.
    pub fn set_governance_auth(&self, require_auth: bool) {
        self.require_governance_auth.store(require_auth, Ordering::SeqCst);
    }

    pub fn register_agent(
        &self,
        owner: Address,
        pubkey: PublicKey,
        name: String,
        url: String,
        metadata_hash: [u8; 32],
    ) -> Result<u64, String> {
        let current_block = self.get_current_block();
        let mut registry = self.agent_registry.write().map_err(|_| "lock poisoned".to_string())?;
        registry
            .register_agent(owner, pubkey, name, url, metadata_hash, None, current_block)
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
        self.agent_balances.write().map_err(|_| "lock poisoned".to_string())?.grant_funds(owner, agent_id, asset_id, amount);
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
            let expected_nonce = state.get_nonce(&caller);
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
            let _ = mempool.insert_evm_tx(evm_tx);
        }

        // Execute immediately
        let tx = match &envelope {
            TxEnvelope::Legacy(signed) => {
                let tx = signed.tx();
                EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.gas_price().unwrap_or(0),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(self.chain_id),
                }
            }
            TxEnvelope::Eip1559(signed) => {
                let tx = signed.tx();
                EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.max_fee_per_gas(),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(self.chain_id),
                }
            }
            TxEnvelope::Eip2930(signed) => {
                let tx = signed.tx();
                EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.gas_price().unwrap_or(0),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(self.chain_id),
                }
            }
            TxEnvelope::Eip7702(signed) => {
                let tx = signed.tx();
                EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.max_fee_per_gas(),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(self.chain_id),
                }
            }
            TxEnvelope::Eip4844(signed) => {
                let tx = signed.tx();
                EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.max_fee_per_gas(),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(self.chain_id),
                }
            }
        };

        let executor = EvmExecutor::new(self.chain_id);
        let mut state = self.evm_state.write().map_err(|_| "lock poisoned".to_string())?;
        let result = executor.execute_tx(tx, &mut state).map_err(|e| format!("{e}"))?;

        // Store receipt
        use call_primitives::ExecutionStatus;
        let status = if result.success {
            ExecutionStatus::Success
        } else {
            ExecutionStatus::Reverted { reason: "execution reverted".into() }
        };
        let receipt = ProtocolReceipt {
            tx_hash,
            status,
            gas_used: result.gas_used,
            gas_payer: caller,
            fee_currency: call_primitives::FeeCurrency::Call,
            fee_amount: result.gas_used as u128 * gas_price,
            block_number: self.get_current_block(),
            instruction_results: vec![InstructionExecResult {
                success: result.success,
                gas_used: result.gas_used,
                revert_reason: if result.success { None } else { Some("reverted".into()) },
            }],
            logs: vec![],
            memos: vec![],
            state_changes: vec![],
        };
        self.store_receipt(tx_hash, receipt);

        // Broadcast EVM transaction to WebSocket subscribers
        self.subscriptions.broadcast_payment(
            format!("0x{}", hex::encode(tx_hash)),
            format!("0x{}", hex::encode(caller.as_slice())),
            to_addr.map(|a| format!("0x{}", hex::encode(a.as_slice()))).unwrap_or_else(|| "contract_creation".into()),
            0,
            value.try_into().unwrap_or(0),
        );

        Ok(tx_hash)
    }

    // ── Protocol transaction submission (inserts into mempool + executes) ────────

    /// Submit a protocol payment transaction: validates, inserts into mempool, and executes
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
            execute_protocol_instructions,
        };

        // Check balance sufficiency
        {
            let balances = self.balance_state.read().map_err(|_| "lock poisoned".to_string())?;
            let balance = balances.get_balance(asset_id, &sender);
            if balance < amount {
                return Err("insufficient balance".into());
            }
        }

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

        // Build tx hash preimage — include all fields to prevent malleability
        let mut preimage = Vec::new();
        preimage.extend_from_slice(sender.as_slice());
        preimage.extend_from_slice(&nonce.to_be_bytes());
        preimage.extend_from_slice(&asset_id.to_be_bytes());
        preimage.extend_from_slice(to.as_slice());
        preimage.extend_from_slice(&amount.to_be_bytes());
        preimage.extend_from_slice(&gas_limit.to_be_bytes());
        preimage.extend_from_slice(&max_fee.to_be_bytes());
        let tx_hash = TxHash::from_slice(&call_crypto::keccak256(&preimage).0);

        // Build protocol transaction
        let tx = call_protocol::transaction::ProtocolTransaction {
            sender,
            nonce,
            instructions: instructions.clone(),
            gas_config: call_protocol::transaction::GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit,
            max_fee,
            auth: call_protocol::transaction::AuthScheme::SingleSig {
                signature: signature.unwrap_or_else(|| {
                    // Derive a deterministic placeholder signature from tx hash preimage
                    let hash = call_crypto::keccak256(&preimage);
                    let mut sig = [0u8; 65];
                    sig[..32].copy_from_slice(&hash.0[..32]);
                    sig
                }),
            },
        };

        // Insert into mempool (for tracking/dedup)
        {
            let mut mempool = self.mempool.write().map_err(|_| "lock poisoned".to_string())?;
            let _ = mempool.insert_protocol_tx(tx);
        }

        // Execute immediately
        let mut balances = self.balance_state.write().map_err(|_| "lock poisoned".to_string())?;
        let registry_guard = self.asset_registry.read().map_err(|_| "lock poisoned".to_string())?;
        let mut compliance_guard = self.compliance_engine.write().map_err(|_| "lock poisoned".to_string())?;
        let mut shielded_state = self.shielded_state.write().map_err(|_| "lock poisoned".to_string())?;

        match execute_protocol_instructions(&instructions, &mut balances, &registry_guard, &mut compliance_guard, &mut shielded_state, sender, None) {
            Ok(results) => {
                let status = call_primitives::ExecutionStatus::Success;
                let gas_used = results.iter().map(|r| match r {
                    call_protocol::InstructionResult::Success => 10_000u64,
                    call_protocol::InstructionResult::Reverted { .. } => 0u64,
                }).sum();
                let receipt = ProtocolReceipt {
                    tx_hash,
                    status,
                    gas_used,
                    gas_payer: sender,
                    fee_currency: call_primitives::FeeCurrency::Call,
                    fee_amount: gas_used as u128 * 10,
                    block_number: self.get_current_block(),
                    instruction_results: results.into_iter().map(|r| InstructionExecResult {
                        success: matches!(r, call_protocol::InstructionResult::Success),
                        gas_used: 10_000,
                        revert_reason: None,
                    }).collect(),
                    logs: vec![],
                    memos: vec![],
                    state_changes: vec![],
                };
                drop(balances);
                self.store_receipt(tx_hash, receipt);

                // Broadcast payment to WebSocket subscribers
                self.subscriptions.broadcast_payment(
                    format!("0x{}", hex::encode(tx_hash)),
                    format!("0x{}", hex::encode(sender.as_slice())),
                    format!("0x{}", hex::encode(to.as_slice())),
                    asset_id,
                    amount,
                );

                Ok(tx_hash)
            }
            Err(e) => {
                let receipt = ProtocolReceipt {
                    tx_hash,
                    status: call_primitives::ExecutionStatus::Reverted { reason: e.to_string() },
                    gas_used: 0,
                    gas_payer: sender,
                    fee_currency: call_primitives::FeeCurrency::Call,
                    fee_amount: 0,
                    block_number: self.get_current_block(),
                    instruction_results: vec![],
                    logs: vec![],
                    memos: vec![],
                    state_changes: vec![],
                };
                drop(balances);
                self.store_receipt(tx_hash, receipt);
                Err(format!("execution failed: {e}"))
            }
        }
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

/// Node-level proposal executor that dispatches to subsystems.
pub struct NodeProposalExecutor {
    state: Arc<RpcState>,
}

impl ProposalExecutor for NodeProposalExecutor {
    fn on_proposal_executed(&self, proposal: &Proposal) -> Result<(), String> {
        use call_governance::ProposalType;

        match &proposal.proposal_type {
            ProposalType::ProtocolUpgrade { activation_block, changelog } => {
                // Parse version from changelog (format: "vX.Y.Z")
                let version = parse_version(changelog).unwrap_or_else(|| {
                    call_primitives::ProtocolVersion::new(1, 0, 0)
                });
                let current = self.state.get_current_block();
                let mut fm = self.state.fork_manager.write().map_err(|_| "fork lock poisoned".to_string())?;
                fm.schedule_governance_upgrade(version, *activation_block, proposal.id, current)
                    .map_err(|e| format!("fork upgrade: {e:?}"))?;
                tracing::info!(version = ?version, height = activation_block, proposal_id = proposal.id, "governance protocol upgrade scheduled");
            }
            ProposalType::ValidatorSlash { validator_id, reason } => {
                // Remove validator from consensus state and slash self-stake
                let mut vs = self.state.validator_state.write().map_err(|_| "validator lock poisoned".to_string())?;
                let slashed = vs.remove_validator(*validator_id)
                    .map_err(|e| format!("failed to slash validator: {e}"))?;
                // Return slashed amount to caller (in production, would transfer to treasury)
                let _ = slashed;
                tracing::info!(validator_id, reason, slashed_amount = slashed, "validator slashed via governance");
            }
            ProposalType::EmergencyPause { reason } => {
                // Already handled by GovernanceManager.apply_proposal
                tracing::info!(reason, "emergency pause confirmed via executor");
            }
            ProposalType::ParameterChange { param_id, new_value } => {
                // Parse new_value as JSON
                match serde_json::from_str::<serde_json::Value>(new_value) {
                    Ok(val) => {
                        // Governance config updates: param_id starts with "governance."
                        if param_id.starts_with("governance.") {
                            let mut gov = self.state.governance.write().map_err(|_| "governance lock poisoned".to_string())?;
                            if let Some(v) = val.get("validator_quorum_bps").and_then(|v| v.as_u64()) {
                                gov.config.validator_quorum_bps = v as u32;
                            }
                            if let Some(v) = val.get("supply_quorum_bps").and_then(|v| v.as_u64()) {
                                gov.config.supply_quorum_bps = v as u32;
                            }
                            if let Some(v) = val.get("treasury_quorum_bps").and_then(|v| v.as_u64()) {
                                gov.config.treasury_quorum_bps = v as u32;
                            }
                            if let Some(v) = val.get("simple_majority_bps").and_then(|v| v.as_u64()) {
                                gov.config.simple_majority_bps = v as u32;
                            }
                            if let Some(v) = val.get("review_period_blocks").and_then(|v| v.as_u64()) {
                                gov.config.review_period_blocks = v;
                            }
                            if let Some(v) = val.get("voting_period_blocks").and_then(|v| v.as_u64()) {
                                gov.config.voting_period_blocks = v;
                            }
                            if let Some(v) = val.get("timelock_period_blocks").and_then(|v| v.as_u64()) {
                                gov.config.timelock_period_blocks = v;
                            }
                            if let Some(v) = val.get("execution_timeout_blocks").and_then(|v| v.as_u64()) {
                                gov.config.execution_timeout_blocks = v;
                            }
                            tracing::info!(param_id, new_value, "governance config updated via executor");
                        } else {
                            // Standard fee params updates
                            let mut fp = self.state.fee_params.write().map_err(|_| "fee params lock poisoned".to_string())?;
                            if let Some(v) = val.get("base_fee").and_then(|v| v.as_u64()) {
                                fp.base_fee = v as u128;
                            }
                            if let Some(v) = val.get("target_gas_per_block").and_then(|v| v.as_u64()) {
                                fp.target_gas_per_block = v;
                            }
                            if let Some(v) = val.get("max_gas_per_block").and_then(|v| v.as_u64()) {
                                fp.max_gas_per_block = v;
                            }
                            if let Some(v) = val.get("oracle_fee_share_bps").and_then(|v| v.as_u64()) {
                                fp.oracle_fee_share_bps = v as u16;
                            }
                            tracing::info!(param_id, new_value, "parameter change applied via executor");
                        }
                    }
                    Err(e) => {
                        tracing::warn!(param_id, error = %e, "failed to parse parameter change value as JSON, skipping");
                    }
                }
            }
            ProposalType::TreasurySpend { recipient, amount, asset_id } => {
                // Already applied by GovernanceManager.apply_proposal
                tracing::info!(asset_id, amount, ?recipient, "treasury spend confirmed via executor");
            }
            ProposalType::ComplianceUpdate { asset_id, new_policy } => {
                // Map policy u8 to CompliancePolicy and set asset compliance
                let policy = match *new_policy {
                    0 => 0, // None
                    1 => 1, // OfacBlacklist
                    2 => 2, // KycRequired
                    3 => 3, // Whitelist
                    4 => 4, // Custom
                    _ => return Err(format!("unknown compliance policy id: {new_policy}")),
                };
                // Update the asset registry with the new policy
                let mut registry = self.state.asset_registry.write().map_err(|_| "asset registry lock poisoned".to_string())?;
                if let Some(asset) = registry.get_asset_mut(*asset_id) {
                    asset.compliance_policy = policy;
                    tracing::info!(asset_id, new_policy, "compliance update applied via executor");
                } else {
                    return Err(format!("asset {asset_id} not found for compliance update"));
                }
            }
            ProposalType::FeeCurrencyAdd { asset_id, name, oracle_price_key } => {
                let key_bytes: Option<[u8; 32]> = if oracle_price_key.is_empty() {
                    None
                } else {
                    let mut arr = [0u8; 32];
                    let bytes = hex::decode(oracle_price_key.trim_start_matches("0x")).unwrap_or_default();
                    let len = bytes.len().min(32);
                    arr[..len].copy_from_slice(&bytes[..len]);
                    Some(arr)
                };
                let current_block = self.state.get_current_block();
                let entry = call_protocol::FeeCurrencyEntry {
                    asset_id: *asset_id,
                    name: name.clone(),
                    decimals: 18,
                    oracle_price_key: key_bytes,
                    added_at_block: current_block,
                    added_by_proposal: proposal.id,
                };
                let mut registry = self.state.fee_currency_registry.write().map_err(|_| "fee currency registry lock poisoned".to_string())?;
                registry.add_fee_currency(entry, proposal.id)
                    .map_err(|e| format!("failed to add fee currency: {e}"))?;
                tracing::info!(asset_id, name, "fee currency registered via governance");
            }
            ProposalType::FeeCurrencyRemove { asset_id, grace_period_blocks } => {
                let mut registry = self.state.fee_currency_registry.write().map_err(|_| "fee currency registry lock poisoned".to_string())?;
                registry.remove_fee_currency(*asset_id, *grace_period_blocks)
                    .map_err(|e| format!("failed to remove fee currency: {e}"))?;
                tracing::info!(asset_id, grace_period_blocks, "fee currency removed via governance");
            }
            ProposalType::FeeCurrencyCap { new_cap_bps } => {
                let mut registry = self.state.fee_currency_registry.write().map_err(|_| "fee currency registry lock poisoned".to_string())?;
                registry.stablecoin_cap_bps = *new_cap_bps;
                tracing::info!(new_cap_bps, "fee currency cap updated via governance");
            }
            ProposalType::ValidatorKeyRotation { validator_id, old_pubkey, new_pubkey, signature } => {
                // Verify the old key signed the rotation request
                let msg = {
                    let mut buf = Vec::with_capacity(64);
                    buf.extend_from_slice(old_pubkey);
                    buf.extend_from_slice(new_pubkey);
                    buf
                };
                let msg_hash = call_crypto::keccak256(&msg);

                // Recover signer address from signature
                if signature.len() != 65 {
                    return Err("rotation signature must be 65 bytes".into());
                }
                let sig_arr: [u8; 65] = signature.as_slice().try_into().map_err(|_| "invalid signature length")?;
                let recovered = call_crypto::recover_secp256k1_signer(&msg_hash, &sig_arr)
                    .map_err(|e| format!("failed to recover signer from rotation signature: {e}"))?;

                // Look up the validator's current address
                let vs = self.state.validator_state.read().map_err(|_| "validator lock poisoned".to_string())?;
                let validator = vs.get_validator_stake(*validator_id)
                    .ok_or_else(|| format!("validator {validator_id} not found"))?;

                // The recovered address must match the validator's address
                let validator_addr = validator.address;
                drop(vs);

                if recovered != validator_addr {
                    return Err(format!("rotation signature from wrong address: expected {validator_addr:?}, got {recovered:?}"));
                }

                // Rotate the key in consensus
                let mut vs = self.state.validator_state.write().map_err(|_| "validator lock poisoned".to_string())?;
                vs.rotate_key(*validator_id, *old_pubkey, *new_pubkey)
                    .map_err(|e| format!("key rotation failed: {e}"))?;
                tracing::info!(validator_id, "validator key rotated via governance");
            }
        }

        Ok(())
    }
}

/// Parse a version string like "v1.2.3" into ProtocolVersion.
fn parse_version(s: &str) -> Option<call_primitives::ProtocolVersion> {
    let s = s.trim_start_matches('v');
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() >= 2 {
        let major = parts[0].parse::<u16>().ok()?;
        let minor = parts[1].parse::<u16>().ok()?;
        let patch = parts.get(2).and_then(|p| p.parse::<u16>().ok()).unwrap_or(0);
        Some(call_primitives::ProtocolVersion::new(major, minor, patch))
    } else {
        None
    }
}

/// Wire the governance executor so proposals can trigger real side effects.
/// Called after RpcState is wrapped in Arc.
pub fn wire_governance_executor(state: &Arc<RpcState>) {
    let executor = Arc::new(NodeProposalExecutor {
        state: Arc::clone(state),
    });
    let balance_source = {
        let state = Arc::clone(state);
        Arc::new(move |addr: Address| {
            state.balance_state.read().ok().map(|s| s.balances.get_balance(0, &addr)).unwrap_or(0)
        })
    };
    if let Ok(mut gov) = state.governance.write() {
        gov.executor = Some(executor);
        gov.balance_source = Some(balance_source);
    }
}

/// Helper: create an invalid params error
pub fn invalid_params(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32602, msg, None::<()>)
}

/// Helper: create an internal error
pub fn internal_error(msg: String) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32603, msg, None::<()>)
}

/// Response types

#[derive(Debug, Clone, serde::Serialize)]
pub struct AssetInfoResponse {
    pub id: AssetId,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub issuer: Address,
    pub total_supply: Balance,
    pub status: String,
    pub compliance_policy: u8,
    pub registered_at: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct AgentInfoResponse {
    pub agent_id: u64,
    pub owner: Address,
    pub name: String,
    pub url: String,
    pub domain_verified: bool,
    pub registered_at: u64,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ShieldedTreeStateResponse {
    pub merkle_root: Hash,
    pub leaf_count: u64,
    pub nullifier_count: usize,
}
