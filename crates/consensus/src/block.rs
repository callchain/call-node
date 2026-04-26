//! T6.1 — Block Structure (per spec §2.4, §2.5)
//!
//! Block and BlockHeader types with hash, validation, and execution.

use call_bridge::{BridgeOp, BridgeConfig};
use call_crypto::{build_merkle_root, keccak256};
use call_governance::GovernanceManager;
use call_primitives::{Address, Balance, BlockHash, Hash, ProtocolVersion};
use call_protocol::account::AccountState;
use call_protocol::instructions::{execute_protocol_instructions, Instruction, InstructionResult};
use call_protocol::registry::{AssetRegistry, AssetStatus};
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
    pub state_root: Hash,
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
        data.extend_from_slice(self.state_root.as_slice());
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

// ── Execution Parameter Grouping ──────────────────────────────────────

/// Core mutable protocol state passed to every block execution.
pub struct ExecutionState<'a> {
    pub account: &'a mut AccountState,
    pub registry: &'a mut AssetRegistry,
    pub compliance: &'a mut call_protocol::compliance::ComplianceEngine,
    pub bridge_state: &'a mut call_bridge::BridgeStateManager,
    pub shielded_state: &'a mut ShieldedState,
    pub evm_state: &'a mut EvmState,
}

/// Block-level context and configuration.
pub struct BlockContext<'a> {
    pub current_block_height: u64,
    pub fee_params: &'a mut FeeParams,
    pub bridge_config: Option<&'a BridgeConfig>,
    pub validators: Option<&'a [Address]>,
}

/// Optional subsystem extensions. Each field is `None` when the subsystem
/// is not active for the current execution context.
pub struct Subsystems<'a> {
    pub oracle: Option<&'a mut call_oracle::OracleManager>,
    pub agent_executor: Option<call_protocol::instructions::AgentExecutor<'a>>,
    pub agent_balances: Option<&'a mut call_agent::AgentBalances>,
    pub agent_registry: Option<&'a mut call_agent::AgentRegistry>,
    pub governance: Option<&'a mut GovernanceManager>,
    pub smart_accounts: Option<&'a call_protocol::smart_accounts::SmartAccountRegistry>,
    pub validator_state: Option<&'a mut ValidatorStateManager>,
    pub fork_manager: Option<&'a mut ForkManager>,
}

impl<'a> ExecutionState<'a> {
    pub fn new(
        account: &'a mut AccountState,
        registry: &'a mut AssetRegistry,
        compliance: &'a mut call_protocol::compliance::ComplianceEngine,
        bridge_state: &'a mut call_bridge::BridgeStateManager,
        shielded_state: &'a mut ShieldedState,
        evm_state: &'a mut EvmState,
    ) -> Self {
        Self { account, registry, compliance, bridge_state, shielded_state, evm_state }
    }
}

impl<'a> BlockContext<'a> {
    pub fn new(
        current_block_height: u64,
        fee_params: &'a mut FeeParams,
    ) -> Self {
        Self { current_block_height, fee_params, bridge_config: None, validators: None }
    }
}

impl<'a> Subsystems<'a> {
    pub fn none() -> Self {
        Self {
            oracle: None,
            agent_executor: None,
            agent_balances: None,
            agent_registry: None,
            governance: None,
            smart_accounts: None,
            validator_state: None,
            fork_manager: None,
        }
    }
}

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
            state_root: Hash::ZERO,
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
    pub fn execute(
        &self,
        state: &mut ExecutionState,
        ctx: &mut BlockContext,
        subsystems: &mut Subsystems,
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
                if call_evm::validate_evm_tx(&tx, state.evm_state).is_err() {
                    continue;
                }

                // Unified gas balance: auto-bridge Protocol→EVM if needed
                let gas_cost_u256 = call_evm::U256::from(tx.gas_limit)
                    .saturating_mul(call_evm::U256::from(tx.gas_price));
                let gas_cost_u128: u128 = gas_cost_u256.try_into().unwrap_or(u128::MAX);
                let evm_balance_u128: u128 =
                    state.evm_state.get_balance(&caller).try_into().unwrap_or(0);

                if evm_balance_u128 < gas_cost_u128 {
                    let needed = gas_cost_u128 - evm_balance_u128;
                    let protocol_balance =
                        state.account.get_balance(call_protocol::CALL_ASSET_ID, &caller);
                    if protocol_balance < needed {
                        continue; // insufficient unified gas
                    }
                    if state.account
                        .deduct_balance(call_protocol::CALL_ASSET_ID, caller, needed)
                        .is_ok()
                    {
                        let new_evm = state.evm_state.get_balance(&caller)
                            + call_evm::U256::from(needed);
                        state.evm_state.set_balance(caller, new_evm);
                    } else {
                        continue;
                    }
                }

                if let Ok(exec_result) = executor.execute_tx(tx, state.evm_state) {
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
            if let Err(_e) = state.account.validate_nonce(&tx.sender, tx.nonce) {
                // Nonce mismatch or already used — skip this tx
                continue;
            }

            // Verify transaction signature before execution
            if let Err(e) = tx.verify_signature_with_registry(subsystems.smart_accounts) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "signature verification failed for tx from {:?}: {e}",
                    tx.sender
                )));
            }

            // Check transaction expiry
            if tx.expires_at != 0 && ctx.current_block_height > tx.expires_at {
                return Err(ConsensusError::InvalidBlock(format!(
                    "transaction from {:?} expired at block {} (current: {})",
                    tx.sender, tx.expires_at, ctx.current_block_height
                )));
            }

            // Compute gas fee
            let gas_units = call_protocol::transaction::calculate_gas_units(&tx.instructions);
            let fee = call_protocol::transaction::compute_fee(
                gas_units,
                call_protocol::transaction::MIN_PRIORITY_FEE_PER_GAS,
                ctx.fee_params.base_fee,
            )
            .min(tx.max_fee);

            // Unified gas balance: auto-bridge EVM→Protocol if needed
            let mut bridged_from_evm = 0u128;
            let protocol_balance = state.account.get_balance(call_protocol::CALL_ASSET_ID, &tx.sender);
            if protocol_balance < fee {
                let needed = fee - protocol_balance;
                let evm_balance_u128: u128 =
                    state.evm_state.get_balance(&tx.sender).try_into().unwrap_or(0);
                if evm_balance_u128 < needed {
                    continue; // insufficient unified gas
                }
                let current_evm = state.evm_state.get_balance(&tx.sender);
                state.evm_state.set_balance(
                    tx.sender,
                    current_evm - call_evm::U256::from(needed),
                );
                if state.account
                    .credit_balance(call_protocol::CALL_ASSET_ID, tx.sender, needed)
                    .is_err()
                {
                    state.evm_state.set_balance(tx.sender, current_evm);
                    continue;
                }
                bridged_from_evm = needed;
            }

            // Deduct gas fee from protocol balance
            let gas_ok = match tx.fee_currency {
                call_primitives::FeeCurrency::Call => state.account
                    .deduct_balance(call_protocol::CALL_ASSET_ID, tx.sender, fee)
                    .is_ok(),
                call_primitives::FeeCurrency::Stablecoin(asset_id) => state.account
                    .deduct_balance(asset_id, tx.sender, fee)
                    .is_ok(),
            };
            if !gas_ok {
                // Rollback EVM bridge if we bridged
                if bridged_from_evm > 0 {
                    let current_evm = state.evm_state.get_balance(&tx.sender);
                    state.evm_state.set_balance(
                        tx.sender,
                        current_evm + call_evm::U256::from(bridged_from_evm),
                    );
                    let _ = state.account.deduct_balance(
                        call_protocol::CALL_ASSET_ID,
                        tx.sender,
                        bridged_from_evm,
                    );
                }
                continue;
            }

            // Increment nonce (consumed on inclusion, regardless of execution result)
            state.account.increment_nonce(tx.sender);

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
            let balance_snapshot = state.account.clone();
            let evm_snapshot = state.evm_state.clone();
            let bridge_snapshot = state.bridge_state.clone();
            let validator_snapshot = subsystems.validator_state.as_ref().map(|vs| (*vs).clone());
            let mut tx_results = Vec::new();
            let mut tx_agent_events = Vec::new();

            let exec_result = (|| -> Result<(), ConsensusError> {
                // Execute regular instructions via protocol engine (includes subsystems.governance)
                if !other_instrs.is_empty() {
                    let results = execute_protocol_instructions(
                        &other_instrs,
                        state.account,
                        state.registry,
                        state.compliance,
                        state.shielded_state,
                        tx.sender,
                        subsystems.oracle.as_deref_mut(),
                        &mut subsystems.agent_executor,
                        subsystems.governance.as_deref_mut(),
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
                            state.account,
                            state.registry,
                            subsystems.governance.as_deref_mut(),
                            state.evm_state,
                            &executor,
                            ctx.current_block_height,
                        )?;
                        tx_results.push(r);
                    }
                }

                // Execute bridge deposit instructions inline
                if !bridge_instrs.is_empty() {
                    let config = ctx.bridge_config.ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "bridge instructions require bridge config".into(),
                        )
                    })?;
                    let vals = ctx.validators.ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "bridge instructions require validator set".into(),
                        )
                    })?;
                    for instr in &bridge_instrs {
                        let r = execute_bridge_instruction(
                            instr,
                            tx.sender,
                            state.account,
                            state.bridge_state,
                            config,
                            vals,
                            ctx.current_block_height,
                            state.evm_state,
                            &executor,
                            state.registry,
                        )?;
                        tx_results.push(r);
                    }
                }

                // Execute agent instructions inline
                if !agent_instrs.is_empty() {
                    let ab = subsystems.agent_balances
                        .as_mut()
                        .ok_or_else(|| {
                            ConsensusError::InvalidBlock(
                                "agent instructions require agent state".into(),
                            )
                        })?;
                    let ar = subsystems.agent_registry.as_mut().ok_or_else(|| {
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
                            state.account,
                            state.evm_state,
                            state.bridge_state,
                            state.registry,
                            &executor,
                            ctx.current_block_height,
                            ctx.fee_params,
                            &mut tx_agent_events,
                        )?;
                        tx_results.push(r);
                    }
                }

                // Execute validator instructions inline
                if !validator_instrs.is_empty() {
                    let vs = subsystems.validator_state.as_mut().ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "validator instructions require validator state".into(),
                        )
                    })?;
                    for instr in &validator_instrs {
                        let r = execute_validator_instruction(
                            instr,
                            tx.sender,
                            state.account,
                            vs,
                            ctx.current_block_height,
                        )?;
                        tx_results.push(r);
                    }
                }

                // Execute rollback instructions inline
                if !rollback_instrs.is_empty() {
                    let fm = subsystems.fork_manager.as_mut().ok_or_else(|| {
                        ConsensusError::InvalidBlock(
                            "rollback instructions require fork manager".into(),
                        )
                    })?;
                    for instr in &rollback_instrs {
                        let maybe_plan = execute_rollback_instruction(
                            instr,
                            fm,
                            ctx.current_block_height,
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
                // Rollback balance/EVM/bridge but nonce stays consumed.
                // Include the failed tx in the block with a Reverted result.
                *state.account = balance_snapshot;
                *state.evm_state = evm_snapshot;
                *state.bridge_state = bridge_snapshot;
                if let Some(ref snapshot) = validator_snapshot {
                    if let Some(ref mut vs) = subsystems.validator_state {
                        **vs = snapshot.clone();
                    }
                }
                result.protocol_tx_count += 1;
                result.instruction_results.push(InstructionResult::Reverted {
                    reason: e.to_string(),
                });
                tracing::warn!(error = %e, sender = ?tx.sender, nonce = tx.nonce, "block: tx execution failed, included as reverted");
            } else {
                result.protocol_tx_count += 1;
                result.instruction_results.extend(tx_results);
                result.agent_events.extend(tx_agent_events);
            }
        }

        // Step 3: Bridge operations
        // Execute internal bridge deposits/withdrawals atomically.
        // Each operation deducts/credits protocol account and mints/burns
        // wrapped ERC-20 tokens in the EVM layer.
        if let Some(config) = ctx.bridge_config {
            for op in &self.bridge_operations {
                let asset_id = op.asset_id();
                // Reject bridge ops on frozen or delisted assets
                if let Some(asset) = state.registry.get_asset(asset_id) {
                    if asset.status != AssetStatus::Active {
                        return Err(ConsensusError::InvalidBlock(format!(
                            "bridge op: asset {} is not active (status: {:?})",
                            asset_id, asset.status
                        )));
                    }
                }
                let Some(contract_addr) = state.registry.get_evm_contract_address(asset_id) else {
                    // Asset has no deployed wrapped token — skip
                    continue;
                };

                let exec_result = match op {
                    BridgeOp::DepositToEvm { from, .. } => {
                        call_bridge::execute_deposit(
                            op,
                            state.account,
                            state.evm_state,
                            &executor,
                            state.bridge_state,
                            config,
                            state.registry,
                            contract_addr,
                            *from,
                            ctx.current_block_height,
                        )
                    }
                    BridgeOp::WithdrawToProtocol { from, .. } => {
                        call_bridge::execute_withdraw(
                            op,
                            state.account,
                            state.evm_state,
                            &executor,
                            state.bridge_state,
                            config,
                            state.registry,
                            contract_addr,
                            *from,
                            ctx.current_block_height,
                        )
                    }
                };

                match exec_result {
                    Ok(_) => {
                        // Update EVM supply tracking in asset registry
                        match op {
                            BridgeOp::DepositToEvm { asset_id, amount, .. } => {
                                let _ = state.registry.add_evm_supply(*asset_id, *amount);
                            }
                            BridgeOp::WithdrawToProtocol { asset_id, amount, .. } => {
                                let _ = state.registry.sub_evm_supply(*asset_id, *amount);
                            }
                        }
                        state.bridge_state.add_pending_op(op.clone(), ctx.current_block_height);
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
                    update_base_fee_after_block(ctx.fee_params, gas_used);
                }
                SystemTxKind::ProtocolUpgrade(_) => {
                    // Version upgrade handled by node layer
                }
            }
            result.system_tx_count += 1;
        }

        // Step 4.5: Auto-finalize pending external deposits and bridge maintenance
        if let Some(config) = ctx.bridge_config {
            state.bridge_state.on_block_finalized(
                ctx.current_block_height,
                config.challenge_period_blocks,
                config.processed_tx_retention_blocks,
            );
        }

        // Step 5: Oracle reward pool allocation from block fees
        if let Some(oracle) = subsystems.oracle.as_mut() {
            // Approximate total gas used: EVM gas + protocol txs * base gas per tx
            let total_gas = result.evm_gas_used + result.protocol_tx_count as u64 * 21_000;
            let total_fees = total_gas as u128 * ctx.fee_params.base_fee;
            let oracle_share = total_fees * ctx.fee_params.oracle_fee_share_bps as u128 / 10_000;
            oracle.add_reward(oracle_share);
            // Note: clear_tracking is NOT called here — the caller must call it
            // after processing outliers for slashing, otherwise outlier data is lost.
        }

        // Compute state roots
        result.payment_root = compute_payment_root(state.account);
        result.evm_state_root = compute_evm_state_root(state.evm_state);
        result.bridge_root = compute_bridge_root(state.bridge_state);
        result.receipt_root = compute_receipt_root(&result);

        Ok(result)
    }

    /// Update header roots after execution
    pub fn finalize(&mut self, result: &BlockExecutionResult) {
        self.header.payment_root = result.payment_root;
        self.header.evm_state_root = result.evm_state_root;
        self.header.bridge_root = result.bridge_root;
        self.header.receipt_root = result.receipt_root;
        self.header.state_root = keccak256(&[
            self.header.payment_root.as_slice(),
            self.header.evm_state_root.as_slice(),
            self.header.bridge_root.as_slice(),
            self.header.receipt_root.as_slice(),
        ].concat());
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
    registry: &mut AssetRegistry,
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
                    let _ = registry.add_evm_supply(*asset_id, *amount);
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
    matches!(instr, Instruction::RegisterAsset { .. } | Instruction::EvmIssuerMint { .. })
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
            max_supply,
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
                .register_asset(symbol.clone(), name.clone(), *decimals, sender, 0, current_block_height, *max_supply)
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
                    sender,
                    call_primitives::U256::from(*max_supply),
                    call_primitives::U256::from(asset_id),
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
        Instruction::EvmIssuerMint {
            asset_id,
            to,
            amount,
        } => {
            // 1. Asset must exist and be active
            let asset = registry
                .get_asset(*asset_id)
                .ok_or_else(|| {
                    ConsensusError::InvalidBlock(format!(
                        "EvmIssuerMint: asset {} not found",
                        asset_id
                    ))
                })?;
            if asset.status != AssetStatus::Active {
                return Err(ConsensusError::InvalidBlock(format!(
                    "EvmIssuerMint: asset {} is not active (status: {:?})",
                    asset_id, asset.status
                )));
            }

            // 2. Only issuer can mint
            if asset.issuer != sender {
                return Err(ConsensusError::InvalidBlock(
                    "EvmIssuerMint: caller is not asset issuer".into(),
                ));
            }

            // 3. CALL (asset_id == 1) has no wrapped ERC-20 contract
            if *asset_id == call_protocol::CALL_ASSET_ID {
                return Err(ConsensusError::InvalidBlock(
                    "EvmIssuerMint: CALL asset has no EVM wrapped token".into(),
                ));
            }

            // 4. Cap check (read-only on registry)
            if asset.would_exceed_cap(*amount) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "EvmIssuerMint: cap exceeded for asset {}",
                    asset_id
                )));
            }

            // 5. Must have an EVM contract address
            let contract_addr = asset.evm_contract_address.ok_or_else(|| {
                ConsensusError::InvalidBlock(format!(
                    "EvmIssuerMint: no EVM contract registered for asset {}",
                    asset_id
                ))
            })?;

            // 6. Execute EVM issuerMint
            let amount_u256 = call_evm::U256::from(*amount);
            let mint_result = evm_executor
                .evm_call_issuer_mint(sender, contract_addr, evm_state, *to, amount_u256)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!(
                        "EvmIssuerMint: EVM call failed: {e:?}"
                    ))
                })?;

            if !mint_result.success {
                return Err(ConsensusError::InvalidBlock(
                    "EvmIssuerMint: EVM issuerMint reverted".into(),
                ));
            }

            // 7. Update evm_supply (cap already verified)
            registry
                .add_evm_supply(*asset_id, *amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("EvmIssuerMint: {e}")))?;

            Ok(InstructionResult::Success)
        }
        _ => Err(ConsensusError::InvalidBlock("not an asset instruction".into())),
    }
}

// ── Bridge instruction helpers ────────────────────────────────────────

fn is_bridge_instruction(instr: &Instruction) -> bool {
    matches!(instr, Instruction::ExternalBridgeDeposit { .. } | Instruction::ExternalBridgeWithdraw { .. } | Instruction::ChallengeBridgeDeposit { .. } | Instruction::BridgeDeposit { .. } | Instruction::BridgeToEvm { .. } | Instruction::BridgeToProtocol { .. })
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
    registry: &mut call_protocol::registry::AssetRegistry,
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
            let asset = registry.get_asset(*asset_id).ok_or_else(|| {
                ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: asset {} not registered",
                    asset_id
                ))
            })?;
            if asset.status != AssetStatus::Active {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToEvm: asset {} is not active (status: {:?})",
                    asset_id, asset.status
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
                        let _ = registry.add_evm_supply(*asset_id, *amount);
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
        Instruction::BridgeToProtocol {
            asset_id,
            to,
            amount,
        } => {
            // 1. Reject virtual USD (asset_id == 0)
            if *asset_id == 0 {
                return Err(ConsensusError::InvalidBlock(
                    "BridgeToProtocol: asset 0 (USD) is not bridgeable".into(),
                ));
            }

            // 2. Validate asset is registered
            let asset = registry.get_asset(*asset_id).ok_or_else(|| {
                ConsensusError::InvalidBlock(format!(
                    "BridgeToProtocol: asset {} not registered",
                    asset_id
                ))
            })?;
            if asset.status != AssetStatus::Active {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToProtocol: asset {} is not active (status: {:?})",
                    asset_id, asset.status
                )));
            }

            // 3. Check bridge not paused
            if bridge_state.is_paused(*asset_id) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "BridgeToProtocol: bridge paused for asset {}",
                    asset_id
                )));
            }

            // 4. Check per-tx limit
            bridge_state
                .check_per_tx_limit(*amount, config.max_per_tx)
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e}")))?;

            // 5. Check daily limit
            bridge_state
                .check_and_update_daily_limit(
                    *asset_id,
                    *amount,
                    config.daily_limit_per_asset,
                    current_block_height,
                    config.blocks_per_day,
                )
                .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e}")))?;

            let amount_u256 = call_evm::U256::from(*amount);

            // 6. Withdraw from EVM
            let exec_result = if *asset_id == call_protocol::CALL_ASSET_ID {
                // CALL: transfer from native EVM balance
                let evm_balance = evm_state.get_balance(&sender);
                if evm_balance < amount_u256 {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "BridgeToProtocol: insufficient EVM native balance for CALL: have {}, need {}",
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
                        "BridgeToProtocol: no EVM contract registered for asset {}",
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
                    .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e:?}")))
            };

            match exec_result {
                Ok(execution) => {
                    if execution.success {
                        // 7. Credit protocol balance
                        account
                            .credit_balance(*asset_id, *to, *amount)
                            .map_err(|e| ConsensusError::InvalidBlock(format!("BridgeToProtocol: {e}")))?;
                        let _ = registry.sub_evm_supply(*asset_id, *amount);
                        bridge_state.record_withdrawal(*asset_id, *amount);
                        Ok(InstructionResult::Success)
                    } else {
                        Err(ConsensusError::InvalidBlock(
                            "BridgeToProtocol: EVM operation reverted".into(),
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
            state_root: Hash::ZERO,
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
            state_root: Hash::ZERO,
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
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
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
                sender,
                call_primitives::U256::ZERO,
                call_primitives::U256::from(1u64),
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
                &mut ExecutionState::new(
                    &mut account, &mut registry, &mut compliance,
                    &mut bridge_state, &mut shielded_state, &mut evm_state,
                ),
                &mut BlockContext {
                    current_block_height: 1,
                    fee_params: &mut fee_params,
                    bridge_config: Some(&bridge_config),
                    validators: None,
                },
                &mut Subsystems::none(),
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
                &mut ExecutionState::new(
                    &mut account, &mut registry, &mut compliance,
                    &mut bridge_state, &mut shielded_state, &mut evm_state,
                ),
                &mut BlockContext {
                    current_block_height: 1,
                    fee_params: &mut fee_params,
                    bridge_config: Some(&bridge_config),
                    validators: None,
                },
                &mut Subsystems::none(),
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
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();
        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();
        let bridge_config = call_bridge::BridgeConfig::default();

        let result = block
            .execute(
                &mut ExecutionState::new(
                    &mut account, &mut registry, &mut compliance,
                    &mut bridge_state, &mut shielded_state, &mut evm_state,
                ),
                &mut BlockContext {
                    current_block_height: 50, // current_block_height < expires_at
                    fee_params: &mut fee_params,
                    bridge_config: Some(&bridge_config),
                    validators: None,
                },
                &mut Subsystems {
                    agent_balances: Some(&mut agent_balances),
                    agent_registry: Some(&mut agent_registry),
                    ..Subsystems::none()
                },
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
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state,
                            &mut evm_state,
                        ),
            &mut BlockContext {
                            current_block_height: 51,
                            fee_params: &mut fee_params,
                            bridge_config: Some(&bridge_config),
                            validators: None,
                        },
            &mut Subsystems::none(),
        );

        assert!(result.is_err());
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("expired"), "expected expiry error, got: {err_msg}");
    }

    #[test]
    fn test_frozen_asset_rejects_bridge_to_evm() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::BridgeToEvm {
                asset_id: call_protocol::CALL_ASSET_ID,
                to: test_addr(2),
                amount: 500,
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
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();
        // Freeze CALL asset
        registry.freeze_asset(call_protocol::CALL_ASSET_ID, &sender).unwrap();

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();
        let bridge_config = call_bridge::BridgeConfig::default();

        let validators: Vec<Address> = vec![sender];
        let result = block.execute(
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                        ),
            &mut BlockContext {
                            current_block_height: 1,
                            fee_params: &mut fee_params,
                            bridge_config: Some(&bridge_config),
                            validators: Some(&validators),
                        },
            &mut Subsystems::none(),
        );

        assert!(result.is_err(), "BridgeToEvm on frozen asset should fail");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("not active"), "expected not-active error, got: {err_msg}");
    }

    #[test]
    fn test_delisted_asset_rejects_bridge_to_protocol() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::BridgeToProtocol {
                asset_id: call_protocol::CALL_ASSET_ID,
                to: test_addr(2),
                amount: 500,
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
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();
        // Delist CALL asset
        registry.delist_asset(call_protocol::CALL_ASSET_ID, &sender).unwrap();

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(sender, call_primitives::U256::from(100_000_000_000u128));
        let bridge_config = call_bridge::BridgeConfig::default();

        let validators: Vec<Address> = vec![sender];
        let result = block.execute(
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                        ),
            &mut BlockContext {
                            current_block_height: 1,
                            fee_params: &mut fee_params,
                            bridge_config: Some(&bridge_config),
                            validators: Some(&validators),
                        },
            &mut Subsystems::none(),
        );

        assert!(result.is_err(), "BridgeToProtocol on delisted asset should fail");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("not active"), "expected not-active error, got: {err_msg}");
    }

    #[test]
    fn test_frozen_asset_rejects_bridge_op_deposit() {
        let sender = test_addr(1);
        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            TEST_VERSION,
            vec![],
            vec![],
            vec![],
            vec![BridgeOp::DepositToEvm {
                asset_id: call_protocol::CALL_ASSET_ID,
                from: sender,
                to: test_addr(2),
                amount: 500,
            }],
        );

        let mut account = AccountState::new();
        account.balances.set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000).unwrap();
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();
        registry.freeze_asset(call_protocol::CALL_ASSET_ID, &sender).unwrap();

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();
        let bridge_config = call_bridge::BridgeConfig::default();

        let result = block.execute(
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                        ),
            &mut BlockContext {
                            current_block_height: 1,
                            fee_params: &mut fee_params,
                            bridge_config: Some(&bridge_config),
                            validators: None,
                        },
            &mut Subsystems::none(),
        );

        assert!(result.is_err(), "BridgeOp DepositToEvm on frozen asset should fail");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("not active"), "expected not-active error, got: {err_msg}");
    }

    #[test]
    fn test_evm_issuer_mint_success() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);
        let recipient = test_addr(2);

        // Deploy ERC-20 contract first
        let executor = EvmExecutor::new(1);
        let mut evm_state = call_evm::EvmState::new();
        let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
        evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
        evm_state.create_account(deployer);
        evm_state.create_account(sender);
        evm_state.set_balance(sender, call_primitives::U256::from(10_000_000u128));

        let (contract_addr, deploy_result) = executor
            .deploy_erc20_template(
                deployer,
                &mut evm_state,
                "Test Token",
                "TST",
                18,
                deployer,
                sender,
                call_primitives::U256::from(10_000),
                call_primitives::U256::from(2u64),
            )
            .unwrap();
        assert!(deploy_result.success, "ERC-20 deploy failed");

        // Register asset with max_supply cap
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();
        registry
            .register_asset("TST".into(), "Test Token".into(), 18, sender, 0, 0, 10_000)
            .unwrap();
        registry.set_evm_contract_address(2, contract_addr);

        // Build EvmIssuerMint instruction
        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::EvmIssuerMint {
                asset_id: 2,
                to: recipient,
                amount: 5_000,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 200_000,
            max_fee: 1_000_000,
            expires_at: 0,
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
        account
            .balances
            .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
            .unwrap();

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();

        let result = block.execute(
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                        ),
            &mut BlockContext::new(1, &mut fee_params),
            &mut Subsystems::none(),
        );

        assert!(result.is_ok(), "EvmIssuerMint should succeed: {:?}", result);

        // evm_supply should be updated
        let asset = registry.get_asset(2).unwrap();
        assert_eq!(asset.evm_supply, 5_000);
        assert_eq!(asset.protocol_supply, 0);
        assert_eq!(asset.all_supply(), 5_000);

        // EVM state reflects the mint (contract storage updated, verified by success + supply tracking)
    }

    #[test]
    fn test_evm_issuer_mint_cap_enforcement() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        // Deploy ERC-20 with small cap
        let executor = EvmExecutor::new(1);
        let mut evm_state = call_evm::EvmState::new();
        let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
        evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
        evm_state.create_account(deployer);

        let (contract_addr, deploy_result) = executor
            .deploy_erc20_template(
                deployer,
                &mut evm_state,
                "Capped",
                "CAP",
                18,
                deployer,
                sender,
                call_primitives::U256::from(1_000),
                call_primitives::U256::from(2u64),
            )
            .unwrap();
        assert!(deploy_result.success);

        let mut registry = AssetRegistry::new();
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();
        registry
            .register_asset("CAP".into(), "Capped".into(), 18, sender, 0, 0, 1_000)
            .unwrap();
        registry.set_evm_contract_address(2, contract_addr);

        // Try to mint more than cap
        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::EvmIssuerMint {
                asset_id: 2,
                to: test_addr(2),
                amount: 1_001,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 200_000,
            max_fee: 1_000_000,
            expires_at: 0,
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
        account
            .balances
            .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
            .unwrap();

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();

        let result = block.execute(
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                        ),
            &mut BlockContext::new(1, &mut fee_params),
            &mut Subsystems::none(),
        );

        assert!(result.is_err(), "EvmIssuerMint over cap should fail");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("cap exceeded"), "expected cap error, got: {err_msg}");
    }

    #[test]
    fn test_evm_issuer_mint_non_issuer_rejected() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);
        let issuer = test_addr(1); // different from sender

        let executor = EvmExecutor::new(1);
        let mut evm_state = call_evm::EvmState::new();
        let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
        evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
        evm_state.create_account(deployer);

        let (contract_addr, deploy_result) = executor
            .deploy_erc20_template(
                deployer,
                &mut evm_state,
                "Test",
                "TST",
                18,
                deployer,
                issuer,
                call_primitives::U256::from(0),
                call_primitives::U256::from(2u64),
            )
            .unwrap();
        assert!(deploy_result.success);

        let mut registry = AssetRegistry::new();
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();
        registry
            .register_asset("TST".into(), "Test".into(), 18, issuer, 0, 0, 0)
            .unwrap();
        registry.set_evm_contract_address(2, contract_addr);

        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::EvmIssuerMint {
                asset_id: 2,
                to: test_addr(2),
                amount: 500,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 200_000,
            max_fee: 1_000_000,
            expires_at: 0,
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
        account
            .balances
            .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
            .unwrap();

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();

        let result = block.execute(
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                        ),
            &mut BlockContext::new(1, &mut fee_params),
            &mut Subsystems::none(),
        );

        assert!(result.is_err(), "non-issuer EvmIssuerMint should fail");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("not asset issuer"), "expected unauthorized error, got: {err_msg}");
    }

    #[test]
    fn test_evm_issuer_mint_frozen_asset_rejected() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        let executor = EvmExecutor::new(1);
        let mut evm_state = call_evm::EvmState::new();
        let deployer = call_protocol::BRIDGE_EVM_ADDRESS;
        evm_state.set_balance(deployer, call_primitives::U256::from(100_000_000_000u128));
        evm_state.create_account(deployer);

        let (contract_addr, deploy_result) = executor
            .deploy_erc20_template(
                deployer,
                &mut evm_state,
                "Test",
                "TST",
                18,
                deployer,
                sender,
                call_primitives::U256::from(0),
                call_primitives::U256::from(2u64),
            )
            .unwrap();
        assert!(deploy_result.success);

        let mut registry = AssetRegistry::new();
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();
        registry
            .register_asset("TST".into(), "Test".into(), 18, sender, 0, 0, 0)
            .unwrap();
        registry.set_evm_contract_address(2, contract_addr);
        registry.freeze_asset(2, &sender).unwrap();

        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::EvmIssuerMint {
                asset_id: 2,
                to: test_addr(2),
                amount: 500,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 200_000,
            max_fee: 1_000_000,
            expires_at: 0,
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
        account
            .balances
            .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
            .unwrap();

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();

        let result = block.execute(
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                        ),
            &mut BlockContext::new(1, &mut fee_params),
            &mut Subsystems::none(),
        );

        assert!(result.is_err(), "EvmIssuerMint on frozen asset should fail");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("not active"), "expected not-active error, got: {err_msg}");
    }

    #[test]
    fn test_evm_issuer_mint_call_asset_rejected() {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);

        let mut tx = ProtocolTransaction {
            sender,
            nonce: 0,
            instructions: vec![Instruction::EvmIssuerMint {
                asset_id: call_protocol::CALL_ASSET_ID,
                to: test_addr(2),
                amount: 500,
            }],
            gas_config: GasConfig::SelfPay,
            fee_currency: call_primitives::FeeCurrency::Call,
            gas_limit: 200_000,
            max_fee: 1_000_000,
            expires_at: 0,
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
        account
            .balances
            .set_balance(call_protocol::CALL_ASSET_ID, sender, 1_000_000_000)
            .unwrap();
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("CALL".into(), "Callchain".into(), 18, sender, 0, 0, 0)
            .unwrap();

        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();

        let result = block.execute(
            &mut ExecutionState::new(
                            &mut account, &mut registry, &mut compliance, &mut bridge_state, &mut shielded_state, &mut evm_state,
                        ),
            &mut BlockContext::new(1, &mut fee_params),
            &mut Subsystems::none(),
        );

        assert!(result.is_err(), "EvmIssuerMint on CALL asset should fail");
        let err_msg = format!("{:?}", result.unwrap_err());
        assert!(err_msg.contains("CALL asset has no EVM wrapped token"), "expected CALL rejection, got: {err_msg}");
    }
}
