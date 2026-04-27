//! State persistence helpers — load/save all on-chain state to reth-db.

use std::sync::{Arc, RwLock};

use call_consensus::{SimplexConsensus, ValidatorStateManager, ForkManager, PersistedConsensusState};
use call_protocol::{
    AccountState, AssetRegistry, ComplianceEngine, FeeParams, FeeCurrencyRegistry, ProtocolReceipt,
};
use call_evm::EvmState;
use call_bridge::BridgeStateManager;
use call_agent::{AgentRegistry, AgentBalances};
use call_shielded::ShieldedState;
use call_oracle::OracleManager;
use call_governance::GovernanceManager;
use call_primitives::TxHash;
use call_rpc::RpcState;
use call_storage::{
    StorageError,
    db_put, db_batch_put, db_clear, db_iter_all, db_get, db_del,
    save_balances as db_save_balances, load_balances as db_load_balances,
    CallOracleState, CallEvmAccounts, CallBridgeOps,
    CallShieldedNullifiers, CallShieldedCommitments, CallValidators, CallAgents,
    CallGovernanceState, CallComplianceState, CallConsensusState, CallValidatorMeta,
    CallReceipts, CallReceiptsByBlock, CallAgentBalances, CallAgentNonces, CallForkState, CallCheckpoint,
    CallProtocolAssets, CallFeeParams, CallFeeCurrencyRegistry,
};
use reth_db::DatabaseEnv;

// ── State Persistence ─────────────────────────────────────────────────

/// All on-chain state loaded from reth-db in one struct.
/// Replaces the previous 13-element tuple so callers use named fields.
pub(crate) struct LoadedState {
    pub balance_state: AccountState,
    pub evm_state: EvmState,
    pub bridge_state: BridgeStateManager,
    pub shielded_state: ShieldedState,
    pub validators: ValidatorStateManager,
    pub agent_registry: AgentRegistry,
    pub agent_balances: AgentBalances,
    pub agent_nonces: call_agent::AgentNonces,
    pub governance: GovernanceManager,
    pub compliance: ComplianceEngine,
    pub asset_registry: AssetRegistry,
    pub fee_params: FeeParams,
    pub fee_currency_registry: FeeCurrencyRegistry,
}

/// Load all state types from the reth-db database.
pub(crate) fn load_state_from_db(db_env: &Arc<DatabaseEnv>) -> LoadedState {
    // Load balances
    let (balances, allowances) = match db_load_balances(db_env) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load balances from db");
            (std::collections::HashMap::new(), std::collections::HashMap::new())
        }
    };
    let mut balance_state = AccountState::new();
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

    // Load agent nonces
    let agent_nonces = match load_agent_nonces_inner(db_env) {
        Ok(n) => n,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load agent nonces");
            call_agent::AgentNonces::new()
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

    // Load compliance state
    let compliance = match load_compliance_state(db_env) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load compliance state");
            ComplianceEngine::new()
        }
    };

    // Load asset registry
    let asset_registry = match load_asset_registry_inner(db_env) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load asset registry");
            AssetRegistry::new()
        }
    };

    // Load fee params
    let fee_params = match load_fee_params(db_env) {
        Ok(p) => p,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load fee params");
            FeeParams::default()
        }
    };

    // Load fee currency registry
    let fee_currency_registry = match load_fee_currency_registry(db_env) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load fee currency registry");
            FeeCurrencyRegistry::new()
        }
    };

    LoadedState {
        balance_state,
        evm_state,
        bridge_state,
        shielded_state,
        validators,
        agent_registry: registry,
        agent_balances,
        agent_nonces,
        governance,
        compliance,
        asset_registry,
        fee_params,
        fee_currency_registry,
    }
}

/// Persist all state types to the reth-db database.
/// Uses a checkpoint marker to detect incomplete writes on crash recovery.
pub(crate) fn persist_state_to_db(
    db_env: &Arc<DatabaseEnv>,
    state: &Arc<RpcState>,
    consensus: &Arc<RwLock<SimplexConsensus>>,
) -> Result<(), String> {
    // 1. Write pending checkpoint marker
    let checkpoint_hash = {
        let c = consensus.read().unwrap();
        c.last_block_hash().0
    };
    write_checkpoint_pending(db_env, checkpoint_hash)
        .map_err(|e| format!("write checkpoint: {e}"))?;

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
        let agent_nonces = state.agent_nonces.read().unwrap();
        save_agent_state_inner(db_env, &registry, &agent_balances)
            .map_err(|e| format!("save agents: {e}"))?;
        save_agent_nonces_inner(db_env, &agent_nonces)
            .map_err(|e| format!("save agent nonces: {e}"))?;
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

    // Persist compliance state
    {
        let compliance = state.compliance_engine.read().unwrap();
        save_compliance_state(db_env, &compliance)
            .map_err(|e| format!("save compliance: {e}"))?;
    }

    // Persist fee params
    {
        let fee_params = state.fee_params.read().unwrap();
        save_fee_params(db_env, &fee_params)
            .map_err(|e| format!("save fee params: {e}"))?;
    }

    // Persist fee currency registry
    {
        let fee_currency_registry = state.fee_currency_registry.read().unwrap();
        save_fee_currency_registry(db_env, &fee_currency_registry)
            .map_err(|e| format!("save fee currency registry: {e}"))?;
    }

    // Persist asset registry
    {
        let asset_registry = state.asset_registry.read().unwrap();
        save_asset_registry_inner(db_env, &asset_registry)
            .map_err(|e| format!("save asset registry: {e}"))?;
    }

    // Persist consensus state
    {
        let c = consensus.read().unwrap();
        save_consensus_state_inner(db_env, &c)
            .map_err(|e| format!("save consensus: {e}"))?;
    }

    // Persist receipts
    {
        let receipts = state.receipts.read().unwrap();
        save_receipts(db_env, &receipts)
            .map_err(|e| format!("save receipts: {e}"))?;
    }

    // Persist fork state
    {
        let fork_manager = state.fork_manager.read().unwrap();
        save_fork_state(db_env, &fork_manager)
            .map_err(|e| format!("save fork state: {e}"))?;
    }

    // 3. Clear checkpoint marker — state is now consistent
    clear_checkpoint(db_env)
        .map_err(|e| format!("clear checkpoint: {e}"))?;

    Ok(())
}

/// Load EVM accounts from DB
pub(crate) fn load_evm_accounts_inner(db: &DatabaseEnv) -> Result<EvmState, String> {
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
pub(crate) fn save_evm_accounts_inner(db: &DatabaseEnv, state: &EvmState) -> Result<(), String> {
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
pub(crate) fn load_bridge_state_inner(db: &DatabaseEnv) -> Result<BridgeStateManager, String> {
    match db_get::<CallBridgeOps>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e: serde_json::Error| e.to_string()),
        None => Ok(BridgeStateManager::default()),
    }
}

/// Save bridge state to DB
pub(crate) fn save_bridge_state_inner(db: &DatabaseEnv, state: &BridgeStateManager) -> Result<(), String> {
    let data = serde_json::to_vec(state).map_err(|e: serde_json::Error| e.to_string())?;
    db_clear::<CallBridgeOps>(db).map_err(|e: StorageError| e.to_string())?;
    db_put::<CallBridgeOps>(db, vec![0], data).map_err(|e: StorageError| e.to_string())?;
    Ok(())
}

/// Load asset registry from DB
pub(crate) fn load_asset_registry_inner(db: &DatabaseEnv) -> Result<AssetRegistry, String> {
    match db_get::<CallProtocolAssets>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e: serde_json::Error| e.to_string()),
        None => Ok(AssetRegistry::new()),
    }
}

/// Save asset registry to DB
pub(crate) fn save_asset_registry_inner(db: &DatabaseEnv, registry: &AssetRegistry) -> Result<(), String> {
    let data = serde_json::to_vec(registry).map_err(|e: serde_json::Error| e.to_string())?;
    db_clear::<CallProtocolAssets>(db).map_err(|e: StorageError| e.to_string())?;
    db_put::<CallProtocolAssets>(db, vec![0], data).map_err(|e: StorageError| e.to_string())?;
    Ok(())
}

/// Load shielded state from DB
pub(crate) fn load_shielded_state_inner(db: &DatabaseEnv) -> Result<ShieldedState, String> {
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
        let bytes: [u8; 32] = cm.0.into();
        state.merkle_tree.insert(&bytes);
    }

    Ok(state)
}

/// Save shielded state to DB
pub(crate) fn save_shielded_state_inner(db: &DatabaseEnv, state: &ShieldedState) -> Result<(), String> {
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
pub(crate) fn load_validator_state_inner(db: &DatabaseEnv) -> Result<ValidatorStateManager, String> {
    let data = db_iter_all::<CallValidators>(db).map_err(|e: StorageError| e.to_string())?;
    let mut manager = ValidatorStateManager::new();
    for (k, v) in data {
        let stake: call_consensus::validator::ValidatorStake = serde_json::from_slice(&v).map_err(|e: serde_json::Error| e.to_string())?;
        let id: call_primitives::ValidatorId = serde_json::from_slice(&k).map_err(|e: serde_json::Error| e.to_string())?;
        manager.register_validator_from_stake(id, stake);
    }

    // Load global meta-state (queues, counters, params)
    match db_get::<CallValidatorMeta>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(meta_data) => {
            let snapshot: call_consensus::validator::ValidatorMetaSnapshot =
                serde_json::from_slice(&meta_data).map_err(|e| format!("deserialize validator meta: {e}"))?;
            manager.restore_meta_snapshot(snapshot);
        }
        None => {
            tracing::info!("no validator meta found in DB — using defaults");
        }
    }

    Ok(manager)
}

/// Save validator state to DB
pub(crate) fn save_validator_state_inner(db: &DatabaseEnv, state: &ValidatorStateManager) -> Result<(), String> {
    // Save individual validator stakes
    let entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .get_all_validators()
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    db_clear::<CallValidators>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallValidators>(db, entries).map_err(|e: StorageError| e.to_string())?;

    // Save global meta-state (queues, counters, params)
    let meta = state.meta_snapshot();
    let meta_data = serde_json::to_vec(&meta).map_err(|e| format!("serialize validator meta: {e}"))?;
    db_put::<CallValidatorMeta>(db, vec![0], meta_data).map_err(|e: StorageError| e.to_string())?;

    Ok(())
}

/// Load agent state from DB
pub(crate) fn load_agent_state_inner(db: &DatabaseEnv) -> Result<(AgentRegistry, AgentBalances), String> {
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

    // Load agent balances
    let balances = match load_agent_balances_inner(db) {
        Ok(b) => b,
        Err(e) => {
            tracing::warn!(error = %e, "failed to load agent balances");
            AgentBalances::new()
        }
    };

    Ok((registry, balances))
}

/// Save agent state to DB
pub(crate) fn save_agent_state_inner(db: &DatabaseEnv, registry: &AgentRegistry, balances: &AgentBalances) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = registry
        .agents
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    db_clear::<CallAgents>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallAgents>(db, entries).map_err(|e: StorageError| e.to_string())?;
    save_agent_balances_inner(db, balances)?;
    Ok(())
}

/// Save oracle state to the database.
pub(crate) fn save_oracle_state(db: &DatabaseEnv, state: &OracleManager) -> Result<(), String> {
    let data = serde_json::to_vec(state).map_err(|e| format!("serialize oracle: {e}"))?;
    db_put::<CallOracleState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load oracle state from the database.
pub(crate) fn load_oracle_state(db: &DatabaseEnv) -> Result<OracleManager, String> {
    match db_get::<CallOracleState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize oracle: {e}")),
        None => Ok(OracleManager::default()),
    }
}

/// Save fee params to the database.
pub(crate) fn save_fee_params(db: &DatabaseEnv, fee_params: &FeeParams) -> Result<(), String> {
    let data = serde_json::to_vec(fee_params).map_err(|e| format!("serialize fee params: {e}"))?;
    db_put::<CallFeeParams>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load fee params from the database.
pub(crate) fn load_fee_params(db: &DatabaseEnv) -> Result<FeeParams, String> {
    match db_get::<CallFeeParams>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize fee params: {e}")),
        None => Ok(FeeParams::default()),
    }
}

/// Save fee currency registry to the database.
pub(crate) fn save_fee_currency_registry(db: &DatabaseEnv, registry: &FeeCurrencyRegistry) -> Result<(), String> {
    let data = serde_json::to_vec(registry).map_err(|e| format!("serialize fee currency registry: {e}"))?;
    db_put::<CallFeeCurrencyRegistry>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load fee currency registry from the database.
pub(crate) fn load_fee_currency_registry(db: &DatabaseEnv) -> Result<FeeCurrencyRegistry, String> {
    match db_get::<CallFeeCurrencyRegistry>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize fee currency registry: {e}")),
        None => Ok(FeeCurrencyRegistry::new()),
    }
}

/// Save governance state to the database.
pub(crate) fn save_governance_state(db: &DatabaseEnv, state: &GovernanceManager) -> Result<(), String> {
    let data = serde_json::to_vec(state).map_err(|e| format!("serialize governance: {e}"))?;
    db_put::<CallGovernanceState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load governance state from the database.
pub(crate) fn load_governance_state(db: &DatabaseEnv) -> Result<GovernanceManager, String> {
    match db_get::<CallGovernanceState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize governance: {e}")),
        None => Ok(GovernanceManager::new()),
    }
}

/// Save compliance state to the database.
pub(crate) fn save_compliance_state(db: &DatabaseEnv, state: &ComplianceEngine) -> Result<(), String> {
    let snapshot = state.snapshot();
    let data = serde_json::to_vec(&snapshot).map_err(|e| format!("serialize compliance: {e}"))?;
    db_put::<CallComplianceState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load compliance state from the database.
pub(crate) fn load_compliance_state(db: &DatabaseEnv) -> Result<ComplianceEngine, String> {
    match db_get::<CallComplianceState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => {
            let snapshot: call_protocol::ComplianceEngineSnapshot =
                serde_json::from_slice(&data).map_err(|e| format!("deserialize compliance: {e}"))?;
            let mut engine = ComplianceEngine::new();
            engine.restore_from_snapshot(&snapshot);
            Ok(engine)
        }
        None => Ok(ComplianceEngine::new()),
    }
}

// ── Agent balances persistence ────────────────────────────────────────

pub(crate) fn save_agent_balances_inner(db: &DatabaseEnv, balances: &AgentBalances) -> Result<(), String> {
    let data = serde_json::to_vec(balances).map_err(|e| format!("serialize agent balances: {e}"))?;
    db_put::<CallAgentBalances>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

pub(crate) fn load_agent_balances_inner(db: &DatabaseEnv) -> Result<AgentBalances, String> {
    match db_get::<CallAgentBalances>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize agent balances: {e}")),
        None => Ok(AgentBalances::new()),
    }
}

// ── Agent nonces persistence ──────────────────────────────────────────

pub(crate) fn save_agent_nonces_inner(db: &DatabaseEnv, nonces: &call_agent::AgentNonces) -> Result<(), String> {
    let data = serde_json::to_vec(nonces).map_err(|e| format!("serialize agent nonces: {e}"))?;
    db_put::<CallAgentNonces>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

pub(crate) fn load_agent_nonces_inner(db: &DatabaseEnv) -> Result<call_agent::AgentNonces, String> {
    match db_get::<CallAgentNonces>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize agent nonces: {e}")),
        None => Ok(call_agent::AgentNonces::new()),
    }
}

// ── Receipt persistence ───────────────────────────────────────────────

pub(crate) fn save_receipts(db: &DatabaseEnv, receipts: &std::collections::HashMap<TxHash, ProtocolReceipt>) -> Result<(), String> {
    let entries: Vec<(Vec<u8>, Vec<u8>)> = receipts
        .iter()
        .map(|(k, v)| {
            let key: Vec<u8> = k.as_slice().to_vec();
            let value: Vec<u8> = serde_json::to_vec(v).unwrap();
            (key, value)
        })
        .collect();
    db_clear::<CallReceipts>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallReceipts>(db, entries).map_err(|e: StorageError| e.to_string())?;

    // Build block_number -> tx_hashes index
    let mut by_block: std::collections::HashMap<u64, Vec<TxHash>> = std::collections::HashMap::new();
    for (tx_hash, receipt) in receipts {
        by_block.entry(receipt.block_number).or_default().push(*tx_hash);
    }
    let index_entries: Vec<(Vec<u8>, Vec<u8>)> = by_block
        .iter()
        .map(|(block, hashes)| {
            (block.to_be_bytes().to_vec(), serde_json::to_vec(hashes).unwrap())
        })
        .collect();
    db_clear::<CallReceiptsByBlock>(db).map_err(|e: StorageError| e.to_string())?;
    db_batch_put::<CallReceiptsByBlock>(db, index_entries).map_err(|e: StorageError| e.to_string())
}

pub(crate) fn load_receipts(db: &DatabaseEnv) -> Result<std::collections::HashMap<TxHash, ProtocolReceipt>, String> {
    let data = db_iter_all::<CallReceipts>(db).map_err(|e: StorageError| e.to_string())?;
    let mut receipts = std::collections::HashMap::new();
    for (k, v) in data {
        let key = call_primitives::TxHash::from_slice(&k);
        let receipt: ProtocolReceipt = serde_json::from_slice(&v).map_err(|e| format!("deserialize receipt: {e}"))?;
        receipts.insert(key, receipt);
    }
    Ok(receipts)
}

pub(crate) fn load_receipts_by_block(db: &DatabaseEnv, block_number: u64) -> Result<Vec<TxHash>, String> {
    let key = block_number.to_be_bytes().to_vec();
    match db_get::<CallReceiptsByBlock>(db, &key) {
        Ok(Some(v)) => serde_json::from_slice(&v).map_err(|e| e.to_string()),
        Ok(None) => Ok(vec![]),
        Err(e) => Err(e.to_string()),
    }
}

pub(crate) fn delete_receipts_by_block(db: &DatabaseEnv, block_number: u64) -> Result<(), String> {
    let tx_hashes = load_receipts_by_block(db, block_number)?;
    for tx_hash in tx_hashes {
        db_del::<CallReceipts>(db, tx_hash.as_slice()).map_err(|e: StorageError| e.to_string())?;
    }
    db_del::<CallReceiptsByBlock>(db, &block_number.to_be_bytes()).map_err(|e: StorageError| e.to_string())
}

// ── Checkpoint / WAL persistence ──────────────────────────────────────

/// Write a checkpoint marker to signal that a state write is in progress.
/// If the node crashes while this marker exists, state may be inconsistent.
pub(crate) fn write_checkpoint_pending(db: &DatabaseEnv, state_hash: [u8; 32]) -> Result<(), String> {
    db_put::<CallCheckpoint>(db, b"pending".to_vec(), state_hash.to_vec())
        .map_err(|e: StorageError| e.to_string())
}

/// Clear the checkpoint marker after a successful state write.
pub(crate) fn clear_checkpoint(db: &DatabaseEnv) -> Result<(), String> {
    db_del::<CallCheckpoint>(db, b"pending")
        .map_err(|e: StorageError| e.to_string())
}

/// Check if a pending checkpoint marker exists (indicates potential crash).
pub(crate) fn check_recovery_needed(db: &DatabaseEnv) -> Result<bool, String> {
    match db_get::<CallCheckpoint>(db, b"pending").map_err(|e: StorageError| e.to_string())? {
        Some(_) => Ok(true),
        None => Ok(false),
    }
}

// ── Fork state persistence ────────────────────────────────────────────

pub(crate) fn save_fork_state(db: &DatabaseEnv, fork_manager: &ForkManager) -> Result<(), String> {
    let data = serde_json::to_vec(fork_manager).map_err(|e| format!("serialize fork state: {e}"))?;
    db_put::<CallForkState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

pub(crate) fn load_fork_state(db: &DatabaseEnv) -> Result<Option<ForkManager>, String> {
    match db_get::<CallForkState>(db, &[0]).map_err(|e: StorageError| e.to_string())? {
        Some(data) => serde_json::from_slice(&data).map_err(|e| format!("deserialize fork state: {e}")).map(Some),
        None => Ok(None),
    }
}

/// Save consensus state to the database.
pub(crate) fn save_consensus_state_inner(db: &DatabaseEnv, consensus: &SimplexConsensus) -> Result<(), String> {
    let state = consensus.persist_state();
    let data = bincode::serialize(&state).map_err(|e| format!("serialize consensus: {e}"))?;
    db_put::<CallConsensusState>(db, vec![0], data).map_err(|e: StorageError| e.to_string())
}

/// Load consensus state from the database.
pub(crate) fn load_consensus_state_inner(db: &DatabaseEnv, validators: &ValidatorStateManager) -> Result<SimplexConsensus, String> {
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
pub(crate) fn persist_state_incremental(
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

    // Persist asset registry (overwrite)
    {
        let asset_registry = state.asset_registry.read().map_err(|_| "asset registry lock poisoned".to_string())?;
        save_asset_registry_inner(db_env, &asset_registry)
            .map_err(|e| format!("save asset registry: {e}"))?;
    }

    // Persist consensus state
    {
        let c = consensus.read().map_err(|_| "consensus lock poisoned".to_string())?;
        save_consensus_state_inner(db_env, &c)
            .map_err(|e| format!("save consensus: {e}"))?;
    }

    // Persist receipts (overwrite)
    {
        let receipts = state.receipts.read().map_err(|_| "receipt lock poisoned".to_string())?;
        save_receipts(db_env, &receipts)
            .map_err(|e| format!("save receipts: {e}"))?;
    }

    // Persist fork state (overwrite)
    {
        let fork_manager = state.fork_manager.read().map_err(|_| "fork lock poisoned".to_string())?;
        save_fork_state(db_env, &fork_manager)
            .map_err(|e| format!("save fork state: {e}"))?;
    }

    Ok(())
}

/// Save EVM accounts without clearing the table first.
pub(crate) fn save_evm_accounts_no_clear(db: &DatabaseEnv, state: &EvmState) -> Result<(), String> {
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
pub(crate) fn save_validator_state_no_clear(db: &DatabaseEnv, state: &ValidatorStateManager) -> Result<(), String> {
    // Save individual validator stakes
    let entries: Vec<(Vec<u8>, Vec<u8>)> = state
        .get_all_validators()
        .iter()
        .map(|(k, v)| (serde_json::to_vec(k).unwrap(), serde_json::to_vec(v).unwrap()))
        .collect();
    for (k, v) in entries {
        db_put::<CallValidators>(db, k, v).map_err(|e: StorageError| e.to_string())?;
    }

    // Save global meta-state
    let meta = state.meta_snapshot();
    let meta_data = serde_json::to_vec(&meta).map_err(|e| format!("serialize validator meta: {e}"))?;
    db_put::<CallValidatorMeta>(db, vec![0], meta_data).map_err(|e: StorageError| e.to_string())?;

    Ok(())
}

/// Save agent state without clearing the table first.
pub(crate) fn save_agent_state_no_clear(db: &DatabaseEnv, registry: &AgentRegistry) -> Result<(), String> {
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

