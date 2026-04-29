//! Switch precompile at 0x207
//!
//! Bidirectional bridge between protocol balance and EVM wrapped tokens:
//! - switchToEvm: protocol balance → EVM ERC-20
//! - switchToProtocol: EVM ERC-20 → protocol balance
//!
//! # Current Limitation
//! This precompile handles protocol-side state only (AccountState + AssetRegistry).
//! The EVM-side (ERC-20 mint/burn) must be handled by a separate mechanism because
//! revm's standard precompile signature does not provide access to EVM state.
//! Future work: special-case this precompile in `CallPrecompiles::run` to access
//! the revm journal directly, or add a trait-based EVM ops callback to `StateHookGuard`.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult, PrecompileOutput};

// (no crate imports needed — switch is disabled)

#[allow(dead_code)]
pub(crate) const SWITCH_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000207");

// ── ABI decoding helpers (shared pattern with asset.rs) ───────────────

fn decode_u64(input: &[u8], slot_offset: usize) -> Option<u64> {
    let start = slot_offset + 24;
    if input.len() < start + 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&input[start..start + 8]);
    Some(u64::from_be_bytes(buf))
}

fn decode_address(input: &[u8], slot_offset: usize) -> Option<alloy_primitives::Address> {
    let start = slot_offset + 12;
    if input.len() < start + 20 {
        return None;
    }
    Some(alloy_primitives::Address::from_slice(&input[start..start + 20]))
}

fn decode_u128(input: &[u8], slot_offset: usize) -> Option<u128> {
    let start = slot_offset + 16;
    if input.len() < start + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&input[start..start + 16]);
    Some(u128::from_be_bytes(buf))
}

// ── Switch precompile entry point ─────────────────────────────────────

pub fn switch_precompile_fn(input: &[u8], _gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    // The switch precompile is disabled until atomic protocol/EVM dual-write
    // is implemented. Without EVM-side mint/burn, switchToEvm would deduct
    // protocol balance without creating the corresponding wrapped ERC-20,
    // and switchToProtocol would credit protocol balance without burning
    // the EVM tokens — creating supply imbalance and stuck funds.
    Err(PrecompileError::Other(
        "switch precompile is disabled: atomic protocol/EVM dual-write not yet implemented"
            .into(),
    ))
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
        // Encode: switchToEvm(assetId, to, amount)
        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0x43, 0x11, 0xf6, 0x13]);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(&alloy_primitives::Address::repeat_byte(0x33).as_slice());
        input[84..100].copy_from_slice(&500u128.to_be_bytes());

        let result = switch_precompile_fn(&input, 50000);
        assert!(result.is_err(), "switchToEvm should be disabled");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("disabled"),
            "error should mention disabled: {err}"
        );
    }

    #[test]
    fn test_switch_to_protocol_is_disabled() {
        // Encode: switchToProtocol(assetId, to, amount)
        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0xbd, 0x8d, 0x87, 0xd4]);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(&alloy_primitives::Address::repeat_byte(0x44).as_slice());
        input[84..100].copy_from_slice(&300u128.to_be_bytes());

        let result = switch_precompile_fn(&input, 50000);
        assert!(result.is_err(), "switchToProtocol should be disabled");
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("disabled"),
            "error should mention disabled: {err}"
        );
    }
}
