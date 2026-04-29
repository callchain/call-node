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

use crate::{current_caller, state_hook, state_hook::with_account_state};

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

fn require_caller() -> Result<alloy_primitives::Address, PrecompileError> {
    current_caller()
        .ok_or_else(|| PrecompileError::Other("caller not available".into()))
}

fn check_compliance(
    asset_id: u64,
    addr: &alloy_primitives::Address,
) -> Result<(), PrecompileError> {
    let policy_id = state_hook::with_registry(|reg| {
        reg.get_asset(asset_id).map(|a| a.compliance_policy)
    })
    .ok_or_else(|| {
        PrecompileError::Other("asset registry not available".into())
    })?
    .ok_or_else(|| {
        PrecompileError::Other("asset not found".into())
    })?;

    state_hook::with_compliance(|engine| {
        engine
            .check_compliance_by_policy_id(addr, policy_id)
            .map_err(|e| {
                PrecompileError::Other(format!("compliance check failed: {e}").into())
            })
    })
    .ok_or_else(|| {
        PrecompileError::Other("compliance engine not available".into())
    })?
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

// ── switchToEvm(uint64 assetId, address to, uint256 amount) ───────────

fn switch_to_evm(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 30000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 100 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let _to = decode_address(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid to address".into())
    })?;
    let amount = decode_u128(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;

    let caller = require_caller()?;

    // Asset must exist and be active
    let asset = state_hook::with_registry(|reg| reg.get_asset(asset_id).cloned())
        .ok_or_else(|| PrecompileError::Other("asset registry not available".into()))?
        .ok_or_else(|| PrecompileError::Other("asset not found".into()))?;

    if asset.status != call_protocol::registry::AssetStatus::Active {
        return Err(PrecompileError::Other("asset is not active".into()));
    }

    // Compliance check
    check_compliance(asset_id, &caller)?;
    check_compliance(asset_id, &_to)?;

    // Deduct protocol balance from caller
    with_account_state(|acc| {
        acc.deduct_balance(asset_id, caller, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))
    .and_then(|r| r)?;

    // Track EVM supply increase
    state_hook::with_registry(|reg| {
        reg.add_evm_supply(asset_id, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("asset registry not available".into()))
    .and_then(|r| r)?;

    // TODO: Mint wrapped ERC-20 tokens on EVM side.
    // This requires EVM state access which is not available through the
    // standard precompile function signature. Future work: special-case
    // this precompile in CallPrecompiles::run to manipulate the revm
    // journal directly, or wire an EvmExecutor callback through StateHookGuard.

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── switchToProtocol(uint64 assetId, address to, uint256 amount) ──────

fn switch_to_protocol(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 30000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 100 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let to = decode_address(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid to address".into())
    })?;
    let amount = decode_u128(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;

    let _caller = require_caller()?;

    // Asset must exist and be active
    let asset = state_hook::with_registry(|reg| reg.get_asset(asset_id).cloned())
        .ok_or_else(|| PrecompileError::Other("asset registry not available".into()))?
        .ok_or_else(|| PrecompileError::Other("asset not found".into()))?;

    if asset.status != call_protocol::registry::AssetStatus::Active {
        return Err(PrecompileError::Other("asset is not active".into()));
    }

    // Compliance check for recipient
    check_compliance(asset_id, &to)?;

    // Track EVM supply decrease
    state_hook::with_registry(|reg| {
        reg.sub_evm_supply(asset_id, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("asset registry not available".into()))
    .and_then(|r| r)?;

    // Credit protocol balance to recipient
    with_account_state(|acc| {
        acc.credit_balance(asset_id, to, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))
    .and_then(|r| r)?;

    // TODO: Burn wrapped ERC-20 tokens on EVM side.
    // The caller is expected to have burned their EVM tokens (e.g. via
    // WrappedToken.bridgeBurn) before or alongside calling this precompile.
    // Atomic dual-write requires EVM state access from the precompile context.

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
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
