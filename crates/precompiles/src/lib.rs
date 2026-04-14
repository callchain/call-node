//! Callchain precompiles for the EVM layer (per spec §25.4)
//!
//! - Oracle precompile at `0x101`: getPrice, getTWAP, isStale, getOracleStatus
//! - Protocol balance precompile at `0x102`
//! - Bridge precompile at `0x103`

mod oracle;
mod balance;
mod bridge;

pub use oracle::*;
pub use balance::*;
pub use bridge::*;

use alloy_primitives::{address, Address};
use revm_precompile::{Precompile, PrecompileId, PrecompileResult, PrecompileError};

/// Precompile addresses
pub const ORACLE_ADDRESS: Address = address!("0000000000000000000000000000000000000101");
pub const BALANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000102");
pub const BRIDGE_ADDRESS: Address = address!("0000000000000000000000000000000000000103");

/// Register all precompile addresses
pub fn all_precompiles() -> &'static [Address] {
    static PRECOMPILES: [Address; 3] = [ORACLE_ADDRESS, BALANCE_ADDRESS, BRIDGE_ADDRESS];
    &PRECOMPILES
}

/// Build the full precompiles set: standard Ethereum precompiles + Callchain custom precompiles
pub fn build_precompiles() -> revm_precompile::Precompiles {
    use revm_precompile::PrecompileSpecId;

    // Start with standard Cancun precompiles
    let mut precompiles = revm_precompile::Precompiles::new(
        PrecompileSpecId::CANCUN,
    ).clone();

    // Extend with Callchain custom precompiles
    precompiles.extend([
        Precompile::new(
            PrecompileId::Custom("call_oracle".into()),
            ORACLE_ADDRESS,
            oracle_precompile_fn,
        ),
        Precompile::new(
            PrecompileId::Custom("call_balance".into()),
            BALANCE_ADDRESS,
            balance_precompile_fn,
        ),
        Precompile::new(
            PrecompileId::Custom("call_bridge".into()),
            BRIDGE_ADDRESS,
            bridge_precompile_fn,
        ),
    ]);

    precompiles
}

/// Oracle precompile entry point
///
/// Input ABI encoding: selector (4 bytes) + args
/// - getPrice(assetId) -> returns price (uint128)
/// - getTWAP(assetId, period) -> returns twap (uint128)
/// - isStale(assetId) -> returns bool
/// - getOracleStatus(assetId) -> returns status (uint8)
fn oracle_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 1000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let selector = &input[..4];
    let _state = OracleState::default();

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
            if let Some(price) = _state.get_price(asset_id) {
                output[16..].copy_from_slice(&price.price.to_be_bytes());
            }
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

/// Protocol balance precompile entry point
///
/// Input: selector (4) + asset_id (32) + address (32)
/// Output: balance (32 bytes, uint256)
fn balance_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 800;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    if input.len() < 68 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let _state = ProtocolBalanceState::default();
    let asset_id = u64::from_be_bytes({
        let mut buf = [0u8; 8];
        buf.copy_from_slice(&input[24..32]);
        buf
    });
    let addr = Address::from_slice(&input[44..64]);
    let balance = _state.get_balance(asset_id, &addr);

    let mut output = [0u8; 32];
    output[16..].copy_from_slice(&balance.to_be_bytes());

    Ok(revm_precompile::PrecompileOutput {
        bytes: alloy_primitives::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

/// Bridge precompile entry point
///
/// Input: selector (4) + args
/// Output: depends on function called
fn bridge_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 1500;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }

    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let _state = BridgeState::default();
    let mut output = [0u8; 32];

    match &input[..4] {
        // getTotalDeposits() -> uint256
        &[0xa8, 0x7e, 0x4f, 0x2a] => {
            output[16..].copy_from_slice(&_state.total_deposits.to_be_bytes());
        }
        // getTotalWithdrawals() -> uint256
        &[0x9c, 0x3e, 0x6d, 0x1b] => {
            output[16..].copy_from_slice(&_state.total_withdrawals.to_be_bytes());
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

    #[test]
    fn test_precompile_addresses() {
        let precompiles = all_precompiles();
        assert_eq!(precompiles.len(), 3);
        assert_eq!(precompiles[0], ORACLE_ADDRESS);
        assert_eq!(precompiles[1], BALANCE_ADDRESS);
        assert_eq!(precompiles[2], BRIDGE_ADDRESS);
    }

    #[test]
    fn test_build_precompiles_contains_custom() {
        let precompiles = build_precompiles();
        // Should contain standard Cancun precompiles (ecrecover at 0x01)
        assert!(precompiles.contains(&Address::left_padding_from(&[1])));
        // Should contain our custom precompiles
        assert!(precompiles.contains(&ORACLE_ADDRESS));
        assert!(precompiles.contains(&BALANCE_ADDRESS));
        assert!(precompiles.contains(&BRIDGE_ADDRESS));
        // Cancun has 10 precompiles + our 3 = 13
        assert_eq!(precompiles.len(), 13);
    }

    #[test]
    fn test_oracle_precompile_out_of_gas() {
        let result = oracle_precompile_fn(&[0x76, 0x3e, 0x4d, 0x8c], 100);
        assert!(matches!(result, Err(PrecompileError::OutOfGas)));
    }

    #[test]
    fn test_oracle_precompile_invalid_input() {
        let result = oracle_precompile_fn(&[], 10000);
        assert!(matches!(result, Err(PrecompileError::Other(_))));
    }

    #[test]
    fn test_balance_precompile_out_of_gas() {
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x00; 4]);
        let result = balance_precompile_fn(&input, 100);
        assert!(matches!(result, Err(PrecompileError::OutOfGas)));
    }

    #[test]
    fn test_bridge_precompile_unknown_selector() {
        let result = bridge_precompile_fn(&[0xff, 0xff, 0xff, 0xff], 10000);
        assert!(matches!(result, Err(PrecompileError::Other(_))));
    }
}
