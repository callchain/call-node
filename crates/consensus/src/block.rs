//! T6.1 — Block Structure (per spec §2.4, §2.5)
//!
//! Block and BlockHeader types with hash, validation, and execution.

use call_bridge::{BridgeOp, BridgeConfig};
use call_crypto::{build_merkle_root, keccak256};
use call_governance::GovernanceManager;
use call_primitives::{Address, Balance, BlockHash, Hash, ProtocolVersion};
use call_protocol::account::AccountState;
use call_protocol::instructions::{execute_protocol_instructions, Instruction, InstructionResult};
use call_protocol::registry::AssetRegistry;
use call_protocol::transaction::ProtocolTransaction;
use call_protocol::FeeParams;
use call_shielded::ShieldedState;
use call_evm::{EvmExecutor, EvmState, EvmTransaction, BlockGasTracker};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::validator::{ConsensusError, STAKING_ESCROW, ValidatorStateManager};
use crate::{ForkManager, RollbackPlan};

// ── Signature Wrapper (for serde) ─────────────────────────────────────

/// Wrapper around [u8; 65] signature with serde support
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BlockSignature(pub [u8; 65]);

impl Default for BlockSignature {
    fn default() -> Self {
        Self([0u8; 65])
    }
}

impl Serialize for BlockSignature {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(&self.0)
    }
}

impl<'de> Deserialize<'de> for BlockSignature {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = <Vec<u8> as serde::Deserialize>::deserialize(deserializer)?;
        let bytes: [u8; 65] = bytes
            .try_into()
            .map_err(|_| serde::de::Error::custom("expected 65 bytes"))?;
        Ok(BlockSignature(bytes))
    }
}

impl From<[u8; 65]> for BlockSignature {
    fn from(s: [u8; 65]) -> Self {
        BlockSignature(s)
    }
}

impl From<BlockSignature> for [u8; 65] {
    fn from(s: BlockSignature) -> Self {
        s.0
    }
}

// ── Block Header ──────────────────────────────────────────────────────

/// Block header with state roots (per spec §2.4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockHeader {
    pub parent_hash: BlockHash,
    pub height: u64,
    pub timestamp_millis: u64,
    pub payment_root: Hash,
    pub evm_state_root: Hash,
    pub bridge_root: Hash,
    pub receipt_root: Hash,
    pub proposer: call_primitives::ValidatorId,
    pub signature: BlockSignature,
    /// Protocol version of this block (per spec §19)
    pub version: ProtocolVersion,
    /// Optional BLS12-381 aggregated signature for light client verification
    pub bls_aggregate_signature: Option<Vec<u8>>,
    /// Bitmask of which validators contributed to the BLS aggregate (bit i = validator i)
    pub bls_signer_bitmap: Vec<u8>,
}

impl BlockHeader {
    /// Compute the block header hash (keccak256 of all fields)
    pub fn hash(&self) -> BlockHash {
        let mut data = Vec::with_capacity(256);
        data.extend_from_slice(self.parent_hash.as_slice());
        data.extend_from_slice(&self.height.to_le_bytes());
        data.extend_from_slice(&self.timestamp_millis.to_le_bytes());
        data.extend_from_slice(self.payment_root.as_slice());
        data.extend_from_slice(self.evm_state_root.as_slice());
        data.extend_from_slice(self.bridge_root.as_slice());
        data.extend_from_slice(self.receipt_root.as_slice());
        data.extend_from_slice(&self.proposer.to_le_bytes());
        data.extend_from_slice(&self.signature.0);
        data.extend_from_slice(&self.version.major.to_le_bytes());
        data.extend_from_slice(&self.version.minor.to_le_bytes());
        data.extend_from_slice(&self.version.patch.to_le_bytes());
        // Note: bls_aggregate_signature is intentionally excluded from the hash
        // because it is a consensus seal added after the block content is finalized.
        keccak256(&data)
    }

    /// Validate header fields including protocol version.
    /// `fork_manager` is used to verify the block's version matches the expected version at its height.
    pub fn validate(
        &self,
        expected_parent: BlockHash,
        fork_manager: &ForkManager,
    ) -> Result<(), ConsensusError> {
        if self.parent_hash != expected_parent {
            return Err(ConsensusError::InvalidBlock(format!(
                "parent hash mismatch: expected {expected_parent}, got {}",
                self.parent_hash
            )));
        }
        if self.height == 0 && self.parent_hash != BlockHash::ZERO {
            return Err(ConsensusError::InvalidBlock(
                "genesis block must have zero parent hash".into(),
            ));
        }
        if self.timestamp_millis == 0 {
            return Err(ConsensusError::InvalidBlock("zero timestamp".into()));
        }
        if self.proposer == 0 {
            return Err(ConsensusError::InvalidBlock("zero proposer".into()));
        }
        // Validate block version matches expected version at this height
        if let Err(e) = fork_manager.validate_block_version(self.height, self.version) {
            return Err(ConsensusError::InvalidBlock(format!(
                "block version mismatch: {e}"
            )));
        }
        Ok(())
    }
}

// ── System Transaction ────────────────────────────────────────────────

/// System transaction for validator reward distribution and fee settlement
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemTx {
    pub kind: SystemTxKind,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemTxKind {
    /// Distribute fees to validator
    ValidatorReward {
        proposer: call_primitives::ValidatorId,
        reward: Balance,
    },
    /// Update base fee for next block
    UpdateBaseFee,
    /// Protocol version upgrade
    ProtocolUpgrade(ProtocolVersion),
}

// ── EVM Transaction ───────────────────────────────────────────────────

/// EVM transaction in raw serialized form (RLP-encoded).
/// Deserialized into `EvmTransaction` during execution.
pub type EvmTx = Vec<u8>;

// ── Block ─────────────────────────────────────────────────────────────

/// Full block structure (per spec §2.4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub protocol_txs: Vec<ProtocolTransaction>,
    pub evm_txs: Vec<EvmTx>,
    pub system_txs: Vec<SystemTx>,
    pub bridge_operations: Vec<BridgeOp>,
}

impl Block {
    /// Build a new block
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        height: u64,
        parent_hash: BlockHash,
        timestamp_millis: u64,
        proposer: call_primitives::ValidatorId,
        version: ProtocolVersion,
        protocol_txs: Vec<ProtocolTransaction>,
        evm_txs: Vec<EvmTx>,
        system_txs: Vec<SystemTx>,
        bridge_operations: Vec<BridgeOp>,
    ) -> Self {
        let header = BlockHeader {
            parent_hash,
            height,
            timestamp_millis,
            payment_root: Hash::ZERO,
            evm_state_root: Hash::ZERO,
            bridge_root: Hash::ZERO,
            receipt_root: Hash::ZERO,
            proposer,
            signature: BlockSignature::default(),
            version,
            bls_aggregate_signature: None,
            bls_signer_bitmap: Vec::new(),
        };

        Self {
            header,
            protocol_txs,
            evm_txs,
            system_txs,
            bridge_operations,
        }
    }

    /// Validate block structure and header
    pub fn validate(
        &self,
        expected_parent: BlockHash,
        fork_manager: &ForkManager,
    ) -> Result<(), ConsensusError> {
        self.header.validate(expected_parent, fork_manager)?;

        // Per spec §2.5: execution order must be EVM → Protocol → Bridge → System
        // We validate that each section is internally consistent

        // Check no duplicate nonces in protocol txs
        let mut seen_nonces = HashSet::new();
        for tx in &self.protocol_txs {
            let key = (tx.sender, tx.nonce);
            if !seen_nonces.insert(key) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "duplicate nonce: {:?}",
                    key
                )));
            }
        }

        Ok(())
    }

    /// Execute all transactions in spec order (per spec §2.5):
    /// 1. EVM transactions (evm_txs)
    /// 2. Protocol transactions (protocol_txs)
    /// 3. Bridge operations (bridge_operations)
    /// 4. System transactions (system_txs)
    ///
    /// Returns all instruction results.
    #[allow(clippy::too_many_arguments)]
    pub fn execute(
        &self,
        account: &mut AccountState,
        registry: &mut AssetRegistry,
        compliance: &mut call_protocol::compliance::ComplianceEngine,
        bridge_state: &mut call_bridge::BridgeStateManager,
        shielded_state: &mut ShieldedState,
        fee_params: &mut FeeParams,
        current_block_height: u64,
        evm_state: &mut EvmState,
        mut oracle: Option<&mut call_oracle::OracleManager>,
        mut agent_executor: Option<call_protocol::instructions::AgentExecutor>,
        mut agent_balances: Option<&mut call_agent::AgentBalances>,
        mut agent_registry: Option<&mut call_agent::AgentRegistry>,
        mut governance: Option<&mut GovernanceManager>,
        bridge_config: Option<&BridgeConfig>,
        validators: Option<&[Address]>,
        smart_accounts: Option<&call_protocol::smart_accounts::SmartAccountRegistry>,
        mut validator_state: Option<&mut ValidatorStateManager>,
        mut fork_manager: Option<&mut ForkManager>,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let mut result = BlockExecutionResult::default();
        let executor = EvmExecutor::new(1); // chain_id = 1
        let max_evm_gas = 30_000_000u64; // default block gas limit (~30M for Ethereum-compatible)
        let mut gas_tracker = BlockGasTracker::new(max_evm_gas);
        // Track nonces separately: EVM and protocol operate on separate namespaces
        let mut used_evm_nonces: HashSet<(call_primitives::Address, u64)> = HashSet::new();

        // Step 1: EVM transactions
        for raw_tx in &self.evm_txs {
            if let Ok(tx) = decode_evm_tx(raw_tx) {
                let caller = tx.caller;
                let nonce = tx.nonce;

                // Check for duplicate nonce within this block
                if !used_evm_nonces.insert((caller, nonce)) {
                    continue; // duplicate nonce in same block
                }

                // Validate nonce and balance before execution
                if call_evm::validate_evm_tx(&tx, evm_state).is_err() {
                    continue;
                }

                // Unified gas balance: auto-bridge Protocol→EVM if needed
                let gas_cost_u256 = call_evm::U256::from(tx.gas_limit)
                    .saturating_mul(call_evm::U256::from(tx.gas_price));
                let gas_cost_u128: u128 = gas_cost_u256.try_into().unwrap_or(u128::MAX);
                let evm_balance_u128: u128 =
                    evm_state.get_balance(&caller).try_into().unwrap_or(0);

                if evm_balance_u128 < gas_cost_u128 {
                    let needed = gas_cost_u128 - evm_balance_u128;
                    let protocol_balance =
                        account.get_balance(call_protocol::CALL_ASSET_ID, &caller);
                    if protocol_balance < needed {
                        continue; // insufficient unified gas
                    }
                    if account
                        .deduct_balance(call_protocol::CALL_ASSET_ID, caller, needed)
                        .is_ok()
                    {
                        let new_evm = evm_state.get_balance(&caller)
                            + call_evm::U256::from(needed);
                        evm_state.set_balance(caller, new_evm);
                    } else {
                        continue;
                    }
                }

                if let Ok(exec_result) = executor.execute_tx(tx, evm_state) {
                    if gas_tracker.add_gas(exec_result.gas_used).is_err() {
                        continue;
                    }
                    result.evm_tx_count += 1;
                    result.evm_gas_used += exec_result.gas_used;
                }
                // Nonce consumed regardless of execution result (same as Ethereum)
            }
        }

        // Step 2: Protocol transactions
        for tx in &self.protocol_txs {
            // Validate nonce against state (nonce consumed on inclusion)
            if let Err(_e) = account.validate_nonce(&tx.sender, tx.nonce) {
                // Nonce mismatch or already used — skip this tx
                continue;
            }

            // Verify transaction signature before execution
            if let Err(e) = tx.verify_signature_with_registry(smart_accounts) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "signature verification failed for tx from {:?}: {e}",
                    tx.sender
                )));
            }

            // Check transaction expiry
            if tx.expires_at != 0 && current_block_height > tx.expires_at {
                return Err(ConsensusError::InvalidBlock(format!(
                    "transaction from {:?} expired at block {} (current: {})",
                    tx.sender, tx.expires_at, current_block_height
                )));
            }

            // Compute gas fee
            let gas_units = call_protocol::transaction::calculate_gas_units(&tx.instructions);
            let fee = call_protocol::transaction::compute_fee(
                gas_units,
                call_protocol::transaction::MIN_PRIORITY_FEE_PER_GAS,
                fee_params.base_fee,
            )
            .min(tx.max_fee);

            // Unified gas balance: auto-bridge EVM→Protocol if needed
            let mut bridged_from_evm = 0u128;
            let protocol_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &tx.sender);
            if protocol_balance < fee {
                let needed = fee - protocol_balance;
                let evm_balance_u128: u128 =
                    evm_state.get_balance(&tx.sender).try_into().unwrap_or(0);
                if evm_balance_u128 < needed {
                    continue; // insufficient unified gas
                }
                let current_evm = evm_state.get_balance(&tx.sender);
                evm_state.set_balance(
                    tx.sender,
                    current_evm - call_evm::U256::from(needed),
                );
                if account
                    .credit_balance(call_protocol::CALL_ASSET_ID, tx.sender, needed)
                    .is_err()
                {
                    evm_state.set_balance(tx.sender, current_evm);
                    continue;
                }
                bridged_from_evm = needed;
            }

            // Deduct gas fee from protocol balance
            let gas_ok = match tx.fee_currency {
                call_primitives::FeeCurrency::Call => account
                    .deduct_balance(call_protocol::CALL_ASSET_ID, tx.sender, fee)
                    .is_ok(),
                call_primitives::FeeCurrency::Stablecoin(asset_id) => account
                    .deduct_balance(asset_id, tx.sender, fee)
                    .is_ok(),
            };
            if !gas_ok {
                // Rollback EVM bridge if we bridged
                if bridged_from_evm > 0 {
                    let current_evm = evm_state.get_balance(&tx.sender);
                    evm_state.set_balance(
                        tx.sender,
                        current_evm + call_evm::U256::from(bridged_from_evm),
                    );
                    let _ = account.deduct_balance(
                        call_protocol::CALL_ASSET_ID,
                        tx.sender,
                        bridged_from_evm,
                    );
                }
                continue;
            }

            // Increment nonce (consumed on inclusion, regardless of execution result)
            account.increment_nonce(tx.sender);

            // Separate instructions by type: bridge, agent, validator, asset, regular
            let (bridge_instrs, non_bridge): (Vec<_>, Vec<_>) = tx
                .instructions
                .iter()
                .cloned()
                .partition(|i| is_bridge_instruction(i));
            let (agent_instrs, non_agent): (Vec<_>, Vec<_>) = non_bridge
                .iter()
                .cloned()
                .partition(|i| is_agent_instruction(i));
            let (validator_instrs, non_validator): (Vec<_>, Vec<_>) = non_agent
                .iter()
                .cloned()
                .partition(|i| is_validator_instruction(i));
            let (asset_instrs, non_asset): (Vec<_>, Vec<_>) = non_validator
                .iter()
                .cloned()
                .partition(|i| is_asset_instruction(i));
            let (rollback_instrs, other_instrs): (Vec<_>, Vec<_>) = non_asset
                .iter()
                .cloned()
                .partition(|i| is_rollback_instruction(i));

            // Take snapshots for atomic rollback (nonce already consumed, not rolled back)
            let balance_snapshot = account.clone();
            let evm_snapshot = evm_state.clone();
            let bridge_snapshot = bridge_state.clone();
            let validator_snapshot = validator_state.as_ref().map(|vs| (*vs).clone());
            let mut tx_results = Vec::new();
            let mut tx_agent_events = Vec::new();

            let exec_result = (|| -> Result<(), ConsensusError> {
                // Execute regular instructions via protocol engine (includes governance)
                if !other_instrs.is_empty() {
                    let results = execute_protocol_instructions(
                        &other_instrs,
                        account,
                        registry,
                        compliance,
                        shielded_state,
                        tx.sender,
                        oracle.as_deref_mut(),
                        &mut agent_executor,
                        governance.as_deref_mut(),
                    )
                    .map_err(|e| {
                        ConsensusError::InvalidBlock(format!("protocol tx: {e}"))
                    })?;
                    tx_results.extend(results);
                }

                // Execute asset registration instructions inline
                if !asset_instrs.is_empty() {
                    for instr in &asset_instrs {
                        let r = execute_asset_instruction(
                            instr,
                            tx.sender,
                            account,
                            registry,
                            governance.as_deref_mut(),
                            evm_state,
                            &executor,
                            current_block_height,
                        )?;
                        tx_results.push(r);
                    }
                }

                // Execute bridge deposit instructions inline
                if !bridge_instrs.is_empty() {
                    let config = bridge_config.ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "bridge instructions require bridge config".into(),
                        )
                    })?;
                    let vals = validators.ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "bridge instructions require validator set".into(),
                        )
                    })?;
                    for instr in &bridge_instrs {
                        let r = execute_bridge_instruction(
                            instr,
                            tx.sender,
                            account,
                            bridge_state,
                            config,
                            vals,
                            current_block_height,
                            evm_state,
                            &executor,
                            registry,
                        )?;
                        tx_results.push(r);
                    }
                }

                // Execute agent instructions inline
                if !agent_instrs.is_empty() {
                    let ab = agent_balances
                        .as_mut()
                        .ok_or_else(|| {
                            ConsensusError::InvalidBlock(
                                "agent instructions require agent state".into(),
                            )
                        })?;
                    let ar = agent_registry.as_mut().ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "agent instructions require agent registry".into(),
                        )
                    })?;
                    for instr in &agent_instrs {
                        let r = execute_agent_instruction(
                            instr,
                            tx.sender,
                            ab,
                            ar,
                            account,
                            evm_state,
                            bridge_state,
                            registry,
                            &executor,
                            current_block_height,
                            fee_params,
                            &mut tx_agent_events,
                        )?;
                        tx_results.push(r);
                    }
                }

                // Execute validator instructions inline
                if !validator_instrs.is_empty() {
                    let vs = validator_state.as_mut().ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "validator instructions require validator state".into(),
                        )
                    })?;
                    for instr in &validator_instrs {
                        let r = execute_validator_instruction(
                            instr,
                            tx.sender,
                            account,
                            vs,
                            current_block_height,
                        )?;
                        tx_results.push(r);
                    }
                }

                // Execute rollback instructions inline
                if !rollback_instrs.is_empty() {
                    let fm = fork_manager.as_mut().ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "rollback instructions require fork manager".into(),
                        )
                    })?;
                    for instr in &rollback_instrs {
                        let maybe_plan = execute_rollback_instruction(
                            instr,
                            fm,
                            current_block_height,
                        )?;
                        if let Some(plan) = maybe_plan {
                            result.pending_rollback = Some(plan);
                        }
                        tx_results.push(InstructionResult::Success);
                    }
                }

                Ok(())
            })();

            if let Err(e) = exec_result {
                // Rollback balance/EVM/bridge but nonce stays consumed
                *account = balance_snapshot;
                *evm_state = evm_snapshot;
                *bridge_state = bridge_snapshot;
                if let Some(ref snapshot) = validator_snapshot {
                    if let Some(ref mut vs) = validator_state {
                        **vs = snapshot.clone();
                    }
                }
                return Err(e);
            }

            result.protocol_tx_count += 1;
            result.instruction_results.extend(tx_results);
            result.agent_events.extend(tx_agent_events);
        }

        // Step 3: Bridge operations
        // Execute internal bridge deposits/withdrawals atomically.
        // Each operation deducts/credits protocol account and mints/burns
        // wrapped ERC-20 tokens in the EVM layer.
        if let Some(config) = bridge_config {
            for op in &self.bridge_operations {
                let asset_id = op.asset_id();
                let Some(contract_addr) = registry.get_evm_contract_address(asset_id) else {
                    // Asset has no deployed wrapped token — skip
                    continue;
                };

                let exec_result = match op {
                    BridgeOp::DepositToEvm { from, .. } => {
                        call_bridge::execute_deposit(
                            op,
                            account,
                            evm_state,
                            &executor,
                            bridge_state,
                            config,
                            registry,
                            contract_addr,
                            *from,
                            current_block_height,
                        )
                    }
                    BridgeOp::WithdrawToProtocol { from, .. } => {
                        call_bridge::execute_withdraw(
                            op,
                            account,
                            evm_state,
                            &executor,
                            bridge_state,
                            config,
                            registry,
                            contract_addr,
                            *from,
                            current_block_height,
                        )
                    }
                };

                match exec_result {
                    Ok(_) => {
                        bridge_state.add_pending_op(op.clone(), current_block_height);
                        result.bridge_op_count += 1;
                    }
                    Err(_) => {
                        // Skip failed bridge ops (same pattern as EVM txs)
                    }
                }
            }
        }

        // Step 4: System transactions
        for sys_tx in &self.system_txs {
            match &sys_tx.kind {
                SystemTxKind::ValidatorReward { proposer: _, reward } => {
                    result.total_validator_reward += *reward;
                }
                SystemTxKind::UpdateBaseFee => {
                    let gas_used = result.protocol_tx_count as u64 * 10_000; // rough estimate
                    update_base_fee_after_block(fee_params, gas_used);
                }
                SystemTxKind::ProtocolUpgrade(_) => {
                    // Version upgrade handled by node layer
                }
            }
            result.system_tx_count += 1;
        }

        // Step 4.5: Auto-finalize pending external deposits and bridge maintenance
        if let Some(config) = bridge_config {
            bridge_state.on_block_finalized(
                current_block_height,
                config.challenge_period_blocks,
                config.processed_tx_retention_blocks,
            );
        }

        // Step 5: Oracle reward pool allocation from block fees
        if let Some(oracle) = oracle.as_mut() {
            // Approximate total gas used: EVM gas + protocol txs * base gas per tx
            let total_gas = result.evm_gas_used + result.protocol_tx_count as u64 * 21_000;
            let total_fees = total_gas as u128 * fee_params.base_fee;
            let oracle_share = total_fees * fee_params.oracle_fee_share_bps as u128 / 10_000;
            oracle.add_reward(oracle_share);
            // Note: clear_tracking is NOT called here — the caller must call it
            // after processing outliers for slashing, otherwise outlier data is lost.
        }

        // Compute state roots
        result.payment_root = compute_payment_root(account);
        result.evm_state_root = compute_evm_state_root(evm_state);
        result.bridge_root = compute_bridge_root(bridge_state);
        result.receipt_root = compute_receipt_root(&result);

        Ok(result)
    }

    /// Update header roots after execution
    pub fn finalize(&mut self, result: &BlockExecutionResult) {
        self.header.payment_root = result.payment_root;
        self.header.evm_state_root = result.evm_state_root;
        self.header.bridge_root = result.bridge_root;
        self.header.receipt_root = result.receipt_root;
    }
}

// ── Agent instruction helpers ─────────────────────────────────────────

fn is_agent_instruction(instr: &Instruction) -> bool {
    matches!(
        instr,
        Instruction::AgentPay { .. }
            | Instruction::AgentBatchPay { .. }
            | Instruction::AgentCall { .. }
            | Instruction::AgentBridgeDeposit { .. }
            | Instruction::RegisterAgent { .. }
            | Instruction::GrantAgentBalance { .. }
            | Instruction::RevokeAgentBalance { .. }
    )
}

/// Verify agent permissions for a single instruction.
fn verify_agent_instruction_permissions(
    agent: &call_agent::AgentRegistration,
    asset_id: call_primitives::AssetId,
    amount: u128,
    current_block: u64,
) -> Result<(), ConsensusError> {
    let perms = &agent.permissions;
    if !perms.is_asset_allowed(asset_id) {
        return Err(ConsensusError::InvalidBlock(format!(
            "agent {}: asset {} not allowed",
            agent.agent_id, asset_id
        )));
    }
    if amount > perms.per_tx_limit {
        return Err(ConsensusError::InvalidBlock(format!(
            "agent {}: amount {} exceeds per-tx limit {}",
            agent.agent_id, amount, perms.per_tx_limit
        )));
    }
    if perms.is_expired(current_block) {
        return Err(ConsensusError::InvalidBlock(format!(
            "agent {}: permissions expired",
            agent.agent_id
        )));
    }
    Ok(())
}

fn execute_agent_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    agent_balances: &mut call_agent::AgentBalances,
    agent_registry: &mut call_agent::AgentRegistry,
    account: &mut AccountState,
    evm_state: &mut EvmState,
    bridge_state: &mut call_bridge::BridgeStateManager,
    registry: &AssetRegistry,
    evm_executor: &EvmExecutor,
    current_block_height: u64,
    fee_params: &FeeParams,
    agent_events: &mut Vec<call_agent::AgentEvent>,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::AgentPay { payment } => {
            let agent = agent_registry
                .get_agent(payment.agent_id)
                .ok_or_else(|| ConsensusError::InvalidBlock("agent not found".into()))?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "agent pay: sender is not owner".into(),
                ));
            }
            verify_agent_instruction_permissions(
                agent,
                payment.asset_id,
                payment.amount,
                current_block_height,
            )?;
            agent_balances
                .deduct(agent.owner, payment.agent_id, payment.asset_id, payment.amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("agent pay: {e}")))?;
            account
                .credit_balance(payment.asset_id, payment.to, payment.amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("agent pay: {e}")))?;
            agent_events.push(call_agent::AgentEvent {
                event_type: call_agent::AgentEventType::AgentPay,
                agent_id: payment.agent_id,
                tx_hash: None,
                asset_id: payment.asset_id,
                amount: payment.amount,
                recipient: Some(payment.to),
                block_height: current_block_height,
            });
            Ok(InstructionResult::Success)
        }
        Instruction::AgentBatchPay { payments } => {
            for payment in payments {
                let agent = agent_registry
                    .get_agent(payment.agent_id)
                    .ok_or_else(|| ConsensusError::InvalidBlock("agent not found".into()))?;
                if agent.owner != sender {
                    return Err(ConsensusError::InvalidBlock(
                        "agent batch pay: sender is not owner".into(),
                    ));
                }
                verify_agent_instruction_permissions(
                    agent,
                    payment.asset_id,
                    payment.amount,
                    current_block_height,
                )?;
                agent_balances
                    .deduct(agent.owner, payment.agent_id, payment.asset_id, payment.amount)
                    .map_err(|e| {
                        ConsensusError::InvalidBlock(format!("agent batch pay: {e}"))
                    })?;
                account
                    .credit_balance(payment.asset_id, payment.to, payment.amount)
                    .map_err(|e| {
                        ConsensusError::InvalidBlock(format!("agent batch pay: {e}"))
                    })?;
                agent_events.push(call_agent::AgentEvent {
                    event_type: call_agent::AgentEventType::AgentBatchPay,
                    agent_id: payment.agent_id,
                    tx_hash: None,
                    asset_id: payment.asset_id,
                    amount: payment.amount,
                    recipient: Some(payment.to),
                    block_height: current_block_height,
                });
            }
            Ok(InstructionResult::Success)
        }
        Instruction::AgentCall { agent_id, target, data } => {
            let agent = agent_registry
                .get_agent(*agent_id)
                .ok_or_else(|| ConsensusError::InvalidBlock("agent not found".into()))?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "agent call: sender is not owner".into(),
                ));
            }
            if !agent.permissions.is_protocol_allowed(target) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "agent {}: target {:?} not in allowed protocols",
                    agent.agent_id, target
                )));
            }
            if agent.permissions.is_expired(current_block_height) {
                return Err(ConsensusError::InvalidBlock(
                    "agent call: permissions expired".into(),
                ));
            }
            let tx = EvmTransaction {
                caller: sender,
                nonce: 0,
                gas_limit: 1_000_000,
                gas_price: 0,
                to: Some(*target),
                value: call_primitives::U256::ZERO,
                data: call_primitives::Bytes::from(data.clone()),
                chain_id: evm_executor.chain_id,
            };
            match evm_executor.execute_tx(tx, evm_state) {
                Ok(_) => {
                    agent_events.push(call_agent::AgentEvent {
                        event_type: call_agent::AgentEventType::AgentCall,
                        agent_id: *agent_id,
                        tx_hash: None,
                        asset_id: 0,
                        amount: 0,
                        recipient: Some(*target),
                        block_height: current_block_height,
                    });
                    Ok(InstructionResult::Success)
                }
                Err(e) => Err(ConsensusError::InvalidBlock(format!("agent call: {e:?}"))),
            }
        }
        Instruction::AgentBridgeDeposit {
            agent_id,
            asset_id,
            amount,
            target_address,
            ..
        } => {
            let agent = agent_registry
                .get_agent(*agent_id)
                .ok_or_else(|| ConsensusError::InvalidBlock("agent not found".into()))?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "agent bridge deposit: sender is not owner".into(),
                ));
            }
            verify_agent_instruction_permissions(
                agent,
                *asset_id,
                *amount,
                current_block_height,
            )?;
            agent_balances
                .deduct(agent.owner, *agent_id, *asset_id, *amount)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!("agent bridge deposit: {e}"))
                })?;
            account
                .deduct_balance(*asset_id, sender, *amount)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!("agent bridge deposit: {e}"))
                })?;

            let config = call_bridge::BridgeConfig::default();
            let op = call_bridge::BridgeOp::DepositToEvm {
                asset_id: *asset_id,
                from: sender,
                to: call_primitives::Address::from_slice(
                    &target_address[..target_address.len().min(20)],
                ),
                amount: *amount,
            };
            let bridge_address = registry
                .get_evm_contract_address(*asset_id)
                .unwrap_or_else(|| call_primitives::Address::from_slice(&[0xCCu8; 20]));

            match call_bridge::execute_deposit(
                &op, account, evm_state, evm_executor, bridge_state, &config, registry,
                bridge_address, sender, current_block_height,
            ) {
                Ok(exec) if exec.success => {
                    agent_events.push(call_agent::AgentEvent {
                        event_type: call_agent::AgentEventType::AgentBridgeDeposit,
                        agent_id: *agent_id,
                        tx_hash: None,
                        asset_id: *asset_id,
                        amount: *amount,
                        recipient: Some(call_primitives::Address::from_slice(
                            &target_address[..target_address.len().min(20)],
                        )),
                        block_height: current_block_height,
                    });
                    Ok(InstructionResult::Success)
                }
                Ok(_) => {
                    let _ = agent_balances.credit(agent.owner, *agent_id, *asset_id, *amount);
                    let _ = account.credit_balance(*asset_id, sender, *amount);
                    Err(ConsensusError::InvalidBlock(
                        "agent bridge deposit failed".into(),
                    ))
                }
                Err(e) => {
                    let _ = agent_balances.credit(agent.owner, *agent_id, *asset_id, *amount);
                    let _ = account.credit_balance(*asset_id, sender, *amount);
                    Err(ConsensusError::InvalidBlock(format!(
                        "agent bridge deposit: {e:?}"
                    )))
                }
            }
        }
        Instruction::RegisterAgent {
            pubkey,
            name,
            url,
        } => {
            if pubkey.len() != 64 {
                return Err(ConsensusError::InvalidBlock(
                    "RegisterAgent: pubkey must be 64 bytes".into(),
                ));
            }
            let mut pk = [0u8; 64];
            pk.copy_from_slice(pubkey);
            let fee = fee_params.base_fee;
            if fee > 0 {
                let sender_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &sender);
                if sender_balance < fee {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "RegisterAgent: insufficient CALL balance for fee: need {fee}, have {sender_balance}"
                    )));
                }
                account
                    .deduct_balance(call_protocol::CALL_ASSET_ID, sender, fee)
                    .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAgent: {e}")))?;
            }
            agent_registry
                .register_agent(
                    sender,
                    pk,
                    name.clone(),
                    url.clone(),
                    [0u8; 32],
                    None,
                    current_block_height,
                    None,
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAgent: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::GrantAgentBalance {
            agent_id,
            asset_id,
            amount,
        } => {
            let agent = agent_registry
                .get_agent(*agent_id)
                .ok_or_else(|| {
                    ConsensusError::InvalidBlock(format!(
                        "GrantAgentBalance: agent {agent_id} not found"
                    ))
                })?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "GrantAgentBalance: only agent owner can grant".into(),
                ));
            }
            agent_balances
                .grant_funds(sender, *agent_id, *asset_id, *amount, account)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!("GrantAgentBalance: {e:?}"))
                })?;
            Ok(InstructionResult::Success)
        }
        Instruction::RevokeAgentBalance {
            agent_id,
            asset_id,
        } => {
            let agent = agent_registry
                .get_agent(*agent_id)
                .ok_or_else(|| {
                    ConsensusError::InvalidBlock(format!(
                        "RevokeAgentBalance: agent {agent_id} not found"
                    ))
                })?;
            if agent.owner != sender {
                return Err(ConsensusError::InvalidBlock(
                    "RevokeAgentBalance: only agent owner can revoke".into(),
                ));
            }
            agent_balances.revoke_funds(sender, *agent_id, *asset_id);
            Ok(InstructionResult::Success)
        }
        _ => Err(ConsensusError::InvalidBlock("not an agent instruction".into())),
    }
}

// ── Asset instruction helpers ─────────────────────────────────────────

fn is_asset_instruction(instr: &Instruction) -> bool {
    matches!(
        instr,
        Instruction::RegisterAsset { .. } | Instruction::RegisterEvmBridge { .. }
    )
}

fn execute_asset_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    account: &mut AccountState,
    registry: &mut AssetRegistry,
    governance: Option<&mut GovernanceManager>,
    evm_state: &mut call_evm::EvmState,
    evm_executor: &call_evm::EvmExecutor,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::RegisterAsset {
            symbol,
            name,
            decimals,
        } => {
            // 1. Collect asset registration fee
            let fee = {
                let gov = governance.as_ref().ok_or_else(|| {
                    ConsensusError::InvalidBlock("governance not available".into())
                })?;
                gov.config.asset_registration_fee
            };
            if fee > 0 {
                let sender_balance = account.get_balance(call_protocol::CALL_ASSET_ID, &sender);
                if sender_balance < fee {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "RegisterAsset: insufficient CALL balance for fee: need {fee}, have {sender_balance}"
                    )));
                }
                account
                    .deduct_balance(call_protocol::CALL_ASSET_ID, sender, fee)
                    .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAsset: {e}")))?;
            }

            // 2. Register asset
            let asset_id = registry
                .register_asset(symbol.clone(), name.clone(), *decimals, sender, 0, current_block_height)
                .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAsset: {e}")))?;

            // 3. Deploy EVM wrapped token via system deployer
            let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
            evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
            evm_state.create_account(deployer);

            let (contract_addr, deploy_result) = evm_executor
                .deploy_erc20_template(
                    deployer,
                    evm_state,
                    name,
                    symbol,
                    *decimals,
                    call_protocol::BRIDGE_EVM_ADDRESS,
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterAsset: ERC-20 deploy failed: {e:?}")))?;

            if !deploy_result.success {
                return Err(ConsensusError::InvalidBlock(
                    "RegisterAsset: ERC-20 deployment reverted".into(),
                ));
            }

            // 4. Bind contract address
            registry.set_evm_contract_address(asset_id, contract_addr);

            Ok(InstructionResult::Success)
        }
        Instruction::RegisterEvmBridge {
            asset_id,
            evm_contract_address,
        } => {
            // 1. Verify asset exists
            let asset = registry.get_asset(*asset_id).ok_or_else(|| {
                ConsensusError::InvalidBlock(format!(
                    "RegisterEvmBridge: asset {} not registered",
                    asset_id
                ))
            })?;

            // 2. Verify sender is the issuer
            if asset.issuer != sender {
                return Err(ConsensusError::InvalidBlock(
                    "RegisterEvmBridge: only the asset issuer can register an EVM bridge".into(),
                ));
            }

            // 3. Check evm_contract_address is not already set
            if asset.evm_contract_address.is_some() {
                return Err(ConsensusError::InvalidBlock(
                    "RegisterEvmBridge: EVM contract address already set".into(),
                ));
            }

            // 4. Verify contract has code
            let code = evm_state.get_code(evm_contract_address);
            if code.is_empty() {
                return Err(ConsensusError::InvalidBlock(
                    "RegisterEvmBridge: no code at contract address".into(),
                ));
            }

            // 5. Static call totalSupply() — reject if > 0
            // selector for totalSupply(): 0x18160ddd
            let tx = call_evm::EvmTransaction {
                caller: sender,
                nonce: evm_state.get_nonce(&sender),
                gas_limit: 50_000,
                gas_price: 10,
                to: Some(*evm_contract_address),
                value: call_primitives::U256::ZERO,
                data: call_evm::Bytes::from(vec![0x18, 0x16, 0x0d, 0xdd]),
                chain_id: evm_executor.chain_id,
            };
            let static_result = evm_executor
                .execute_tx(tx, evm_state)
                .map_err(|e| ConsensusError::InvalidBlock(format!("RegisterEvmBridge: static call failed: {e:?}")))?;

            if !static_result.success {
                return Err(ConsensusError::InvalidBlock(
                    "RegisterEvmBridge: totalSupply() static call reverted".into(),
                ));
            }

            if static_result.output.len() >= 32 {
                let total_supply_bytes: [u8; 32] = static_result.output[..32].try_into().unwrap_or([0u8; 32]);
                let total_supply = call_primitives::U256::from_be_bytes(total_supply_bytes);
                if total_supply > call_primitives::U256::ZERO {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "RegisterEvmBridge: totalSupply must be 0, got {}",
                        total_supply
                    )));
                }
            }

            // 6. Bind contract address
            registry.set_evm_contract_address(*asset_id, *evm_contract_address);

            Ok(InstructionResult::Success)
        }
        _ => Err(ConsensusError::InvalidBlock("not an asset instruction".into())),
    }
}

// ── Bridge instruction helpers ────────────────────────────────────────

fn is_bridge_instruction(instr: &Instruction) -> bool {
    matches!(instr, Instruction::ExternalBridgeDeposit { .. } | Instruction::ExternalBridgeWithdraw { .. } | Instruction::ChallengeBridgeDeposit { .. } | Instruction::BridgeDeposit { .. } | Instruction::BridgeToEvm { .. } | Instruction::WithdrawFromEvm { .. })
}

fn execute_bridge_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    account: &mut AccountState,
    bridge_state: &mut call_bridge::BridgeStateManager,
    config: &call_bridge::BridgeConfig,
    validators: &[call_primitives::Address],
    current_block_height: u64,
    evm_state: &mut call_evm::EvmState,
    evm_executor: &call_evm::EvmExecutor,
    registry: &call_protocol::registry::AssetRegistry,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::ExternalBridgeDeposit {
            source_tx_hash,
            source_chain,
            source_block_number,
            external_sender,
            recipient,
            asset_id,
            amount,
            validator_signatures,
        } => {
            let chain = match *source_chain {
                0 => call_bridge::ExternalChain::EthereumMainnet,
                1 => call_bridge::ExternalChain::Arbitrum,
                _ => return Err(ConsensusError::InvalidBlock("bridge: unknown source chain".into())),
            };
            let signatures: Vec<call_bridge::BridgeSignature> = validator_signatures
                .iter()
                .map(|(idx, sig)| call_bridge::BridgeSignature {
                    validator_index: *idx,
                    signature: sig.as_slice().try_into().unwrap_or([0u8; 65]),
                })
                .collect();
            let op = call_bridge::ExternalBridgeOp::Deposit {
                source_chain: chain,
                source_tx_hash: call_primitives::B256::from(*source_tx_hash),
                source_block_number: *source_block_number,
                sender: external_sender.clone(),
                recipient: *recipient,
                asset_id: *asset_id,
                amount: *amount,
                signatures,
            };
            match call_bridge::process_external_deposit(
                &op,
                account,
                bridge_state,
                config,
                validators,
                current_block_height,
                None, // source_contract: not provided in ExternalBridgeDeposit; registry check used
            ) {
                Ok(_) => Ok(InstructionResult::Success),
                Err(e) => Err(ConsensusError::InvalidBlock(format!("bridge deposit: {e:?}"))),
            }
        }
        Instruction::ExternalBridgeWithdraw {
            target_chain,
            target_address,
            asset_id,
            sender,
            amount,
        } => {
            let chain = match *target_chain {
                0 => call_bridge::ExternalChain::EthereumMainnet,
                1 => call_bridge::ExternalChain::Arbitrum,
                _ => return Err(ConsensusError::InvalidBlock("bridge: unknown target chain".into())),
            };
            let op = call_bridge::ExternalBridgeOp::Withdraw {
                target_chain: chain,
                target_address: target_address.clone(),
                asset_id: *asset_id,
                sender: *sender,
                amount: *amount,
            };
            match call_bridge::process_external_withdraw(
                &op,
                account,
                bridge_state,
                config,
                current_block_height,
            ) {
                Ok(_) => Ok(InstructionResult::Success),
                Err(e) => Err(ConsensusError::InvalidBlock(format!("bridge withdraw: {e:?}"))),
            }
        }
        Instruction::BridgeDeposit {
            source_chain,
            target_address,
            amount,
            asset_id,
            proof,
        } => {
            // Legacy BridgeDeposit: proof must contain a serialized BridgeDepositProof
            if proof.is_empty() {
                return Err(ConsensusError::InvalidBlock(
                    "bridge deposit: empty proof".into(),
                ));
            }
            let deposit_proof: call_bridge::BridgeDepositProof = serde_json::from_slice(proof)
                .map_err(|e| ConsensusError::InvalidBlock(format!("bridge deposit: invalid proof format: {e}")))?;

            let chain = match *source_chain as u8 {
                0 => call_bridge::ExternalChain::EthereumMainnet,
                1 => call_bridge::ExternalChain::Arbitrum,
                _ => return Err(ConsensusError::InvalidBlock("bridge: unknown source chain".into())),
            };
            let signatures: Vec<call_bridge::BridgeSignature> = deposit_proof.signatures
                .iter()
                .map(|(idx, sig)| call_bridge::BridgeSignature {
                    validator_index: *idx,
                    signature: sig.as_slice().try_into().unwrap_or([0u8; 65]),
                })
                .collect();
            let op = call_bridge::ExternalBridgeOp::Deposit {
                source_chain: chain,
                source_tx_hash: call_primitives::B256::from(deposit_proof.source_tx_hash),
                source_block_number: deposit_proof.source_block_number,
                sender: deposit_proof.external_sender,
                recipient: *target_address,
                asset_id: *asset_id,
                amount: *amount,
                signatures,
            };
            match call_bridge::process_external_deposit(
                &op,
                account,
                bridge_state,
                config,
                validators,
                current_block_height,
                None,
            ) {
                Ok(_) => Ok(InstructionResult::Success),
                Err(e) => Err(ConsensusError::InvalidBlock(format!("bridge deposit: {e:?}"))),
            }
        }
        Instruction::ChallengeBridgeDeposit {
            source_tx_hash,
            proof,
        } => {
            // Permissionless challenge: anyone can submit proof during challenge period
            if proof.is_empty() {
                return Err(ConsensusError::InvalidBlock(
                    "bridge challenge: proof cannot be empty".into(),
                ));
            }
            let revoked = call_bridge::challenge_pending_deposit(
                bridge_state,
                &call_primitives::B256::from(*source_tx_hash),
                current_block_height,
            );
            if revoked {
                Ok(InstructionResult::Success)
            } else {
                Err(ConsensusError::InvalidBlock(
                    "bridge challenge: no pending deposit found for source_tx_hash".into(),
                ))
            }
        }
        Instruction::BridgeToEvm {
            asset_id,
            to,
            amount,
        } => {
            // 1. Reject virtual USD (asset_id == 0)
            if *asset_id == 0 {
                return Err(ConsensusError::InvalidBlock(
                    "BridgeToEvm: asset 0 (USD) is not bridgeable".into(),
                ));
            }

            // 2. Validate asset is registered
            if registry.get_asset(*asset_id).is_none() {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: asset {} not registered",
                    asset_id
                )));
            }

            // 3. Check bridge not paused
            if bridge_state.is_paused(*asset_id) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: bridge paused for asset {}",
                    asset_id
                )));
            }

            // 4. Check per-tx limit
            bridge_state
                .check_per_tx_limit(*amount, config.max_per_tx)
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e}")))?;

            // 5. Check daily limit
            bridge_state
                .check_and_update_daily_limit(
                    *asset_id,
                    *amount,
                    config.daily_limit_per_asset,
                    current_block_height,
                    config.blocks_per_day,
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e}")))?;

            // 6. Check protocol balance is sufficient
            let protocol_balance = account.get_balance(*asset_id, &sender);
            if protocol_balance < *amount {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: insufficient protocol balance for asset {}: have {}, need {}",
                    asset_id, protocol_balance, amount
                )));
            }

            // 7. Deduct protocol balance
            account
                .deduct_balance(*asset_id, sender, *amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e}")))?;

            // 8. Bridge to EVM
            let amount_u256 = call_evm::U256::from(*amount);
            let exec_result = if *asset_id == call_protocol::CALL_ASSET_ID {
                // CALL: transfer as native EVM balance
                let current = evm_state.get_balance(to);
                evm_state.set_balance(*to, current + amount_u256);
                Ok(call_evm::EvmExecutionResult {
                    success: true,
                    gas_used: 21_000,
                    output: call_evm::Bytes::default(),
                    logs: vec![],
                })
            } else {
                // User-defined asset: mint ERC-20 wrapped token
                let Some(contract_addr) = registry.get_evm_contract_address(*asset_id) else {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "BridgeToEvm: no EVM contract registered for asset {}",
                        asset_id
                    )));
                };
                evm_executor
                    .evm_call_bridge_mint(
                        call_protocol::BRIDGE_EVM_ADDRESS,
                        contract_addr,
                        evm_state,
                        *to,
                        amount_u256,
                    )
                    .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToEvm: {e:?}")))
            };

            match exec_result {
                Ok(execution) => {
                    if execution.success {
                        bridge_state.record_deposit(*asset_id, *amount);
                        Ok(InstructionResult::Success)
                    } else {
                        Err(ConsensusError::InvalidBlock(
                            "BridgeToEvm: EVM operation reverted".into(),
                        ))
                    }
                }
                Err(e) => Err(e),
            }
        }
        Instruction::WithdrawFromEvm {
            asset_id,
            to,
            amount,
        } => {
            // 1. Reject virtual USD (asset_id == 0)
            if *asset_id == 0 {
                return Err(ConsensusError::InvalidBlock(
                    "WithdrawFromEvm: asset 0 (USD) is not bridgeable".into(),
                ));
            }

            // 2. Validate asset is registered
            if registry.get_asset(*asset_id).is_none() {
                return Err(ConsensusError::InvalidBlock(format!(
                    "WithdrawFromEvm: asset {} not registered",
                    asset_id
                )));
            }

            // 3. Check bridge not paused
            if bridge_state.is_paused(*asset_id) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "WithdrawFromEvm: bridge paused for asset {}",
                    asset_id
                )));
            }

            // 4. Check per-tx limit
            bridge_state
                .check_per_tx_limit(*amount, config.max_per_tx)
                .map_err(|e| ConsensusError::InvalidBlock(format!("WithdrawFromEvm: {e}")))?;

            // 5. Check daily limit
            bridge_state
                .check_and_update_daily_limit(
                    *asset_id,
                    *amount,
                    config.daily_limit_per_asset,
                    current_block_height,
                    config.blocks_per_day,
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("WithdrawFromEvm: {e}")))?;

            let amount_u256 = call_evm::U256::from(*amount);

            // 6. Withdraw from EVM
            let exec_result = if *asset_id == call_protocol::CALL_ASSET_ID {
                // CALL: transfer from native EVM balance
                let evm_balance = evm_state.get_balance(&sender);
                if evm_balance < amount_u256 {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "WithdrawFromEvm: insufficient EVM native balance for CALL: have {}, need {}",
                        evm_balance, amount_u256
                    )));
                }
                evm_state.set_balance(sender, evm_balance - amount_u256);
                Ok(call_evm::EvmExecutionResult {
                    success: true,
                    gas_used: 21_000,
                    output: call_evm::Bytes::default(),
                    logs: vec![],
                })
            } else {
                // User-defined asset: burn ERC-20 wrapped token
                let Some(contract_addr) = registry.get_evm_contract_address(*asset_id) else {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "WithdrawFromEvm: no EVM contract registered for asset {}",
                        asset_id
                    )));
                };
                evm_executor
                    .evm_call_bridge_burn(
                        sender,
                        contract_addr,
                        evm_state,
                        amount_u256,
                    )
                    .map_err(|e| ConsensusError::InvalidBlock(format!("WithdrawFromEvm: {e:?}")))
            };

            match exec_result {
                Ok(execution) => {
                    if execution.success {
                        // 7. Credit protocol balance
                        account
                            .credit_balance(*asset_id, *to, *amount)
                            .map_err(|e| ConsensusError::InvalidBlock(format!("WithdrawFromEvm: {e}")))?;
                        bridge_state.record_withdrawal(*asset_id, *amount);
                        Ok(InstructionResult::Success)
                    } else {
                        Err(ConsensusError::InvalidBlock(
                            "WithdrawFromEvm: EVM operation reverted".into(),
                        ))
                    }
                }
                Err(e) => Err(e),
            }
        }
        _ => Err(ConsensusError::InvalidBlock("not a bridge instruction".into())),
    }
}

// ── Validator instruction helpers ─────────────────────────────────────

fn is_validator_instruction(instr: &Instruction) -> bool {
    matches!(
        instr,
        Instruction::ValidatorStake { .. }
            | Instruction::ValidatorUnstake { .. }
            | Instruction::ValidatorClaimUnbonded { .. }
    )
}

fn execute_validator_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    account: &mut AccountState,
    validator_state: &mut ValidatorStateManager,
    current_block_height: u64,
) -> Result<InstructionResult, ConsensusError> {
    match instruction {
        Instruction::ValidatorStake {
            ed25519_pubkey,
            self_stake,
        } => {
            // Verify sender has sufficient balance
            let balance = account.get_balance(call_protocol::CALL_ASSET_ID, &sender);
            if balance < *self_stake {
                return Err(ConsensusError::InvalidBlock(
                    "validator stake: insufficient balance".into(),
                ));
            }
            // Transfer stake to escrow (Cosmos-style module account)
            account
                .transfer(call_protocol::CALL_ASSET_ID, sender, STAKING_ESCROW, *self_stake)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator stake: {e}")))?;
            // Register validator
            validator_state.set_current_block(current_block_height);
            validator_state
                .stake(sender, *ed25519_pubkey, *self_stake)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator stake: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::ValidatorUnstake { validator_id } => {
            validator_state.set_current_block(current_block_height);
            validator_state
                .unstake(*validator_id, sender)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator unstake: {e}")))?;
            Ok(InstructionResult::Success)
        }
        Instruction::ValidatorClaimUnbonded { validator_id } => {
            validator_state.set_current_block(current_block_height);
            let (amount, recipient) = validator_state
                .claim_unbonded(*validator_id)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator claim: {e}")))?;
            // Return staked tokens from escrow to the original staker
            account
                .transfer(call_protocol::CALL_ASSET_ID, STAKING_ESCROW, recipient, amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("validator claim: {e}")))?;
            Ok(InstructionResult::Success)
        }
        _ => Err(ConsensusError::InvalidBlock(
            "not a validator instruction".into(),
        )),
    }
}

// ── Rollback instruction helpers ──────────────────────────────────────

fn is_rollback_instruction(instr: &Instruction) -> bool {
    matches!(instr, Instruction::SubmitRollbackSignature { .. })
}

fn execute_rollback_instruction(
    instruction: &Instruction,
    fork_manager: &mut ForkManager,
    current_block_height: u64,
) -> Result<Option<RollbackPlan>, ConsensusError> {
    match instruction {
        Instruction::SubmitRollbackSignature {
            validator_id,
            target_height,
            target_version_major,
            target_version_minor,
            target_version_patch,
            nonce,
            signature,
        } => {
            if signature.len() != 64 {
                return Err(ConsensusError::InvalidBlock(
                    "SubmitRollbackSignature: signature must be 64 bytes".into(),
                ));
            }
            let mut sig = [0u8; 64];
            sig.copy_from_slice(signature);
            let target_version = ProtocolVersion::new(
                *target_version_major,
                *target_version_minor,
                *target_version_patch,
            );
            let sig_result = fork_manager.submit_rollback_signature(
                *validator_id,
                *target_height,
                target_version,
                *nonce,
                sig,
            );
            match sig_result {
                Ok(Some(rollback_result)) => {
                    let plan = fork_manager.execute_rollback(rollback_result, current_block_height);
                    Ok(Some(plan))
                }
                Ok(None) => Ok(None),
                Err(e) => Err(ConsensusError::InvalidBlock(format!(
                    "SubmitRollbackSignature: {e}"
                ))),
            }
        }
        _ => Err(ConsensusError::InvalidBlock(
            "not a rollback instruction".into(),
        )),
    }
}

// ── Block Execution Result ────────────────────────────────────────────

/// Result of executing all transactions in a block
#[derive(Debug, Default)]
pub struct BlockExecutionResult {
    pub instruction_results: Vec<InstructionResult>,
    pub payment_root: Hash,
    pub evm_state_root: Hash,
    pub bridge_root: Hash,
    pub receipt_root: Hash,
    pub evm_tx_count: usize,
    pub protocol_tx_count: usize,
    pub bridge_op_count: usize,
    pub system_tx_count: usize,
    pub total_validator_reward: Balance,
    /// Total gas used by EVM transactions
    pub evm_gas_used: u64,
    /// Agent activity events emitted during block execution
    pub agent_events: Vec<call_agent::AgentEvent>,
    /// Emergency rollback plan produced during block execution (quorum reached)
    pub pending_rollback: Option<RollbackPlan>,
}

impl BlockExecutionResult {
    /// Total transactions processed
    pub fn total_tx_count(&self) -> usize {
        self.evm_tx_count + self.protocol_tx_count + self.bridge_op_count + self.system_tx_count
    }
}

// ── Helper Functions ──────────────────────────────────────────────────

/// Update base fee after block execution (delegates to protocol layer)
fn update_base_fee_after_block(params: &mut FeeParams, gas_used: u64) {
    call_protocol::transaction::update_base_fee(params, gas_used);
}

/// Compute payment root from current balance state
fn compute_payment_root(account: &AccountState) -> Hash {
    let mut leaves: Vec<Hash> = account
        .balances
        .iter()
        .map(|(&(asset_id, addr), &balance)| {
            let mut data = Vec::with_capacity(60);
            data.extend_from_slice(&asset_id.to_le_bytes());
            data.extend_from_slice(addr.as_slice());
            data.extend_from_slice(&balance.to_le_bytes());
            keccak256(&data)
        })
        .collect();

    if leaves.is_empty() {
        return Hash::ZERO;
    }

    leaves.sort();
    build_merkle_root(&leaves).unwrap_or(Hash::ZERO)
}

/// Compute bridge state root
fn compute_bridge_root(
    bridge_state: &call_bridge::BridgeStateManager,
) -> Hash {
    let mut data = Vec::new();
    data.extend_from_slice(&(bridge_state.pending_ops.len() as u64).to_le_bytes());
    for (asset_id, total) in &bridge_state.total_deposits {
        data.extend_from_slice(&asset_id.to_le_bytes());
        data.extend_from_slice(&total.to_le_bytes());
    }
    for (asset_id, total) in &bridge_state.total_withdrawals {
        data.extend_from_slice(&asset_id.to_le_bytes());
        data.extend_from_slice(&total.to_le_bytes());
    }
    keccak256(&data)
}

/// Compute EVM state root from the EVM state trie
fn compute_evm_state_root(evm_state: &EvmState) -> Hash {
    evm_state.compute_state_root()
}

/// Compute receipt root from block execution results
fn compute_receipt_root(result: &BlockExecutionResult) -> Hash {
    let mut data = Vec::new();
    data.extend_from_slice(&result.evm_gas_used.to_le_bytes());
    data.extend_from_slice(&(result.evm_tx_count as u64).to_le_bytes());
    data.extend_from_slice(&(result.protocol_tx_count as u64).to_le_bytes());
    data.extend_from_slice(&(result.bridge_op_count as u64).to_le_bytes());
    for instr_result in &result.instruction_results {
        match instr_result {
            InstructionResult::Success => data.push(1),
            InstructionResult::Reverted { reason } => {
                data.push(0);
                data.extend_from_slice(reason.as_bytes());
            }
        }
    }
    for event in &result.agent_events {
        data.extend_from_slice(&event.agent_id.to_be_bytes());
        data.extend_from_slice(&event.asset_id.to_be_bytes());
        data.extend_from_slice(&event.amount.to_be_bytes());
        data.extend_from_slice(&event.block_height.to_be_bytes());
    }
    keccak256(&data)
}

/// Attempt to decode raw EVM transaction bytes into a structured EvmTransaction.
/// Supports RLP-encoded Legacy and EIP-1559 transactions.
/// Falls back to JSON deserialization for backward compatibility with test data.
/// Returns Err if the bytes cannot be parsed as a valid EVM tx.
fn decode_evm_tx(raw: &[u8]) -> Result<EvmTransaction, ()> {
    use alloy_consensus::{Transaction, TxEnvelope};
    use alloy_rlp::Decodable;

    if raw.is_empty() {
        return Err(());
    }

    // 1) Try RLP / EIP-2718 enveloped decoding (real wallet transactions)
    if let Ok(envelope) = TxEnvelope::decode(&mut &raw[..]) {
        match envelope {
            TxEnvelope::Legacy(signed) => {
                let tx = signed.tx();
                let caller = signed.recover_signer().map_err(|_| ())?;
                return Ok(EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.gas_price().unwrap_or(0),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(1),
                });
            }
            TxEnvelope::Eip1559(signed) => {
                let tx = signed.tx();
                let caller = signed.recover_signer().map_err(|_| ())?;
                return Ok(EvmTransaction {
                    caller,
                    nonce: tx.nonce(),
                    gas_limit: tx.gas_limit(),
                    gas_price: tx.max_fee_per_gas(),
                    to: tx.to(),
                    value: tx.value(),
                    data: tx.input().clone(),
                    chain_id: tx.chain_id().unwrap_or(1),
                });
            }
            // Unsupported transaction types (EIP-2930, EIP-4844, EIP-7702)
            _ => return Err(()),
        }
    }

    // 2) Fallback: JSON-serialized EvmTransaction (test data / internal tooling)
    if let Ok(tx) = serde_json::from_slice::<EvmTransaction>(raw) {
        return Ok(tx);
    }

    Err(())
}

// ── Consensus Error (re-exported for block validation) ────────────────
// Defined in validator.rs, re-exported via lib.rs

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ForkManager;
    use call_primitives::Address;
    use call_protocol::instructions::Instruction;
    use call_protocol::transaction::{AuthScheme, GasConfig};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    fn test_fork_manager() -> ForkManager {
        ForkManager::new(ProtocolVersion::new(1, 0, 0), 1)
    }

    const TEST_VERSION: ProtocolVersion = ProtocolVersion::new(1, 0, 0);

    fn make_test_evm_tx() -> EvmTransaction {
        EvmTransaction {
            caller: test_addr(1),
            nonce: 0,
            gas_limit: 21_000,
            gas_price: 1_000_000_000,
            to: Some(test_addr(2)),
            value: call_primitives::U256::from(100),
            data: call_evm::Bytes::default(),
            chain_id: 1,
        }
    }

    fn make_test_tx() -> ProtocolTransaction {
        ProtocolTransaction {
            sender: test_addr(1),
            nonce: 0,
            instructions: vec![Instruction::Transfer {
                asset_id: call_protocol::CALL_ASSET_ID,
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
        }
    }

    /// Create a protocol transaction with a valid secp256k1 signature.
    /// Returns the transaction and the sender address (derived from the signing key).
    fn make_signed_test_tx() -> (ProtocolTransaction, Address) {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);
        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::Transfer {
                asset_id: call_protocol::CALL_ASSET_ID,
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
        let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
        tx.auth = AuthScheme::SingleSig { signature };
        (tx, sender)
    }

    #[test]
    fn test_block_structure_serialization() {
        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            TEST_VERSION,
            vec![make_test_tx()],
            vec![vec![0u8; 100]],
            vec![SystemTx {
                kind: SystemTxKind::UpdateBaseFee,
                data: vec![],
            }],
            vec![],
        );

        assert_eq!(block.header.height, 1);
        assert_eq!(block.header.proposer, 1);
        assert_eq!(block.protocol_txs.len(), 1);
        assert_eq!(block.evm_txs.len(), 1);
        assert_eq!(block.system_txs.len(), 1);
        assert_eq!(block.bridge_operations.len(), 0);
    }

    #[test]
    fn test_block_header_hash() {
        let header = BlockHeader {
            parent_hash: BlockHash::ZERO,
            height: 1,
            timestamp_millis: 1000,
            payment_root: Hash::ZERO,
            evm_state_root: Hash::ZERO,
            bridge_root: Hash::ZERO,
            receipt_root: Hash::ZERO,
            proposer: 1,
            signature: BlockSignature::default(),
            version: TEST_VERSION,
            bls_aggregate_signature: None,
            bls_signer_bitmap: Vec::new(),
        };

        let hash = header.hash();
        // Hash should be deterministic
        let hash2 = header.hash();
        assert_eq!(hash, hash2);
        // Hash should be 32 bytes
        assert_eq!(hash.as_slice().len(), 32);
    }

    #[test]
    fn test_block_header_validate() {
        let header = BlockHeader {
            parent_hash: BlockHash::repeat_byte(1),
            height: 2,
            timestamp_millis: 1000,
            payment_root: Hash::ZERO,
            evm_state_root: Hash::ZERO,
            bridge_root: Hash::ZERO,
            receipt_root: Hash::ZERO,
            proposer: 1,
            signature: BlockSignature::default(),
            version: TEST_VERSION,
            bls_aggregate_signature: None,
            bls_signer_bitmap: Vec::new(),
        };
        let fm = test_fork_manager();

        assert!(header.validate(BlockHash::repeat_byte(1), &fm).is_ok());
        assert!(header.validate(BlockHash::repeat_byte(2), &fm).is_err());
    }

    #[test]
    fn test_block_validate() {
        let block = Block::new(
            1,
            BlockHash::repeat_byte(1),
            1000,
            1,
            TEST_VERSION,
            vec![make_test_tx()],
            vec![],
            vec![],
            vec![],
        );
        let fm = test_fork_manager();

        assert!(block.validate(BlockHash::repeat_byte(1), &fm).is_ok());
        assert!(block.validate(BlockHash::repeat_byte(2), &fm).is_err());
    }

    #[test]
    fn test_block_validate_duplicate_nonce() {
        let tx = make_test_tx();
        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            TEST_VERSION,
            vec![tx.clone(), tx],
            vec![],
            vec![],
            vec![],
        );
        let fm = test_fork_manager();

        let result = block.validate(BlockHash::ZERO, &fm);
        assert!(result.is_err());
    }

    #[test]
    fn test_block_execution_order() {
        let evm_tx = make_test_evm_tx();
        let evm_bytes = serde_json::to_vec(&evm_tx).unwrap();
        let (protocol_tx, sender) = make_signed_test_tx();
        let mut block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            TEST_VERSION,
            vec![protocol_tx],
            vec![evm_bytes],
            vec![SystemTx {
                kind: SystemTxKind::ValidatorReward {
                    proposer: 1,
                    reward: 1000,
                },
                data: vec![],
            }],
            vec![BridgeOp::DepositToEvm {
                asset_id: call_protocol::CALL_ASSET_ID,
                from: sender,
                to: test_addr(2),
                amount: 500,
            }],
        );

        // Setup balance state
        let mut account = AccountState::new();
        account
            .balances
            .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
            .unwrap();
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0)
            .unwrap();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(sender, call_primitives::U256::from(100_000_000_000_000u128));
        evm_state.set_balance(test_addr(1), call_primitives::U256::from(100_000_000_000_000u128));

        // Deploy wrapped token contract for asset 1
        let deploy_executor = call_evm::EvmExecutor::new(1);
        let (contract_addr, deploy_result) = deploy_executor
            .deploy_erc20_template(
                sender,
                &mut evm_state,
                "CALL",
                "CALL",
                18,
                call_protocol::BRIDGE_EVM_ADDRESS,
            )
            .unwrap();
        assert!(deploy_result.success, "ERC-20 deploy failed");
        registry.set_evm_contract_address(1, contract_addr);

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let bridge_config = call_bridge::BridgeConfig::default();

        let result = block
            .execute(
                &mut account,
                &mut registry,
                &mut compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                1,
                &mut evm_state,
                None,
                None,
                None,
                None,
                None,
                Some(&bridge_config),
                None,
                None,
                None,
                None,
            )
            .unwrap();

        // Execution order: EVM(1) → Protocol(1) → Bridge(1) → System(1)
        assert_eq!(result.evm_tx_count, 1);
        assert_eq!(result.protocol_tx_count, 1);
        assert_eq!(result.bridge_op_count, 1);
        assert_eq!(result.system_tx_count, 1);

        // Finalize
        block.finalize(&result);
        assert_ne!(block.header.payment_root, Hash::ZERO);
        assert_ne!(block.header.bridge_root, Hash::ZERO);
    }

    #[test]
    fn test_base_fee_update_after_block() {
        let mut fee_params = FeeParams::default();
        fee_params.base_fee = 100;

        // High gas usage should increase fee
        update_base_fee_after_block(&mut fee_params, 15_000_000);
        assert!(fee_params.base_fee > 100);

        // Low gas usage should decrease fee
        fee_params.base_fee = 100;
        update_base_fee_after_block(&mut fee_params, 5_000_000);
        assert!(fee_params.base_fee < 100);
    }

    #[test]
    fn test_system_tx_reward_distribution() {
        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            TEST_VERSION,
            vec![],
            vec![],
            vec![SystemTx {
                kind: SystemTxKind::ValidatorReward {
                    proposer: 1,
                    reward: 500_000,
                },
                data: vec![],
            }],
            vec![],
        );

        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();
        let bridge_config = call_bridge::BridgeConfig::default();

        let result = block
            .execute(
                &mut account,
                &mut registry,
                &mut compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                1,
                &mut evm_state,
                None,
                None,
                None,
                None,
                None,
                Some(&bridge_config),
                None,
                None,
                None,
                None,
            )
            .unwrap();

        assert_eq!(result.total_validator_reward, 500_000);
    }

    #[test]
    fn test_block_new_builder() {
        let block = Block::new(
            42,
            BlockHash::repeat_byte(0xFF),
            999_000,
            7,
            TEST_VERSION,
            vec![],
            vec![],
            vec![],
            vec![],
        );

        assert_eq!(block.header.height, 42);
        assert_eq!(block.header.parent_hash, BlockHash::repeat_byte(0xFF));
        assert_eq!(block.header.timestamp_millis, 999_000);
        assert_eq!(block.header.proposer, 7);
        assert_eq!(block.header.version, TEST_VERSION);
    }

    #[test]
    fn test_block_execution_result() {
        let result = BlockExecutionResult {
            evm_tx_count: 10,
            protocol_tx_count: 5,
            bridge_op_count: 2,
            system_tx_count: 1,
            ..Default::default()
        };

        assert_eq!(result.total_tx_count(), 18);
    }

    #[test]
    fn test_agent_instruction_emits_event() {
        use call_agent::{AgentBalances, AgentEventType, AgentRegistry};
        use call_protocol::instructions::AgentPayment;

        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        // Register agent
        let mut agent_registry = AgentRegistry::new_with_format_verifier();
        let agent_id = agent_registry
            .register_agent(
                sender,
                pubkey,
                "test-agent".into(),
                "https://test.com".into(),
                [0u8; 32],
                None,
                1,
                None,
            )
            .unwrap();

        // Fund owner and grant to agent
        let mut account = AccountState::new();
        account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000).unwrap();
        let mut agent_balances = AgentBalances::new();
        agent_balances
            .grant_funds(sender, agent_id, 1, 5_000, &mut account)
            .unwrap();

        // Build signed AgentPay transaction
        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::AgentPay {
                payment: AgentPayment {
                    agent_id,
                    asset_id: call_protocol::CALL_ASSET_ID,
                    to: test_addr(2),
                    amount: 1_000,
                },
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 100,
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let tx_hash = tx.compute_tx_hash();
        let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
        tx.auth = AuthScheme::SingleSig { signature };

        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            TEST_VERSION,
            vec![tx],
            vec![],
            vec![],
            vec![],
        );

        let mut registry = AssetRegistry::new();
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0)
            .unwrap();
        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();
        let bridge_config = call_bridge::BridgeConfig::default();

        let result = block
            .execute(
                &mut account,
                &mut registry,
                &mut compliance,
                &mut bridge_state,
                &mut shielded_state,
                &mut fee_params,
                50, // current_block_height < expires_at
                &mut evm_state,
                None,
                None,
                Some(&mut agent_balances),
                Some(&mut agent_registry),
                None,
                Some(&bridge_config),
                None,
                None,
                None,
                None,
            )
            .unwrap();

        eprintln!("protocol_tx_count={} agent_events={}", result.protocol_tx_count, result.agent_events.len());
        // Verify agent event was emitted
        assert_eq!(result.agent_events.len(), 1);
        let event = &result.agent_events[0];
        assert!(matches!(event.event_type, AgentEventType::AgentPay));
        assert_eq!(event.agent_id, agent_id);
        assert_eq!(event.asset_id, 1);
        assert_eq!(event.amount, 1_000);
        assert_eq!(event.recipient, Some(test_addr(2)));
        assert_eq!(event.block_height, 50);

        // Verify receipt root includes agent events
        let receipt_root = compute_receipt_root(&result);
        assert_ne!(receipt_root, Hash::ZERO);
    }

    #[test]
    fn test_expired_transaction_rejected() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::Transfer {
                asset_id: call_protocol::CALL_ASSET_ID,
                to: test_addr(2),
                amount: 100,
                memo: None,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 100_000,
            max_fee: 1_000_000,
            expires_at: 50, // expires at block 50
            auth: AuthScheme::SingleSig {
                signature: [0u8; 65],
            },
        };
        let tx_hash = tx.compute_tx_hash();
        let signature = call_crypto::secp256k1_sign(&secret, &tx_hash);
        tx.auth = AuthScheme::SingleSig { signature };

        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            TEST_VERSION,
            vec![tx],
            vec![],
            vec![],
            vec![],
        );

        let mut account = AccountState::new();
        account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000).unwrap();
        let mut registry = AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();
        let bridge_config = call_bridge::BridgeConfig::default();

        let result = block.execute(
            &mut account,
            &mut registry,
            &mut compliance,
            &mut bridge_state,
            &mut shielded_state,
            &mut fee_params,
            51, // current_block_height > expires_at
            &mut evm_state,
            None,
            None,
            None,
            None,
            None,
            Some(&bridge_config),
            None,
            None,
            None,
            None,
        );

        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("expired"), "expected expiry error, got: {err_msg}");
    }
}
