//! T6.1 — Block Structure (per spec §2.4, §2.5)
//!
//! Block and BlockHeader types with hash, validation, and execution.

use call_bridge::{BridgeOp, BridgeConfig};
use call_crypto::keccak256;
use call_primitives::{Address, Balance, BlockHash, Hash, ProtocolVersion, TxHash};
use call_primitives::ExecutionStatus;
use call_protocol::account::AccountState;
use call_protocol::instructions::{Instruction, InstructionResult};
use call_protocol::registry::AssetRegistry;
use call_protocol::transaction::ProtocolTransaction;
use call_protocol::FeeParams;
use call_shielded::ShieldedState;
use call_evm::{EvmExecutor, EvmState, EvmTransaction, BlockGasTracker};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::validator::ConsensusError;
use crate::{ForkManager, RollbackPlan};
use crate::exec::{evm_instructions, rollback};

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
    pub smart_accounts: Option<&'a call_protocol::smart_accounts::SmartAccountRegistry>,
    pub fork_manager: Option<&'a mut ForkManager>,
}

impl<'a> ExecutionState<'a> {
    pub fn new(
        account: &'a mut AccountState,
        registry: &'a mut AssetRegistry,
        compliance: &'a mut call_protocol::compliance::ComplianceEngine,
        shielded_state: &'a mut ShieldedState,
        evm_state: &'a mut EvmState,
    ) -> Self {
        Self { account, registry, compliance, shielded_state, evm_state }
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
            smart_accounts: None,
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
            let tx = match decode_evm_tx(raw_tx) {
                Ok(t) => t,
                Err(()) => {
                    tracing::warn!("block: decode_evm_tx failed, skipping tx");
                    continue;
                }
            };
            let caller = tx.caller;
            let nonce = tx.nonce;

            // Check for duplicate nonce within this block
            if !used_evm_nonces.insert((caller, nonce)) {
                tracing::warn!(?caller, nonce, "block: duplicate evm nonce in same block, skipping");
                continue;
            }

            // Validate nonce and balance before execution
            if let Err(e) = call_evm::validate_evm_tx(&tx, state.evm_state) {
                let evm_nonce = state.evm_state.get_nonce(&caller);
                let evm_bal = state.evm_state.get_balance(&caller);
                tracing::warn!(?caller, tx_nonce = nonce, evm_nonce, ?evm_bal, error = ?e, "block: validate_evm_tx failed, skipping");
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
                    evm_instructions::read_balance(state.evm_state, call_protocol::CALL_ASSET_ID, caller);
                if protocol_balance < needed {
                    tracing::warn!(?caller, needed, protocol_balance, evm_balance = evm_balance_u128, "block: insufficient unified gas, skipping");
                    continue;
                }
                let new_protocol_bal = protocol_balance
                    .checked_sub(needed)
                    .expect("checked above");
                evm_instructions::seed_balance(
                    state.evm_state,
                    call_protocol::CALL_ASSET_ID,
                    caller,
                    new_protocol_bal,
                );
                let new_evm = state.evm_state.get_balance(&caller)
                    + call_evm::U256::from(needed);
                state.evm_state.set_balance(caller, new_evm);
            }

            let tx_to = tx.to;
            let tx_gas_price = tx.gas_price;
            match executor.execute_tx(tx, state.evm_state) {
                Ok(exec_result) => {
                    let new_evm_nonce = state.evm_state.get_nonce(&caller);
                    tracing::info!(?caller, tx_nonce = nonce, new_evm_nonce, gas_used = exec_result.gas_used, success = exec_result.success, "block: evm tx executed");
                    if gas_tracker.add_gas(exec_result.gas_used).is_err() {
                        tracing::warn!(?caller, gas_used = exec_result.gas_used, "block: block gas limit exceeded, skipping");
                        continue;
                    }
                    result.evm_tx_count += 1;
                    result.evm_gas_used += exec_result.gas_used;

                    // Compute contract address for CREATE transactions
                    let contract_address = if tx_to.is_none() {
                        Some(call_evm::derive_create_address(caller, nonce))
                    } else {
                        None
                    };

                    // Convert revm logs to protocol LogEntry
                    let logs: Vec<call_protocol::LogEntry> = exec_result
                        .logs
                        .iter()
                        .map(|log| call_protocol::LogEntry {
                            address: log.address,
                            topics: log
                                .data
                                .topics()
                                .iter()
                                .map(|t| call_primitives::Hash::from(t.0))
                                .collect(),
                            data: log.data.data.to_vec(),
                        })
                        .collect();

                    let tx_hash = keccak256(raw_tx);
                    result.evm_tx_results.push(EvmTxResult {
                        tx_hash,
                        gas_used: exec_result.gas_used,
                        status: exec_result.success,
                        caller,
                        to: tx_to,
                        contract_address,
                        logs,
                        gas_price: tx_gas_price,
                    });
                }
                Err(e) => {
                    tracing::warn!(?caller, error = ?e, "block: executor.execute_tx failed, skipping");
                }
            }
            // Nonce consumed regardless of execution result (same as Ethereum)
        }

        // Step 2: Protocol transactions
        for tx in &self.protocol_txs {
            // Validate nonce against EVM state (nonce consumed on inclusion)
            if state.evm_state.get_nonce(&tx.sender) != tx.nonce {
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
            let priority_fee = tx.max_priority_fee.max(call_protocol::transaction::MIN_PRIORITY_FEE_PER_GAS);
            let fee = call_protocol::transaction::compute_fee(
                gas_units,
                priority_fee,
                ctx.fee_params.base_fee,
            )
            .min(tx.max_fee);

            // Check fee balance in EVM storage
            let fee_asset_id = match tx.fee_currency {
                call_primitives::FeeCurrency::Call => call_protocol::CALL_ASSET_ID,
                call_primitives::FeeCurrency::Stablecoin(asset_id) => asset_id,
            };
            let fee_balance = evm_instructions::read_balance(state.evm_state, fee_asset_id, tx.sender);
            if fee_balance < fee {
                continue; // insufficient fee balance
            }

            // Deduct gas fee from EVM storage
            let new_fee_balance = fee_balance
                .checked_sub(fee)
                .expect("fee balance checked above");
            evm_instructions::seed_balance(state.evm_state, fee_asset_id, tx.sender, new_fee_balance);

            // Increment nonce (consumed on inclusion, regardless of execution result)
            state.evm_state.increment_nonce(tx.sender);

            // Separate instructions by type: rollback, evm
            // Separate rollback instructions (stateless, need ForkManager)
            let (rollback_instrs, non_rollback): (Vec<_>, Vec<_>) = tx
                .instructions
                .iter()
                .cloned()
                .partition(|i| rollback::is_rollback_instruction(i));

            // Take snapshots for atomic rollback (nonce already consumed, not rolled back)
            let account_snapshot = state.account.clone();
            let evm_snapshot = state.evm_state.clone();
            let shielded_snapshot = state.shielded_state.clone();
            let mut tx_results = Vec::new();

            let exec_result = (|| -> Result<(), ConsensusError> {
                for instr in &non_rollback {
                    let r = evm_instructions::execute_instruction_on_evm(
                        instr,
                        tx.sender,
                        state.evm_state,
                        Some(state.compliance),
                        state.shielded_state,
                        ctx.current_block_height,
                        ctx.bridge_config,
                        ctx.validators,
                        Some(&executor),
                    )?;
                    tx_results.push(r);
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
                // Rollback account/EVM/shielded but nonce stays consumed.
                // Include the failed tx in the block with a Reverted result.
                *state.account = account_snapshot;
                *state.evm_state = evm_snapshot;
                *state.shielded_state = shielded_snapshot;
                result.protocol_tx_count += 1;
                result.protocol_priority_fees.push(priority_fee);
                result.transaction_results.push(TransactionResult {
                    tx_hash: tx.compute_tx_hash().into(),
                    status: ExecutionStatus::Reverted { reason: e.to_string() },
                    gas_used: gas_units,
                    fee_amount: fee,
                    instruction_count: 0,
                    agent_events: vec![],
                });
                tracing::warn!(error = %e, sender = ?tx.sender, nonce = tx.nonce, "block: tx execution failed, included as reverted");
            } else {
                result.protocol_tx_count += 1;
                result.protocol_priority_fees.push(priority_fee);
                result.transaction_results.push(TransactionResult {
                    tx_hash: tx.compute_tx_hash().into(),
                    status: ExecutionStatus::Success,
                    gas_used: gas_units,
                    fee_amount: fee,
                    instruction_count: tx_results.len(),
                    agent_events: vec![],
                });
            }
        }

        // Step 3: Bridge operations
        // Execute internal bridge deposits/withdrawals atomically via EVM storage.
        if let Some(config) = ctx.bridge_config {
            for op in &self.bridge_operations {
                let asset_id = op.asset_id();
                // Reject bridge ops on frozen or delisted assets (hard block failure)
                let status = evm_instructions::read_asset_status(state.evm_state, asset_id);
                if status != 0 {
                    return Err(ConsensusError::InvalidBlock(format!(
                        "bridge op: asset {asset_id} is not active (status: {status})"
                    )));
                }

                let exec_result = match op {
                    call_bridge::BridgeOp::DepositToEvm { .. } => {
                        evm_instructions::exec_bridge_op_deposit(
                            state.evm_state,
                            op,
                            ctx.current_block_height,
                            config,
                            Some(&executor),
                        )
                    }
                    call_bridge::BridgeOp::WithdrawToProtocol { .. } => {
                        evm_instructions::exec_bridge_op_withdraw(
                            state.evm_state,
                            op,
                            ctx.current_block_height,
                            config,
                            Some(&executor),
                        )
                    }
                };

                if exec_result.is_ok() {
                    result.bridge_op_count += 1;
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

        // Compute state root (EVM-only mode)
        result.state_root = compute_evm_state_root(state.evm_state);

        Ok(result)
    }

    /// Update header state_root after execution.
    /// In EVM-only mode state_root is exactly the EVM state root.
    pub fn finalize(&mut self, result: &BlockExecutionResult) {
        self.header.state_root = result.state_root;
    }
}

// ── Block Execution Result ────────────────────────────────────────────

/// Result of executing a single protocol transaction
#[derive(Debug, Clone)]
pub struct TransactionResult {
    pub tx_hash: TxHash,
    pub status: ExecutionStatus,
    pub gas_used: u64,
    pub fee_amount: u128,
    pub instruction_count: usize,
    pub agent_events: Vec<call_agent::AgentEvent>,
}

/// Result of executing a single EVM transaction
#[derive(Debug, Clone)]
pub struct EvmTxResult {
    pub tx_hash: TxHash,
    pub gas_used: u64,
    pub status: bool,
    pub caller: Address,
    pub to: Option<Address>,
    pub contract_address: Option<Address>,
    pub logs: Vec<call_protocol::LogEntry>,
    pub gas_price: u128,
}

/// Result of executing all transactions in a block
#[derive(Debug, Default, Clone)]
pub struct BlockExecutionResult {
    pub transaction_results: Vec<TransactionResult>,
    pub state_root: Hash,
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
    /// Priority fees paid by each protocol tx (for fee history percentile computation)
    pub protocol_priority_fees: Vec<u128>,
    /// Per-EVM-tx results (for receipt generation)
    pub evm_tx_results: Vec<EvmTxResult>,
}

impl BlockExecutionResult {
    /// Total transactions processed
    pub fn total_tx_count(&self) -> usize {
        self.evm_tx_count + self.protocol_tx_count + self.bridge_op_count + self.system_tx_count
    }

    /// Compute priority fee percentiles for fee history.
    ///
    /// Returns a Vec of priority fees at the requested percentiles.
    /// If no protocol transactions exist, returns a Vec filled with the minimum.
    pub fn priority_fee_percentiles(&self, percentiles: &[f64]) -> Vec<u128> {
        if self.protocol_priority_fees.is_empty() {
            return vec![call_protocol::transaction::MIN_PRIORITY_FEE_PER_GAS; percentiles.len()];
        }
        let mut sorted = self.protocol_priority_fees.clone();
        sorted.sort_unstable();
        let n = sorted.len();
        percentiles
            .iter()
            .map(|p| {
                let idx = ((n - 1) as f64 * p.min(100.0).max(0.0) / 100.0).round() as usize;
                sorted[idx.min(n - 1)]
            })
            .collect()
    }
}

// ── Helper Functions ──────────────────────────────────────────────────

/// Update base fee after block execution (delegates to protocol layer)
pub(crate) fn update_base_fee_after_block(params: &mut FeeParams, gas_used: u64) {
    call_protocol::transaction::update_base_fee(params, gas_used);
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
    for tr in &result.transaction_results {
        let mut leaf = Vec::new();
        leaf.extend_from_slice(tr.tx_hash.as_slice());
        leaf.push(if tr.status.is_success() { 1 } else { 0 });
        leaf.extend_from_slice(&tr.gas_used.to_be_bytes());
        leaf.extend_from_slice(&tr.fee_amount.to_be_bytes());
        data.extend_from_slice(keccak256(&leaf).as_slice());
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

