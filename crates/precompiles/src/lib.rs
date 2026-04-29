//! Callchain precompiles for the EVM layer (per spec §25.4)
//!
//! Precompiles expose protocol functionality to EVM contracts and EOAs:
//! - `0x101` Oracle: getPrice, getTWAP, isStale, submitPrice
//! - `0x103` Bridge: getTotalDeposits, getTotalWithdrawals, externalBridgeDeposit, externalBridgeWithdraw, challengeBridgeDeposit
//! - `0x201` Asset: getBalance, getAssetInfo, transfer, batchTransfer, approve, transferFrom, mint, burn, register
//! - `0x202` Shielded: shieldedDeposit, shieldedWithdraw, shieldedTransfer
//! - `0x203` Governance: submitProposal, vote, queue, execute, emergencyPause, emergencyResume
//! - `0x204` Validator: stake, unstake, claimUnbonded
//! - `0x205` Compliance: updateCompliance, checkCompliance
//! - `0x207` Switch: switchToEvm, switchToProtocol
//! - `0x209` Agent: registerAgent, grantAgentBalance, revokeAgentBalance
//!
mod oracle;
mod bridge;
mod asset;
mod switch;
mod shielded;
mod governance;
mod validator;
mod compliance;
mod agent;
pub mod state_hook;

pub use oracle::*;
pub use bridge::*;
pub use asset::*;
pub use switch::*;
pub use shielded::*;
pub use governance::*;
pub use validator::*;
pub use compliance::*;
pub use agent::*;

// Re-export types needed by external precompile implementations
pub use revm_precompile::{PrecompileError, PrecompileOutput, PrecompileResult};
pub use alloy_primitives::Bytes;

use alloy_primitives::{address, Address, U256};
use revm::context_interface::local::LocalContextTr;
use revm_precompile::{Precompile, PrecompileId};
use std::sync::OnceLock;

// ── External precompile registration ──────────────────────────────────
///
/// Crates that own protocol state (call-consensus, call-agent, call-bridge)
/// can register their precompile implementations here to avoid cyclic
/// dependencies. If no implementation is registered, the built-in stub
/// is used.

static VALIDATOR_PRECOMPILE_FN: OnceLock<fn(&[u8], u64) -> PrecompileResult> = OnceLock::new();
static AGENT_PRECOMPILE_FN: OnceLock<fn(&[u8], u64) -> PrecompileResult> = OnceLock::new();
static BRIDGE_EXT_PRECOMPILE_FN: OnceLock<fn(&[u8], u64) -> PrecompileResult> = OnceLock::new();

/// Register the validator precompile implementation from call-consensus.
pub fn set_validator_precompile_fn(f: fn(&[u8], u64) -> PrecompileResult) {
    let _ = VALIDATOR_PRECOMPILE_FN.set(f);
}

/// Register the agent precompile implementation from call-agent.
pub fn set_agent_precompile_fn(f: fn(&[u8], u64) -> PrecompileResult) {
    let _ = AGENT_PRECOMPILE_FN.set(f);
}

/// Register the bridge extension precompile implementation from call-bridge.
pub fn set_bridge_ext_precompile_fn(f: fn(&[u8], u64) -> PrecompileResult) {
    let _ = BRIDGE_EXT_PRECOMPILE_FN.set(f);
}

// ── Thread-local call context for write precompiles ───────────────────

thread_local! {
    static CURRENT_CALLER: std::cell::Cell<Option<Address>> = std::cell::Cell::new(None);
    static CURRENT_CALL_VALUE: std::cell::Cell<U256> = std::cell::Cell::new(U256::ZERO);
}

/// Get the caller address of the current precompile invocation (if any).
/// Write precompiles use this to identify the transaction sender.
pub fn current_caller() -> Option<Address> {
    CURRENT_CALLER.with(|c| c.get())
}

/// Get the call value (ETH sent) of the current precompile invocation.
pub fn current_call_value() -> U256 {
    CURRENT_CALL_VALUE.with(|c| c.get())
}

/// Set the caller address for testing or manual invocation.
pub fn set_current_caller(addr: Option<Address>) {
    CURRENT_CALLER.with(|c| c.set(addr));
}

/// Set the call value for testing or manual invocation.
pub fn set_current_call_value(value: U256) {
    CURRENT_CALL_VALUE.with(|c| c.set(value));
}

/// Precompile addresses
pub const ORACLE_ADDRESS: Address = address!("0000000000000000000000000000000000000101");
pub const BRIDGE_ADDRESS: Address = address!("0000000000000000000000000000000000000103");
pub const ASSET_ADDRESS: Address = address!("0000000000000000000000000000000000000201");
pub const SHIELDED_ADDRESS: Address = address!("0000000000000000000000000000000000000202");
pub const GOVERNANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000203");
pub const VALIDATOR_ADDRESS: Address = address!("0000000000000000000000000000000000000204");
pub const COMPLIANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000205");
pub const SWITCH_ADDRESS: Address = address!("0000000000000000000000000000000000000207");
pub const AGENT_ADDRESS: Address = address!("0000000000000000000000000000000000000209");

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

/// Callchain precompile provider implementing revm's `PrecompileProvider` trait.
///
/// Wraps standard Ethereum precompiles with Callchain custom precompiles.
/// All custom precompiles are executed through revm's normal call-frame
/// mechanism with proper gas accounting, state isolation, and call depth tracking.
pub struct CallPrecompiles {
    precompiles: revm_precompile::Precompiles,
    spec: revm::primitives::hardfork::SpecId,
}

impl CallPrecompiles {
    /// Create a new CallPrecompiles for the given spec.
    pub fn new(spec: revm::primitives::hardfork::SpecId) -> Self {
        Self {
            precompiles: build_precompiles_for_spec(spec),
            spec,
        }
    }
}

fn build_precompiles_for_spec(
    spec: revm::primitives::hardfork::SpecId,
) -> revm_precompile::Precompiles {
    use revm_precompile::PrecompileSpecId;

    let mut precompiles =
        revm_precompile::Precompiles::new(PrecompileSpecId::from_spec_id(spec)).clone();

    precompiles.extend([
        Precompile::new(
            PrecompileId::Custom("call_oracle".into()),
            ORACLE_ADDRESS,
            oracle_precompile_fn,
        ),
        Precompile::new(
            PrecompileId::Custom("call_bridge".into()),
            BRIDGE_ADDRESS,
            BRIDGE_EXT_PRECOMPILE_FN.get().copied().unwrap_or(bridge_precompile_fn),
        ),
        Precompile::new(
            PrecompileId::Custom("call_asset".into()),
            ASSET_ADDRESS,
            asset_precompile_fn,
        ),
        Precompile::new(
            PrecompileId::Custom("call_shielded".into()),
            SHIELDED_ADDRESS,
            shielded_precompile_fn,
        ),
        Precompile::new(
            PrecompileId::Custom("call_governance".into()),
            GOVERNANCE_ADDRESS,
            governance_precompile_fn,
        ),
        Precompile::new(
            PrecompileId::Custom("call_validator".into()),
            VALIDATOR_ADDRESS,
            VALIDATOR_PRECOMPILE_FN.get().copied().unwrap_or(validator_precompile_fn),
        ),
        Precompile::new(
            PrecompileId::Custom("call_compliance".into()),
            COMPLIANCE_ADDRESS,
            compliance_precompile_fn,
        ),
        Precompile::new(
            PrecompileId::Custom("call_switch".into()),
            SWITCH_ADDRESS,
            switch_precompile_fn,
        ),
        Precompile::new(
            PrecompileId::Custom("call_agent".into()),
            AGENT_ADDRESS,
            AGENT_PRECOMPILE_FN.get().copied().unwrap_or(agent_precompile_fn),
        ),
    ]);

    precompiles
}

impl<CTX: revm::context::ContextTr> revm::handler::PrecompileProvider<CTX>
    for CallPrecompiles
{
    type Output = revm::interpreter::InterpreterResult;

    fn set_spec(&mut self, spec: <CTX::Cfg as revm::context::Cfg>::Spec) -> bool {
        let spec: revm::primitives::hardfork::SpecId = spec.into();
        if spec == self.spec {
            return false;
        }
        self.precompiles = build_precompiles_for_spec(spec);
        self.spec = spec;
        true
    }

    fn run(
        &mut self,
        context: &mut CTX,
        inputs: &revm::interpreter::CallInputs,
    ) -> Result<Option<Self::Output>, String> {
        let Some(precompile) = self.precompiles.get(&inputs.bytecode_address) else {
            return Ok(None);
        };

        let mut result = revm::interpreter::InterpreterResult {
            result: revm::interpreter::InstructionResult::Return,
            gas: revm::interpreter::Gas::new(inputs.gas_limit),
            output: revm::primitives::Bytes::new(),
        };

        // Inject call context for write precompiles
        CURRENT_CALLER.with(|c| c.set(Some(inputs.caller)));
        CURRENT_CALL_VALUE.with(|c| c.set(inputs.call_value()));

        let exec_result = {
            let r;
            let input_bytes = match &inputs.input {
                revm::interpreter::CallInput::SharedBuffer(range) => {
                    if let Some(slice) =
                        context.local().shared_memory_buffer_slice(range.clone())
                    {
                        r = slice;
                        r.as_ref()
                    } else {
                        &[]
                    }
                }
                revm::interpreter::CallInput::Bytes(bytes) => bytes.0.iter().as_slice(),
            };
            precompile.execute(input_bytes, inputs.gas_limit)
        };

        // Clear call context to prevent leakage
        CURRENT_CALLER.with(|c| c.set(None));
        CURRENT_CALL_VALUE.with(|c| c.set(U256::ZERO));

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
    }

    fn warm_addresses(&self) -> Box<impl Iterator<Item = revm::primitives::Address>> {
        Box::new(self.precompiles.addresses().cloned())
    }

    fn contains(&self, address: &revm::primitives::Address) -> bool {
        self.precompiles.contains(address)
    }
}

/// Build the full precompiles set: standard Ethereum precompiles + Callchain custom precompiles
pub fn build_precompiles() -> revm_precompile::Precompiles {
    build_precompiles_for_spec(revm::primitives::hardfork::SpecId::CANCUN)
}

/// Oracle precompile entry point
///
/// Input ABI encoding: selector (4 bytes) + args
/// - getPrice(assetId) -> returns price (uint128)
/// - getTWAP(assetId, period) -> returns twap (uint128)
/// - isStale(assetId) -> returns bool
/// - getOracleStatus(assetId) -> returns status (uint8)
pub fn oracle_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let selector = &input[..4];

    // Write operation: submitPrice needs write lock and higher gas
    match selector {
        &[0x7a, 0xe9, 0x19, 0xf7] => return oracle_submit_price(input, gas_limit),
        _ => {}
    }

    // Read operations
    const GAS_COST: u64 = 1000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    let Some(oracle_guard) = get_live_oracle() else {
        return Err(PrecompileError::Other("oracle not initialized".into()));
    };
    let oracle: std::sync::RwLockReadGuard<_> =
        oracle_guard.read().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    let mut output = [0u8; 32];
    match selector {
        // getPrice(uint64 assetId) -> uint128 price
        &[0x76, 0x3e, 0x4d, 0x8c] => {
            let asset_id = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                if input.len() >= 36 {
                    buf.copy_from_slice(&input[28..36]);
                }
                buf
            });
            if let Some(price) = oracle.get_price_by_asset(asset_id) {
                output[16..].copy_from_slice(&price.median_price.to_be_bytes());
            }
        }
        // getTWAP(uint64 assetId, uint64 currentTimestamp) -> uint128 twap
        &[0xab, 0xcd, 0xef, 0x01] => {
            let asset_id = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                if input.len() >= 36 {
                    buf.copy_from_slice(&input[28..36]);
                }
                buf
            });
            let current_ts = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                if input.len() >= 68 {
                    buf.copy_from_slice(&input[60..68]);
                }
                buf
            });
            if let Some(twap) = oracle.get_twap_by_asset(asset_id, current_ts) {
                let twap_bytes: [u8; 16] = twap.to_be_bytes();
                output[16..].copy_from_slice(&twap_bytes);
            }
        }
        // isStale(uint64 assetId, uint64 currentTimestamp) -> bool
        &[0x12, 0x34, 0x56, 0x78] => {
            let asset_id = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                if input.len() >= 36 {
                    buf.copy_from_slice(&input[28..36]);
                }
                buf
            });
            let current_ts = u64::from_be_bytes({
                let mut buf = [0u8; 8];
                if input.len() >= 68 {
                    buf.copy_from_slice(&input[60..68]);
                }
                buf
            });
                        output[31] = if oracle.is_stale_by_asset(asset_id, current_ts) { 1 } else { 0 };
        }
        _ => return Err(PrecompileError::Other("unknown selector".into())),
    }

    Ok(revm_precompile::PrecompileOutput {
        bytes: alloy_primitives::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

/// submitPrice(uint64 assetId, uint256 price, uint64 timestamp, uint64 blockNumber)
/// Selector: 0x7ae919f7
fn oracle_submit_price(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 5000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 132 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = u64::from_be_bytes({
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&input[28..36]);
        buf
    });
    let price = u128::from_be_bytes({
        let mut buf = [0u8; 16];
        buf.copy_from_slice(&input[48..64]);
        buf
    });
    let timestamp = u64::from_be_bytes({
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&input[92..100]);
        buf
    });
    let block_number = u64::from_be_bytes({
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&input[124..132]);
        buf
    });

    let Some(oracle_guard) = get_live_oracle() else {
        return Err(PrecompileError::Other("oracle not initialized".into()));
    };
    let mut oracle: std::sync::RwLockWriteGuard<_> =
        oracle_guard.write().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    oracle.record_direct_price_by_asset(asset_id, price, timestamp, block_number);

    Ok(revm_precompile::PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

/// Bridge precompile entry point
///
/// Input: selector (4) + args
/// Output: depends on function called
pub fn bridge_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 1500;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let Some(bridge_guard) = get_live_bridge() else {
        return Err(PrecompileError::Other("bridge state not initialized".into()));
    };
    let bridge_state: std::sync::RwLockReadGuard<_> =
        bridge_guard.read().map_err(|_| PrecompileError::Other("lock poisoned".into()))?;

    let mut output = [0u8; 32];

    match input[..4] {
        // getTotalDeposits() -> uint256
        [0xa8, 0x7e, 0x4f, 0x2a] => {
            output[16..].copy_from_slice(&bridge_state.total_deposits.to_be_bytes());
        }
        // getTotalWithdrawals() -> uint256
        [0x9c, 0x3e, 0x6d, 0x1b] => {
            output[16..].copy_from_slice(&bridge_state.total_withdrawals.to_be_bytes());
        }
        _ => return Err(PrecompileError::Other("unknown selector".into())),
    }

    Ok(revm_precompile::PrecompileOutput {
        bytes: alloy_primitives::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_oracle::{OracleConfig, OracleManager};
    use std::sync::{Arc, RwLock};

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
        assert!(precompiles.contains(&ORACLE_ADDRESS));
        assert!(precompiles.contains(&BRIDGE_ADDRESS));
        assert!(precompiles.contains(&ASSET_ADDRESS));
        assert!(precompiles.contains(&SHIELDED_ADDRESS));
        assert!(precompiles.contains(&GOVERNANCE_ADDRESS));
        assert!(precompiles.contains(&VALIDATOR_ADDRESS));
        assert!(precompiles.contains(&COMPLIANCE_ADDRESS));
        assert!(precompiles.contains(&SWITCH_ADDRESS));
        assert!(precompiles.contains(&AGENT_ADDRESS));
        assert_eq!(precompiles.len(), 19);
    }

    #[test]
    fn test_oracle_precompile_out_of_gas() {
        let manager = OracleManager::new(OracleConfig::default());
        set_live_oracle(Arc::new(RwLock::new(manager)));
        let result = oracle_precompile_fn(&[0x76, 0x3e, 0x4d, 0x8c], 100);
        assert!(matches!(result, Err(PrecompileError::OutOfGas)));
    }

    #[test]
    fn test_oracle_precompile_unknown_selector() {
        let manager = OracleManager::new(OracleConfig::default());
        set_live_oracle(Arc::new(RwLock::new(manager)));
        let result = oracle_precompile_fn(&[0xff, 0xff, 0xff, 0xff], 10000);
        assert!(matches!(result, Err(PrecompileError::Other(_))));
    }

    #[test]
    fn test_oracle_precompile_submit_price() {
        let manager = OracleManager::new(OracleConfig::default());
        set_live_oracle(Arc::new(RwLock::new(manager)));

        // Encode: submitPrice(assetId=1, price=2_000_000, timestamp=1000, blockNumber=100)
        let mut input = vec![0u8; 132];
        input[0..4].copy_from_slice(&[0x7a, 0xe9, 0x19, 0xf7]);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..64].copy_from_slice(&2_000_000u128.to_be_bytes());
        input[92..100].copy_from_slice(&1000u64.to_be_bytes());
        input[124..132].copy_from_slice(&100u64.to_be_bytes());

        let result = oracle_precompile_fn(&input, 10000);
        assert!(result.is_ok(), "submitPrice failed: {:?}", result);

        // Verify price was recorded
        let result = oracle_precompile_fn(&[0x76, 0x3e, 0x4d, 0x8c], 10000);
        // getPrice needs asset_id argument
        let mut get_price_input = vec![0u8; 36];
        get_price_input[0..4].copy_from_slice(&[0x76, 0x3e, 0x4d, 0x8c]);
        get_price_input[28..36].copy_from_slice(&1u64.to_be_bytes());
        let result = oracle_precompile_fn(&get_price_input, 10000);
        assert!(result.is_ok(), "getPrice failed: {:?}", result);
        let output = result.unwrap().bytes;
        let price = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&output[16..32]);
            buf
        });
        assert_eq!(price, 2_000_000);
    }

    #[test]
    fn test_oracle_precompile_submit_price_out_of_gas() {
        let manager = OracleManager::new(OracleConfig::default());
        set_live_oracle(Arc::new(RwLock::new(manager)));

        let mut input = vec![0u8; 132];
        input[0..4].copy_from_slice(&[0x7a, 0xe9, 0x19, 0xf7]);
        let result = oracle_precompile_fn(&input, 1000);
        assert!(matches!(result, Err(PrecompileError::OutOfGas)));
    }

    #[test]
    fn test_bridge_precompile_unknown_selector() {
        set_live_bridge(Arc::new(RwLock::new(BridgeState::default())));
        let result = bridge_precompile_fn(&[0xff, 0xff, 0xff, 0xff], 10000);
        assert!(matches!(result, Err(PrecompileError::Other(_))));
    }
}
