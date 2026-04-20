//! T6.1 — Block Structure (per spec §2.4, §2.5)
//!
//! Block and BlockHeader types with hash, validation, and execution.

use call_bridge::{BridgeOp, BridgeConfig};
use call_crypto::{build_merkle_root, keccak256};
use call_governance::GovernanceManager;
use call_primitives::{Address, Balance, BlockHash, Hash, ProtocolVersion};
use call_protocol::balances::BalanceState;
use call_protocol::instructions::{execute_protocol_instructions, Instruction, InstructionResult};
use call_protocol::registry::AssetRegistry;
use call_protocol::transaction::ProtocolTransaction;
use call_protocol::FeeParams;
use call_shielded::ShieldedState;
use call_evm::{EvmExecutor, EvmState, EvmTransaction, BlockGasTracker};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::validator::ConsensusError;

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
        // Note: bls_aggregate_signature is intentionally excluded from the hash
        // because it is a consensus seal added after the block content is finalized.
        keccak256(&data)
    }

    /// Validate header fields
    pub fn validate(&self, expected_parent: BlockHash) -> Result<(), ConsensusError> {
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
    pub fn validate(&self, expected_parent: BlockHash) -> Result<(), ConsensusError> {
        self.header.validate(expected_parent)?;

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
        balances: &mut BalanceState,
        registry: &AssetRegistry,
        compliance: &mut call_protocol::compliance::ComplianceEngine,
        bridge_state: &mut call_bridge::BridgeStateManager,
        shielded_state: &mut ShieldedState,
        fee_params: &mut FeeParams,
        current_block_height: u64,
        evm_state: &mut EvmState,
        mut oracle: Option<&mut call_oracle::OracleManager>,
        mut agent_executor: Option<call_protocol::instructions::AgentExecutor>,
        mut agent_balances: Option<&mut call_agent::AgentBalances>,
        agent_registry: Option<&call_agent::AgentRegistry>,
        mut governance: Option<&mut GovernanceManager>,
        bridge_config: Option<&BridgeConfig>,
        validators: Option<&[Address]>,
        smart_accounts: Option<&call_protocol::smart_accounts::SmartAccountRegistry>,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        let mut result = BlockExecutionResult::default();
        let executor = EvmExecutor::new(1); // chain_id = 1
        let max_evm_gas = 30_000_000u64; // default block gas limit (~30M for Ethereum-compatible)
        let mut gas_tracker = BlockGasTracker::new(max_evm_gas);
        // Track nonces separately: EVM and protocol operate on separate namespaces
        let mut used_evm_nonces: HashSet<(call_primitives::Address, u64)> = HashSet::new();
        let mut used_protocol_nonces: HashSet<(call_primitives::Address, u64)> = HashSet::new();

        // Step 1: EVM transactions
        for raw_tx in &self.evm_txs {
            if let Ok(tx) = decode_evm_tx(raw_tx) {
                // Validate nonce and balance before execution
                if call_evm::validate_evm_tx(&tx, evm_state).is_err() {
                    // Skip invalid txs — they don't consume gas
                    continue;
                }
                // Check for duplicate nonce within this block
                let caller = tx.caller;
                let nonce = tx.nonce;
                if !used_evm_nonces.insert((caller, nonce)) {
                    continue; // duplicate nonce in same block
                }
                if let Ok(exec_result) = executor.execute_tx(tx, evm_state) {
                    if gas_tracker.add_gas(exec_result.gas_used).is_err() {
                        // Gas limit exceeded — skip this tx, release nonce
                        used_evm_nonces.remove(&(caller, nonce));
                        continue;
                    }
                    result.evm_tx_count += 1;
                    result.evm_gas_used += exec_result.gas_used;
                } else {
                    // Execution failed — release nonce so it can be retried
                    used_evm_nonces.remove(&(caller, nonce));
                }
            }
        }

        // Step 2: Protocol transactions
        for tx in &self.protocol_txs {
            // Reject duplicate nonce within the same block
            if !used_protocol_nonces.insert((tx.sender, tx.nonce)) {
                continue; // duplicate nonce
            }

            // Verify transaction signature before execution
            if let Err(e) = tx.verify_signature_with_registry(smart_accounts) {
                return Err(ConsensusError::InvalidBlock(format!(
                    "signature verification failed for tx from {:?}: {e}",
                    tx.sender
                )));
            }

            // Separate instructions by type: bridge, agent, regular
            let (bridge_instrs, non_bridge): (Vec<_>, Vec<_>) = tx
                .instructions
                .iter()
                .cloned()
                .partition(|i| is_bridge_instruction(i));
            let (agent_instrs, other_instrs): (Vec<_>, Vec<_>) = non_bridge
                .iter()
                .cloned()
                .partition(|i| is_agent_instruction(i));

            // Take snapshots for atomic rollback
            let balance_snapshot = balances.clone();
            let evm_snapshot = evm_state.clone();
            let bridge_snapshot = bridge_state.clone();
            let mut tx_results = Vec::new();

            let exec_result = (|| -> Result<(), ConsensusError> {
                // Execute regular instructions via protocol engine (includes governance)
                if !other_instrs.is_empty() {
                    let results = execute_protocol_instructions(
                        &other_instrs,
                        balances,
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
                            balances,
                            bridge_state,
                            config,
                            vals,
                            current_block_height,
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
                    let ar = agent_registry.ok_or_else(|| {
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
                            balances,
                            evm_state,
                            bridge_state,
                            registry,
                            &executor,
                        )?;
                        tx_results.push(r);
                    }
                }

                Ok(())
            })();

            if let Err(e) = exec_result {
                *balances = balance_snapshot;
                *evm_state = evm_snapshot;
                *bridge_state = bridge_snapshot;
                return Err(e);
            }

            result.protocol_tx_count += 1;
            result.instruction_results.extend(tx_results);
        }

        // Step 3: Bridge operations
        for op in &self.bridge_operations {
            bridge_state.add_pending_op(op.clone(), current_block_height);
            result.bridge_op_count += 1;
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
        result.payment_root = compute_payment_root(balances);
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
    )
}

fn execute_agent_instruction(
    instruction: &Instruction,
    sender: call_primitives::Address,
    agent_balances: &mut call_agent::AgentBalances,
    agent_registry: &call_agent::AgentRegistry,
    balances: &mut BalanceState,
    evm_state: &mut EvmState,
    bridge_state: &mut call_bridge::BridgeStateManager,
    registry: &AssetRegistry,
    evm_executor: &EvmExecutor,
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
            agent_balances
                .deduct(agent.owner, payment.agent_id, payment.asset_id, payment.amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("agent pay: {e}")))?;
            balances
                .credit_balance(payment.asset_id, payment.to, payment.amount)
                .map_err(|e| ConsensusError::InvalidBlock(format!("agent pay: {e}")))?;
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
                agent_balances
                    .deduct(agent.owner, payment.agent_id, payment.asset_id, payment.amount)
                    .map_err(|e| {
                        ConsensusError::InvalidBlock(format!("agent batch pay: {e}"))
                    })?;
                balances
                    .credit_balance(payment.asset_id, payment.to, payment.amount)
                    .map_err(|e| {
                        ConsensusError::InvalidBlock(format!("agent batch pay: {e}"))
                    })?;
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
                Ok(_) => Ok(InstructionResult::Success),
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
            agent_balances
                .deduct(agent.owner, *agent_id, *asset_id, *amount)
                .map_err(|e| {
                    ConsensusError::InvalidBlock(format!("agent bridge deposit: {e}"))
                })?;
            balances
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
            let bridge_address = call_primitives::Address::from_slice(&[0xCCu8; 20]);

            match call_bridge::execute_deposit(
                &op, balances, evm_state, evm_executor, bridge_state, &config, registry,
                bridge_address, sender,
            ) {
                Ok(exec) if exec.success => Ok(InstructionResult::Success),
                Ok(_) => {
                    let _ = agent_balances.credit(agent.owner, *agent_id, *asset_id, *amount);
                    let _ = balances.credit_balance(*asset_id, sender, *amount);
                    Err(ConsensusError::InvalidBlock(
                        "agent bridge deposit failed".into(),
                    ))
                }
                Err(e) => {
                    let _ = agent_balances.credit(agent.owner, *agent_id, *asset_id, *amount);
                    let _ = balances.credit_balance(*asset_id, sender, *amount);
                    Err(ConsensusError::InvalidBlock(format!(
                        "agent bridge deposit: {e:?}"
                    )))
                }
            }
        }
        _ => Err(ConsensusError::InvalidBlock("not an agent instruction".into())),
    }
}

// ── Bridge instruction helpers ────────────────────────────────────────

fn is_bridge_instruction(instr: &Instruction) -> bool {
    matches!(instr, Instruction::ExternalBridgeDeposit { .. } | Instruction::ChallengeBridgeDeposit { .. })
}

fn execute_bridge_instruction(
    instruction: &Instruction,
    _sender: call_primitives::Address,
    balances: &mut BalanceState,
    bridge_state: &mut call_bridge::BridgeStateManager,
    config: &call_bridge::BridgeConfig,
    validators: &[call_primitives::Address],
    current_block_height: u64,
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
                balances,
                bridge_state,
                config,
                validators,
                current_block_height,
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
            let revoked = bridge_state.revoke_pending_external_deposit(
                &call_primitives::B256::from(*source_tx_hash),
            );
            if revoked {
                Ok(InstructionResult::Success)
            } else {
                Err(ConsensusError::InvalidBlock(
                    "bridge challenge: no pending deposit found for source_tx_hash".into(),
                ))
            }
        }
        _ => Err(ConsensusError::InvalidBlock("not a bridge instruction".into())),
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
fn compute_payment_root(balances: &BalanceState) -> Hash {
    let mut leaves: Vec<Hash> = balances
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
    use call_primitives::Address;
    use call_protocol::instructions::Instruction;
    use call_protocol::transaction::{AuthScheme, GasConfig};

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

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
            nonce: 1,
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

    /// Create a protocol transaction with a valid secp256k1 signature.
    /// Returns the transaction and the sender address (derived from the signing key).
    fn make_signed_test_tx() -> (ProtocolTransaction, Address) {
        let (secret, pubkey) = call_crypto::generate_keypair();
        let sender = call_crypto::pubkey_to_address(&pubkey);
        let mut tx = ProtocolTransaction {
            sender,
            nonce: 1,
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
            bls_aggregate_signature: None,
            bls_signer_bitmap: Vec::new(),
        };

        assert!(header.validate(BlockHash::repeat_byte(1)).is_ok());
        assert!(header.validate(BlockHash::repeat_byte(2)).is_err());
    }

    #[test]
    fn test_block_validate() {
        let block = Block::new(
            1,
            BlockHash::repeat_byte(1),
            1000,
            1,
            vec![make_test_tx()],
            vec![],
            vec![],
            vec![],
        );

        assert!(block.validate(BlockHash::repeat_byte(1)).is_ok());
        assert!(block.validate(BlockHash::repeat_byte(2)).is_err());
    }

    #[test]
    fn test_block_validate_duplicate_nonce() {
        let tx = make_test_tx();
        let block = Block::new(
            1,
            BlockHash::ZERO,
            1000,
            1,
            vec![tx.clone(), tx],
            vec![],
            vec![],
            vec![],
        );

        let result = block.validate(BlockHash::ZERO);
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
                asset_id: 1,
                from: sender,
                to: test_addr(2),
                amount: 500,
            }],
        );

        // Setup balance state
        let mut balances = BalanceState::new();
        balances
            .balances
            .set_balance(1, sender, 10_000)
            .unwrap();
        let registry = AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();
        evm_state.set_balance(sender, call_primitives::U256::from(100_000_000_000_000u128));
        evm_state.set_balance(test_addr(1), call_primitives::U256::from(100_000_000_000_000u128));

        let result = block
            .execute(
                &mut balances,
                &registry,
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

        let mut balances = BalanceState::new();
        let registry = AssetRegistry::new();
        let mut compliance = call_protocol::compliance::ComplianceEngine::new();
        let mut bridge_state = call_bridge::BridgeStateManager::default();
        let mut shielded_state = call_shielded::ShieldedState::new();
        let mut fee_params = FeeParams::default();
        let mut evm_state = call_evm::EvmState::new();

        let result = block
            .execute(
                &mut balances,
                &registry,
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
            vec![],
            vec![],
            vec![],
            vec![],
        );

        assert_eq!(block.header.height, 42);
        assert_eq!(block.header.parent_hash, BlockHash::repeat_byte(0xFF));
        assert_eq!(block.header.timestamp_millis, 999_000);
        assert_eq!(block.header.proposer, 7);
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
}
