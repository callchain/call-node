//! Bridge withdrawal: EVM balance → Protocol balance (per spec §5.3)
//!
//! Internal withdrawal logic has been moved to EVM-based execution.
//! All bridge state (limits, pauses, pending deposits) lives in EVM storage.

use crate::BridgeError;
use alloy_primitives::{Address, U256};

/// Check if there is sufficient EVM balance for a withdrawal.
pub fn check_withdraw_balance(
    from: Address,
    balance: U256,
    amount: u128,
) -> Result<(), BridgeError> {
    let amount_u256 = U256::from(amount);
    if balance < amount_u256 {
        Err(BridgeError::InsufficientEvmBalance(from, amount_u256))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_withdraw_insufficient_evm_balance() {
        assert!(check_withdraw_balance(test_addr(1), U256::from(100), 500).is_err());
        assert!(check_withdraw_balance(test_addr(1), U256::from(100), 100).is_ok());
    }
}
