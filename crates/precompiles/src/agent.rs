//! Agent precompile at 0x209
//!
//! Agent registry operations: registerAgent, grantAgentBalance, revokeAgentBalance.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult};

#[allow(dead_code)]
pub(crate) const AGENT_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000209");

/// Agent precompile entry point
pub fn agent_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 10000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    // TODO: Implement selector dispatch
    // registerAgent(bytes,string,string) -> uint64
    // grantAgentBalance(uint64,uint64,uint256)
    // revokeAgentBalance(uint64,uint64)

    Err(PrecompileError::Other("not yet implemented".into()))
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
