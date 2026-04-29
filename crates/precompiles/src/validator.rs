//! Validator precompile at 0x204 — stub
//!
//! The full validator precompile implementation is pending migration to
//! the stateful pattern. This stub returns "not yet implemented" for
//! all calls.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult};

#[allow(dead_code)]
pub(crate) const VALIDATOR_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000204");

/// Stub validator precompile entry point.
///
/// Returns an error so callers get immediate feedback that the
/// validator precompile is not yet implemented.
pub fn validator_precompile_fn(_input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    Err(PrecompileError::Other(
        "validator precompile not yet implemented".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validator_address() {
        assert_eq!(
            VALIDATOR_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000204")
        );
    }
}
