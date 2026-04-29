//! Shielded precompile at 0x202 — stub
//!
//! The full shielded precompile implementation is pending migration to
//! the stateful pattern. Shielded instructions are currently handled
//! by the protocol instruction executor (crates/protocol/src/instructions/exec.rs).
//! This stub returns "not yet implemented" for all EVM calls.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult};

#[allow(dead_code)]
pub(crate) const SHIELDED_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000202");

/// Stub shielded precompile entry point.
pub fn shielded_precompile_fn(_input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    Err(PrecompileError::Other(
        "shielded precompile not yet implemented".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_shielded_address() {
        assert_eq!(
            SHIELDED_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000202")
        );
    }
}
