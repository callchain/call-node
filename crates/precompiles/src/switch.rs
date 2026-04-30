//! Switch precompile at 0x207
//!
//! Bidirectional bridge between protocol balance and EVM wrapped tokens:
//! - switchToEvm: protocol balance -> EVM ERC-20
//! - switchToProtocol: EVM ERC-20 -> protocol balance
//!
//! # Current Limitation
//! This precompile handles protocol-side state only (ASSET_ADDRESS balance slots).
//! The EVM-side (ERC-20 mint/burn) requires an EVM executor which is not available
//! in the precompile context. Future work: extend StorageProvider with balance
//! mutation or provide EVM executor access to precompiles.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult};

use crate::StatefulPrecompile;

#[allow(dead_code)]
pub(crate) const SWITCH_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000207");

// ── SwitchPrecompile ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct SwitchPrecompile;

impl SwitchPrecompile {
    // switchToEvm(uint64 assetId, address to, uint128 amount) -> 0x4311f613
    fn switch_to_evm(
        &self,
        _input: &[u8],
        _msg_sender: alloy_primitives::Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        Err(PrecompileError::Other(
            "switch precompile is disabled: atomic protocol/EVM dual-write not yet implemented"
                .into(),
        ))
    }

    // switchToProtocol(uint64 assetId, address to, uint128 amount) -> 0xbd8d87d4
    fn switch_to_protocol(
        &self,
        _input: &[u8],
        _msg_sender: alloy_primitives::Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 20000;
        crate::storage::StorageCtx::deduct_gas(GAS_COST).ok_or(PrecompileError::OutOfGas)?;
        Err(PrecompileError::Other(
            "switch precompile is disabled: atomic protocol/EVM dual-write not yet implemented"
                .into(),
        ))
    }
}

impl StatefulPrecompile for SwitchPrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: alloy_primitives::Address,
    ) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("too short".into()));
        }
        let selector = [calldata[0], calldata[1], calldata[2], calldata[3]];
        match selector {
            [0x43, 0x11, 0xf6, 0x13] => self.switch_to_evm(calldata, msg_sender),
            [0xbd, 0x8d, 0x87, 0xd4] => self.switch_to_protocol(calldata, msg_sender),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_switch_address() {
        assert_eq!(
            SWITCH_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000207")
        );
    }

    #[test]
    fn test_switch_to_evm_is_disabled() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = alloy_primitives::Address::repeat_byte(0x33);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = SwitchPrecompile;

            // Encode: switchToEvm(assetId, to, amount)
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0x43, 0x11, 0xf6, 0x13]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(&alloy_primitives::Address::repeat_byte(0x33).as_slice());
            input[84..100].copy_from_slice(&500u128.to_be_bytes());

            let result = precompile.call(&input, sender);
            assert!(result.is_err(), "switchToEvm should be disabled");
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("disabled"),
                "error should mention disabled: {err}"
            );
        });
    }

    #[test]
    fn test_switch_to_protocol_is_disabled() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let sender = alloy_primitives::Address::repeat_byte(0x44);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = SwitchPrecompile;

            // Encode: switchToProtocol(assetId, to, amount)
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0xbd, 0x8d, 0x87, 0xd4]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[48..68].copy_from_slice(&alloy_primitives::Address::repeat_byte(0x44).as_slice());
            input[84..100].copy_from_slice(&300u128.to_be_bytes());

            let result = precompile.call(&input, sender);
            assert!(result.is_err(), "switchToProtocol should be disabled");
            let err = result.unwrap_err().to_string();
            assert!(
                err.contains("disabled"),
                "error should mention disabled: {err}"
            );
        });
    }
}
