//! Validator precompile at 0x204
//!
//! Staking operations: stake, unstake, claimUnbonded.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult};

#[allow(dead_code)]
pub(crate) const VALIDATOR_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000204");

/// Validator precompile entry point
pub fn validator_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 20000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    // TODO: Implement selector dispatch
    // stake(bytes32,uint256) -> uint32
    // unstake(uint32)
    // claimUnbonded(uint32)

    Err(PrecompileError::Other("not yet implemented".into()))
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
