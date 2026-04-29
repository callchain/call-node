//! Agent precompile at 0x209 — stub
//!
//! The full agent precompile implementation is pending migration to
//! the stateful pattern. This stub returns "not yet implemented" for
//! all calls.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult};

#[allow(dead_code)]
pub(crate) const AGENT_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000209");

/// Stub agent precompile entry point.
///
/// Returns an error so callers get immediate feedback that the
/// agent precompile is not yet implemented.
pub fn agent_precompile_fn(_input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 10000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    Err(PrecompileError::Other(
        "agent precompile not yet implemented".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_address() {
        assert_eq!(
            AGENT_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000209")
        );
    }
}
