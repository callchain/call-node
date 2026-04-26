//! T6.1 — Block Structure (per spec §2.4, §2.5)
//!
//! Block and BlockHeader types with hash, validation, and execution.

use call_bridge::{BridgeOp, BridgeConfig};
use call_crypto::{build_merkle_root, keccak256};
use call_governance::GovernanceManager;
use call_primitives::{Address, Balance, BlockHash, Hash, ProtocolVersion};
use call_protocol::account::AccountState;
use call_protocol::instructions::{execute_protocol_instructions, InstructionResult};
use call_protocol::registry::{AssetRegistry, AssetStatus};
use call_protocol::transaction::ProtocolTransaction;
use call_protocol::FeeParams;
use call_shielded::ShieldedState;
use call_evm::{EvmExecutor, EvmState, EvmTransaction, BlockGasTracker};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::validator::{ConsensusError, ValidatorStateManager};
use crate::{ForkManager, RollbackPlan};
use crate::exec::{agent, asset, bridge, validator, rollback};

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
                .partition(|i| bridge::is_bridge_instruction(i));
            let (agent_instrs, non_agent): (Vec<_>, Vec<_>) = non_bridge
                .iter()
                .cloned()
                .partition(|i| agent::is_agent_instruction(i));
            let (validator_instrs, non_validator): (Vec<_>, Vec<_>) = non_agent
                .iter()
                .cloned()
                .partition(|i| validator::is_validator_instruction(i));
            let (asset_instrs, non_asset): (Vec<_>, Vec<_>) = non_validator
                .iter()
                .cloned()
                .partition(|i| asset::is_asset_instruction(i));
            let (rollback_instrs, other_instrs): (Vec<_>, Vec<_>) = non_asset
                .iter()
                .cloned()
                .partition(|i| rollback::is_rollback_instruction(i));

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
                        let r = asset::execute_asset_instruction(
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
                        let r = bridge::execute_bridge_instruction(
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
                        let r = agent::execute_agent_instruction(
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
                        let r = validator::execute_validator_instruction(
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
                        let maybe_plan = rollback::execute_rollback_instruction(
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

// ── Block Execution Result ────────────────────────────────────────────

/// Result of executing all transactions in a block
#[derive(Debug, Default, Clone)]
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
pub(crate) fn update_base_fee_after_block(params: &mut FeeParams, gas_used: u64) {
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

    // Sort by asset_id to ensure deterministic ordering across nodes
    let mut deposits: Vec<_> = bridge_state.total_deposits.iter().collect();
    deposits.sort_by_key(|(asset_id, _)| *asset_id);
    for (asset_id, total) in deposits {
        data.extend_from_slice(&asset_id.to_le_bytes());
        data.extend_from_slice(&total.to_le_bytes());
    }

    let mut withdrawals: Vec<_> = bridge_state.total_withdrawals.iter().collect();
    withdrawals.sort_by_key(|(asset_id, _)| *asset_id);
    for (asset_id, total) in withdrawals {
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
pub(crate) fn compute_receipt_root(result: &BlockExecutionResult) -> Hash {
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

