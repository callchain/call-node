//! Governance precompile at 0x203 — stub
//!
//! The full governance precompile implementation is pending migration to
//! the stateful pattern. Governance instructions are currently handled
//! by the protocol instruction executor (crates/protocol/src/instructions/exec.rs).
//! This stub returns "not yet implemented" for all EVM calls.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult};

#[allow(dead_code)]
pub(crate) const GOVERNANCE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000203");

/// Stub governance precompile entry point.
pub fn governance_precompile_fn(_input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    Err(PrecompileError::Other(
        "governance precompile not yet implemented".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_governance_address() {
        assert_eq!(
            GOVERNANCE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000203")
        );
    }
}
