//! Callchain precompiles for the EVM layer (per spec §25.4)
//!
//! Precompiles expose protocol functionality to EVM contracts and EOAs:
//! - `0x101` Oracle: getPrice, getTWAP, isStale, submitPrice
//! - `0x103` Bridge: getTotalDeposits, getTotalWithdrawals, externalBridgeDeposit, externalBridgeWithdraw, challengeBridgeDeposit
//! - `0x201` Asset: getBalance, getAssetInfo, transfer, batchTransfer, approve, transferFrom, mint, burn, register
//! - `0x202` Shielded: deposit, withdraw, transfer
//! - `0x203` Governance: submitProposal, vote, queue, execute, emergencyPause, emergencyResume
//! - `0x204` Validator: stake, unstake, claimUnbonded
//! - `0x205` Compliance: updateCompliance, checkCompliance
//! - `0x207` Switch: switchToEvm, switchToProtocol
//! - `0x209` Agent: register, grant, revoke

pub mod storage;
pub mod utils;
pub mod journal_backend;
pub mod dispatch;

pub use utils::*;

/// Oracle precompile address (0x101).
pub const ORACLE_ADDRESS: Address = address!("0000000000000000000000000000000000000101");

/// Bridge precompile address (0x103).
pub const BRIDGE_ADDRESS: Address = address!("0000000000000000000000000000000000000103");

/// Governance precompile address (0x203).
pub const GOVERNANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000203");

/// Shielded precompile address (0x202).
pub const SHIELDED_ADDRESS: Address = address!("0000000000000000000000000000000000000202");

/// Validator precompile address (0x204).
pub const VALIDATOR_ADDRESS: Address = address!("0000000000000000000000000000000000000204");

/// Compliance precompile address (0x205).
pub const COMPLIANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000205");

/// Switch precompile address (0x207).
pub const SWITCH_ADDRESS: Address = address!("0000000000000000000000000000000000000207");

/// Agent precompile address (0x209).
pub const AGENT_ADDRESS: Address = address!("0000000000000000000000000000000000000209");

// Re-export types needed by external precompile implementations
pub use revm_precompile::{PrecompileError, PrecompileOutput, PrecompileResult};
pub use alloy_primitives::Bytes;

use alloy_primitives::{address, Address};
use revm::context::Block;
use revm::context_interface::cfg::Cfg;
use revm::context_interface::local::LocalContextTr;
use revm_precompile::PrecompileSpecId;
use std::collections::HashMap;

use crate::storage::{EvmStorageProvider, StorageCtx};

// ── StatefulPrecompile trait ──────────────────────────────────────────

/// Trait implemented by all Callchain custom precompiles.
pub trait StatefulPrecompile {
    /// Dispatch an EVM call to this precompile.
    ///
    /// `calldata` is ABI-encoded (4-byte selector + args).
    /// `msg_sender` is the EVM caller.
    fn call(&mut self, calldata: &[u8], msg_sender: Address) -> PrecompileResult;
}

// ── Precompile addresses ──────────────────────────────────────────────
//
// Defined in their respective submodules and re-exported via `pub use` above.

/// Register all precompile addresses
pub fn all_precompiles() -> &'static [Address] {
    static PRECOMPILES: [Address; 9] = [
        ORACLE_ADDRESS,
        BRIDGE_ADDRESS,
        ASSET_ADDRESS,
        SHIELDED_ADDRESS,
        GOVERNANCE_ADDRESS,
        VALIDATOR_ADDRESS,
        COMPLIANCE_ADDRESS,
        SWITCH_ADDRESS,
        AGENT_ADDRESS,
    ];
    &PRECOMPILES
}

// ── CallPrecompiles ───────────────────────────────────────────────────

/// Callchain precompile provider implementing revm's `PrecompileProvider` trait.
///
/// Wraps standard Ethereum precompiles with Callchain custom precompiles.
/// Custom precompiles are executed through revm's normal call-frame mechanism
/// with [`EvmStorageProvider`] giving them access to the live journal.
pub struct CallPrecompiles {
    standard: revm_precompile::Precompiles,
    custom: HashMap<Address, Box<dyn StatefulPrecompile>>,
    spec: revm::primitives::hardfork::SpecId,
}

impl CallPrecompiles {
    /// Create a new CallPrecompiles for the given spec.
    ///
    /// Custom precompiles must be registered via [`with_custom`].
    pub fn new(spec: revm::primitives::hardfork::SpecId) -> Self {
        Self {
            standard: build_standard_precompiles(spec),
            custom: HashMap::new(),
            spec,
        }
    }

    /// Register a custom stateful precompile at the given address.
    pub fn with_custom(
        mut self,
        address: Address,
        precompile: Box<dyn StatefulPrecompile>,
    ) -> Self {
        self.custom.insert(address, precompile);
        self
    }
}

fn build_standard_precompiles(
    spec: revm::primitives::hardfork::SpecId,
) -> revm_precompile::Precompiles {
    revm_precompile::Precompiles::new(PrecompileSpecId::from_spec_id(spec)).clone()
}

impl<CTX: revm::context::ContextTr> revm::handler::PrecompileProvider<CTX> for CallPrecompiles {
    type Output = revm::interpreter::InterpreterResult;

    fn set_spec(&mut self, spec: <CTX::Cfg as revm::context::Cfg>::Spec) -> bool {
        let spec: revm::primitives::hardfork::SpecId = spec.into();
        if spec == self.spec {
            return false;
        }
        self.standard = build_standard_precompiles(spec);
        self.spec = spec;
        true
    }

    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &revm::interpreter::CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        let address = inputs.bytecode_address;
        // Try custom precompiles first
        if let Some(precompile) = self.custom.get_mut(&address) {
            let mut result = revm::interpreter::InterpreterResult {
                result: revm::interpreter::InstructionResult::Return,
                gas: revm::interpreter::Gas::new(inputs.gas_limit),
                output: revm::primitives::Bytes::new(),
            };

            // Extract calldata
            let input_bytes = match &inputs.input {
                revm::interpreter::CallInput::SharedBuffer(range) => context
                    .local()
                    .shared_memory_buffer_slice(range.clone())
                    .map(|s| s.to_vec())
                    .unwrap_or_default(),
                revm::interpreter::CallInput::Bytes(bytes) => bytes.0.to_vec(),
            };

            // Create storage provider from revm journal
            // Read block/cfg before journal_mut to avoid borrow conflict
            let timestamp = context.block().timestamp();
            let number = context.block().number().to::<u64>();
            let beneficiary = context.block().beneficiary();
            let chain_id = context.cfg().chain_id();
            let journal = context.journal_mut();

            let mut provider = EvmStorageProvider::new(
                journal,
                inputs.gas_limit, // enforce precompile gas limit so revm never underflows
                inputs.is_static,
                chain_id,
                timestamp,
                number,
                beneficiary,
            );

            let exec_result = StorageCtx::enter(&mut provider, || {
                precompile.call(&input_bytes, inputs.caller)
            });

            match exec_result {
                Ok(output) => {
                    result.gas.record_refund(output.gas_refunded);
                    let underflow = result.gas.record_cost(output.gas_used);
                    assert!(underflow, "Gas underflow is not possible");
                    result.result = if output.reverted {
                        revm::interpreter::InstructionResult::Revert
                    } else {
                        revm::interpreter::InstructionResult::Return
                    };
                    result.output = output.bytes;
                }
                Err(revm_precompile::PrecompileError::Fatal(e)) => return Err(e),
                Err(e) => {
                    result.result = if e.is_oog() {
                        revm::interpreter::InstructionResult::PrecompileOOG
                    } else {
                        revm::interpreter::InstructionResult::PrecompileError
                    };
                    if !e.is_oog() {
                        context.local_mut().set_precompile_error_context(e.to_string());
                    }
                }
            }
            Ok(Some(result))
        } else if let Some(precompile) = self.standard.get(&address) {
            // Standard Ethereum precompile
            let mut result = revm::interpreter::InterpreterResult {
                result: revm::interpreter::InstructionResult::Return,
                gas: revm::interpreter::Gas::new(inputs.gas_limit),
                output: revm::primitives::Bytes::new(),
            };

            let input_bytes = match &inputs.input {
                revm::interpreter::CallInput::SharedBuffer(range) => context
                    .local()
                    .shared_memory_buffer_slice(range.clone())
                    .map(|s| s.to_vec())
                    .unwrap_or_default(),
                revm::interpreter::CallInput::Bytes(bytes) => bytes.0.to_vec(),
            };

            let exec_result = precompile.execute(&input_bytes, inputs.gas_limit);

            match exec_result {
                Ok(output) => {
                    result.gas.record_refund(output.gas_refunded);
                    let underflow = result.gas.record_cost(output.gas_used);
                    assert!(underflow, "Gas underflow is not possible");
                    result.result = if output.reverted {
                        revm::interpreter::InstructionResult::Revert
                    } else {
                        revm::interpreter::InstructionResult::Return
                    };
                    result.output = output.bytes;
                }
                Err(revm_precompile::PrecompileError::Fatal(e)) => return Err(e),
                Err(e) => {
                    result.result = if e.is_oog() {
                        revm::interpreter::InstructionResult::PrecompileOOG
                    } else {
                        revm::interpreter::InstructionResult::PrecompileError
                    };
                    if !e.is_oog() {
                        context.local_mut().set_precompile_error_context(e.to_string());
                    }
                }
            }
            Ok(Some(result))
        } else {
            Ok(None)
        }
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = revm::primitives::Address>> {
        Box::new(
            self.standard
                .addresses()
                .cloned()
                .chain(self.custom.keys().cloned()),
        )
    }

    fn contains(&self, address: &revm::primitives::Address) -> bool {
        self.standard.contains(address) || self.custom.contains_key(address)
    }
}

impl CallPrecompiles {
    /// Total number of precompiles (standard + custom).
    pub fn len(&self) -> usize {
        self.standard.len() + self.custom.len()
    }

    /// Whether this address is a known precompile (standard or custom).
    pub fn contains(&self, address: &revm::primitives::Address) -> bool {
        self.standard.contains(address) || self.custom.contains_key(address)
    }
}

/// Build the full Callchain precompiles set (standard + custom).
///
/// Note: AssetPrecompile (0x201), BridgePrecompile (0x103),
/// OraclePrecompile (0x101), GovernancePrecompile (0x203),
/// ValidatorPrecompile (0x204), CompliancePrecompile (0x205),
/// ShieldedPrecompile (0x202), AgentPrecompile (0x209), and
/// SwitchPrecompile (0x207) are registered externally by `call-evm`
/// since they live in their own domain crates.
pub fn build_precompiles() -> CallPrecompiles {
    CallPrecompiles::new(revm::primitives::hardfork::SpecId::CANCUN)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_precompile_addresses() {
        let precompiles = all_precompiles();
        assert_eq!(precompiles.len(), 9);
        assert_eq!(precompiles[0], ORACLE_ADDRESS);
        assert_eq!(precompiles[1], BRIDGE_ADDRESS);
        assert_eq!(precompiles[2], ASSET_ADDRESS);
        assert_eq!(precompiles[3], SHIELDED_ADDRESS);
        assert_eq!(precompiles[4], GOVERNANCE_ADDRESS);
        assert_eq!(precompiles[5], VALIDATOR_ADDRESS);
        assert_eq!(precompiles[6], COMPLIANCE_ADDRESS);
        assert_eq!(precompiles[7], SWITCH_ADDRESS);
        assert_eq!(precompiles[8], AGENT_ADDRESS);
    }

    #[test]
    fn test_build_precompiles_contains_custom() {
        let precompiles = build_precompiles();
        assert!(precompiles.contains(&Address::left_padding_from(&[1])));
        // AssetPrecompile, BridgePrecompile, OraclePrecompile, GovernancePrecompile,
        // ShieldedPrecompile, ValidatorPrecompile, CompliancePrecompile,
        // AgentPrecompile, and SwitchPrecompile live in domain crates
        assert_eq!(precompiles.len(), 10);
    }
}
