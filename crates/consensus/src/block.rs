//! T6.1 — Block Structure (per spec §2.4, §2.5)
//!
//! Block and BlockHeader types with hash, validation, and execution.

use call_bridge::BridgeConfig;
use call_crypto::keccak256;
use call_primitives::{Address, Balance, BlockHash, Hash, ProtocolVersion, TxHash};
use call_protocol::FeeParams;
use call_shielded::ShieldedState;
use call_evm::{EvmExecutor, EvmState, EvmTransaction, BlockGasTracker};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;

use crate::validator::ConsensusError;
use crate::ForkManager;
use crate::exec::evm_instructions;

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

// ── EVM Transaction ───────────────────────────────────────────────────

/// EVM transaction in raw serialized form (RLP-encoded).
/// Deserialized into `EvmTransaction` during execution.
pub type EvmTx = Vec<u8>;

// ── Execution Parameter Grouping ──────────────────────────────────────

/// Core mutable protocol state passed to every block execution.
pub struct ExecutionState<'a> {
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
    pub fork_manager: Option<&'a mut ForkManager>,
}

impl<'a> ExecutionState<'a> {
    pub fn new(
        shielded_state: &'a mut ShieldedState,
        evm_state: &'a mut EvmState,
    ) -> Self {
        Self { shielded_state, evm_state }
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
            fork_manager: None,
        }
    }
}

// ── Block ─────────────────────────────────────────────────────────────

/// Full block structure (per spec §2.4)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Block {
    pub header: BlockHeader,
    pub evm_txs: Vec<EvmTx>,
}

impl Block {
    /// Build a new block
    pub fn new(
        height: u64,
        parent_hash: BlockHash,
        timestamp_millis: u64,
        proposer: call_primitives::ValidatorId,
        version: ProtocolVersion,
        evm_txs: Vec<EvmTx>,
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
            evm_txs,
        }
    }

    /// Validate block structure and header
    pub fn validate(
        &self,
        expected_parent: BlockHash,
        fork_manager: &ForkManager,
    ) -> Result<(), ConsensusError> {
        self.header.validate(expected_parent, fork_manager)?;
        Ok(())
    }

    /// Execute all transactions in spec order (per spec §2.5):
    /// 1. EVM transactions (evm_txs)
    /// 2. System settlement (base-fee update, oracle reward)
    ///
    /// Returns EVM execution results.
    pub fn execute(
        &self,
        state: &mut ExecutionState,
        ctx: &mut BlockContext,
        _subsystems: &mut Subsystems,
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
            match executor.execute_tx(tx, state.evm_state, ctx.current_block_height) {
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

        // Step 2: System settlement (base-fee update, oracle reward pool)
        let total_gas = result.evm_gas_used;
        update_base_fee_after_block(ctx.fee_params, total_gas);

        let total_fees = total_gas as u128 * ctx.fee_params.base_fee;
        let oracle_share = total_fees * ctx.fee_params.oracle_fee_share_bps as u128 / 10_000;
        evm_instructions::add_oracle_reward(state.evm_state, oracle_share);

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
    pub state_root: Hash,
    pub evm_tx_count: usize,
    pub total_validator_reward: Balance,
    /// Total gas used by EVM transactions
    pub evm_gas_used: u64,
    /// Per-EVM-tx results (for receipt generation)
    pub evm_tx_results: Vec<EvmTxResult>,
}

impl BlockExecutionResult {
    /// Total transactions processed
    pub fn total_tx_count(&self) -> usize {
        self.evm_tx_count
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
    for evm in &result.evm_tx_results {
        let mut leaf = Vec::new();
        leaf.extend_from_slice(evm.tx_hash.as_slice());
        leaf.push(if evm.status { 1 } else { 0 });
        leaf.extend_from_slice(&evm.gas_used.to_be_bytes());
        leaf.extend_from_slice(&evm.gas_price.to_be_bytes());
        data.extend_from_slice(keccak256(&leaf).as_slice());
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

