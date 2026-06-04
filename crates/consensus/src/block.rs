//! T6.1 — Block Structure (per spec §2.4, §2.5)
//!
//! Block and BlockHeader types with hash, validation, and execution.

use call_crypto::keccak256;
use call_primitives::{Address, Balance, BlockHash, Hash, ProtocolVersion, TxHash};
use call_protocol::gas::FeeParams;
use serde::{Deserialize, Serialize};

use crate::exec::state_accessors;
use crate::validator::ConsensusError;
use crate::ForkManager;

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

        Self { header, evm_txs }
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
    /// The execution pipeline is fully MDBX-native:
    ///   * `provider` is loaded from MDBX by the caller.
    ///   * EVM transactions run against a [`CacheDB`] backed by the provider.
    ///   * The revm delta is applied back to the provider.
    ///   * System settlement mutates the provider directly.
    ///   * The caller must persist the provider back to MDBX if desired.
    pub fn execute(
        &self,
        provider: &mut call_evm::provider::InMemoryStateProvider,
        fee_params: &mut FeeParams,
        current_block_height: u64,
        db_env: Option<std::sync::Arc<reth_db::DatabaseEnv>>,
    ) -> Result<BlockExecutionResult, ConsensusError> {
        // Use LazyStateProvider for execution if db_env is available,
        // avoiding the expensive full-state clone of InMemoryStateProvider.
        let (block_tx_result, revm_delta) = if let Some(ref db) = db_env {
            let lazy = call_evm::provider::LazyStateProvider::new(std::sync::Arc::clone(db));
            call_evm::block_executor::execute_block_transactions(
                &self.evm_txs,
                lazy,
                current_block_height,
                self.header.timestamp_millis,
                fee_params.base_fee,
            )
            .map_err(|e| ConsensusError::InvalidBlock(format!("evm execution failed: {e}")))?
        } else {
            let provider_for_exec = provider.clone();
            call_evm::block_executor::execute_block_transactions(
                &self.evm_txs,
                provider_for_exec,
                current_block_height,
                self.header.timestamp_millis,
                fee_params.base_fee,
            )
            .map_err(|e| ConsensusError::InvalidBlock(format!("evm execution failed: {e}")))?
        };

        // Apply revm delta back to the provider.
        provider.state_mut().apply_from_revm_state(&revm_delta);

        // Record historical state diffs for eth_getBalance(blockTag) / eth_getStorageAt(blockTag)
        if let Some(ref db) = db_env {
            let _ = call_evm::db::record_revm_delta_history(db, current_block_height, &revm_delta);
        }

        let mut result = BlockExecutionResult {
            evm_tx_count: block_tx_result.evm_tx_count,
            evm_gas_used: block_tx_result.evm_gas_used,
            evm_tx_results: block_tx_result
                .evm_tx_results
                .into_iter()
                .map(|e| EvmTxResult {
                    tx_hash: e.tx_hash,
                    gas_used: e.gas_used,
                    status: e.status,
                    caller: e.caller,
                    to: e.to,
                    contract_address: e.contract_address,
                    logs: e.logs,
                    gas_price: e.gas_price,
                })
                .collect(),
            ..Default::default()
        };

        // System settlement
        let total_gas = result.evm_gas_used;
        update_base_fee_after_block(fee_params, total_gas);
        let total_fees = total_gas as u128 * fee_params.base_fee;
        let oracle_share = total_fees * fee_params.oracle_fee_share_bps as u128 / 10_000;
        state_accessors::add_oracle_reward(provider.state_mut(), oracle_share);

        // Validator reward: distribute remaining fees to the proposer validator.
        // This is applied before state root computation so the state root
        // captures all consensus-driven state changes.
        let validator_share = total_fees * fee_params.validator_fee_share_bps as u128 / 10_000;
        if validator_share > 0 {
            let proposer_addr =
                state_accessors::read_validator_addr(provider.state(), self.header.proposer as u64);
            if proposer_addr != call_primitives::Address::ZERO {
                state_accessors::distribute_reward_evm(
                    provider.state_mut(),
                    proposer_addr,
                    validator_share,
                );
                result.total_validator_reward = validator_share;
            }
        }

        // Compute state root and collect trie updates for persistence
        let (root, updates) = provider.state().compute_state_root_with_updates();
        result.state_root = root;

        // Persist trie nodes to MDBX for eth_getProof
        if let Some(ref db) = db_env {
            let _ = call_evm::db::apply_trie_updates_to_mdbx(db, &updates);
        }

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
    call_protocol::gas::update_base_fee(params, gas_used);
}

// ── Property-based tests ──────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_block_header_serde_roundtrip(
            parent_hash in prop::array::uniform32(any::<u8>()),
            height in any::<u64>(),
            ts in any::<u64>(),
            state_root in prop::array::uniform32(any::<u8>()),
            prop_id in any::<u32>(),
            sig_bytes in prop::collection::vec(any::<u8>(), 65),
            major in any::<u16>(),
            minor in any::<u16>(),
            patch in any::<u16>(),
        ) {
            let header = BlockHeader {
                parent_hash: BlockHash::from(parent_hash),
                height,
                timestamp_millis: ts.saturating_add(1u64),
                state_root: Hash::from(state_root),
                proposer: prop_id.saturating_add(1u32),
                signature: {
                    let arr: [u8; 65] = sig_bytes.try_into().expect("invariant: 65-byte signature");
                    BlockSignature::from(arr)
                },
                version: ProtocolVersion::new(major, minor, patch),
                bls_aggregate_signature: None,
                bls_signer_bitmap: Vec::new(),
            };
            let json = serde_json::to_vec(&header).expect("serialize");
            let decoded: BlockHeader = serde_json::from_slice(&json).expect("deserialize");
            prop_assert_eq!(header.parent_hash, decoded.parent_hash);
            prop_assert_eq!(header.height, decoded.height);
            prop_assert_eq!(header.timestamp_millis, decoded.timestamp_millis);
            prop_assert_eq!(header.state_root, decoded.state_root);
            prop_assert_eq!(header.proposer, decoded.proposer);
            prop_assert_eq!(header.signature.0, decoded.signature.0);
            prop_assert_eq!(header.version, decoded.version);
        }

        #[test]
        fn prop_block_serde_roundtrip(
            parent_hash in prop::array::uniform32(any::<u8>()),
            height in any::<u64>(),
            timestamp_millis in any::<u64>(),
            proposer in any::<u32>(),
            version_major in any::<u16>(),
            version_minor in any::<u16>(),
            version_patch in any::<u16>(),
            tx_count in 0usize..10usize,
        ) {
            let version = ProtocolVersion::new(version_major, version_minor, version_patch);
            let block = Block::new(
                height,
                BlockHash::from(parent_hash),
                timestamp_millis.saturating_add(1),
                proposer.saturating_add(1),
                version,
                vec![vec![1, 2, 3]; tx_count],
            );
            let json = serde_json::to_vec(&block).expect("serialize");
            let decoded: Block = serde_json::from_slice(&json).expect("deserialize");
            prop_assert_eq!(block.header.height, decoded.header.height);
            prop_assert_eq!(block.header.parent_hash, decoded.header.parent_hash);
            prop_assert_eq!(block.evm_txs.len(), decoded.evm_txs.len());
        }
    }
}

// ── Consensus Error (re-exported for block validation) ────────────────
// Defined in validator.rs, re-exported via lib.rs
