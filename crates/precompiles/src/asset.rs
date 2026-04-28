//! Asset precompile at 0x201
//!
//! Unified asset operations: getBalance, getAssetInfo, transfer, approve,
//! transferFrom, mint, burn, registerAsset.
//!
//! Replaces the deprecated 0x102 Balance precompile.

use alloy_primitives::{address, Address};
use revm_precompile::{PrecompileError, PrecompileResult, PrecompileOutput};

use crate::{current_caller, state_hook, state_hook::with_account_state};

#[allow(dead_code)]
pub(crate) const ASSET_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000201");

// ── ABI decoding helpers ──────────────────────────────────────────────

/// Read a uint64 from a 32-byte ABI-encoded slot (big-endian, right-aligned)
fn decode_u64(input: &[u8], slot_offset: usize) -> Option<u64> {
    let start = slot_offset + 24;
    if input.len() < start + 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&input[start..start + 8]);
    Some(u64::from_be_bytes(buf))
}

/// Read a u128 from a 32-byte ABI-encoded slot (big-endian, right-aligned)
fn decode_u128(input: &[u8], slot_offset: usize) -> Option<u128> {
    let start = slot_offset + 16;
    if input.len() < start + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&input[start..start + 16]);
    Some(u128::from_be_bytes(buf))
}

/// Read an Address from a 32-byte ABI-encoded slot (right-aligned)
fn decode_address(input: &[u8], slot_offset: usize) -> Option<Address> {
    let start = slot_offset + 12;
    if input.len() < start + 20 {
        return None;
    }
    Some(Address::from_slice(&input[start..start + 20]))
}

/// Read a dynamic string from ABI-encoded input.
/// `slot_offset` points to the 32-byte offset slot.
fn decode_string(input: &[u8], slot_offset: usize) -> Option<String> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset; // args start at byte 4
    if input.len() < abs_offset + 32 {
        return None;
    }
    let len = decode_u256_usize(input, abs_offset)?;
    let data_start = abs_offset + 32;
    if input.len() < data_start + len {
        return None;
    }
    Some(
        String::from_utf8_lossy(&input[data_start..data_start + len])
            .into_owned(),
    )
}

/// Decode a dynamic `address[]` from an ABI-encoded offset slot.
fn decode_address_array(input: &[u8], slot_offset: usize) -> Option<Vec<Address>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset; // args start at byte 4
    if input.len() < abs_offset + 32 {
        return None;
    }
    let len = decode_u256_usize(input, abs_offset)?;
    let elem_start = abs_offset + 32;
    if input.len() < elem_start + len * 32 {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let addr = decode_address(input, elem_start + i * 32)?;
        out.push(addr);
    }
    Some(out)
}

/// Decode a dynamic `uint128[]` from an ABI-encoded offset slot.
fn decode_u128_array(input: &[u8], slot_offset: usize) -> Option<Vec<u128>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset; // args start at byte 4
    if input.len() < abs_offset + 32 {
        return None;
    }
    let len = decode_u256_usize(input, abs_offset)?;
    let elem_start = abs_offset + 32;
    if input.len() < elem_start + len * 32 {
        return None;
    }
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let val = decode_u128(input, elem_start + i * 32)?;
        out.push(val);
    }
    Some(out)
}

/// Read a uint256 as usize from a 32-byte slot (saturating)
fn decode_u256_usize(input: &[u8], slot_offset: usize) -> Option<usize> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let bytes = &input[slot_offset..slot_offset + 32];
    // Most values fit in u64; saturate if too large
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[24..32]);
    let val = u64::from_be_bytes(buf);
    Some(val as usize)
}

/// Encode a uint256 into 32 bytes (big-endian)
fn encode_u256(value: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&value.to_be_bytes());
    out
}

/// Encode a uint64 into 32 bytes (big-endian, right-aligned)
fn encode_u64_slot(value: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&value.to_be_bytes());
    out
}

/// Encode a string into a bytes32 slot (left-aligned, space-padded)
fn encode_string32(s: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    let bytes = s.as_bytes();
    let len = bytes.len().min(32);
    out[..len].copy_from_slice(&bytes[..len]);
    out
}

// ── Asset precompile entry point ──────────────────────────────────────

pub fn asset_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let selector = &input[..4];

    match selector {
        &[0xd2, 0x14, 0x25, 0xdf] => asset_get_balance(input, gas_limit),
        &[0x4e, 0xc3, 0xce, 0x7f] => asset_get_asset_info(input, gas_limit),
        &[0xd1, 0x5d, 0xcd, 0x62] => asset_transfer(input, gas_limit),
        &[0x5f, 0x91, 0x61, 0xbb] => asset_batch_transfer(input, gas_limit),
        &[0x7e, 0x2e, 0xad, 0x93] => asset_approve(input, gas_limit),
        &[0xa1, 0x3e, 0x0f, 0xba] => asset_transfer_from(input, gas_limit),
        &[0xf2, 0xbe, 0x45, 0x99] => asset_mint(input, gas_limit),
        &[0x1a, 0x6e, 0x5f, 0x3b] => asset_burn(input, gas_limit),
        &[0xb2, 0xbf, 0x15, 0xdd] => asset_register_asset(input, gas_limit),
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}

// ── Read operations ───────────────────────────────────────────────────

fn asset_get_balance(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 800;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 68 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let addr = decode_address(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid address".into())
    })?;

    let balance = with_account_state(|acc| acc.get_balance(asset_id, &addr))
        .ok_or_else(|| {
            PrecompileError::Other("account state not available".into())
        })?;

    let output = encode_u256(balance);

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_get_asset_info(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 1000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 36 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;

    let asset = state_hook::with_registry(|reg| reg.get_asset(asset_id).cloned())
        .ok_or_else(|| {
            PrecompileError::Other("asset registry not available".into())
        })?
        .ok_or_else(|| {
            PrecompileError::Other("asset not found".into())
        })?;

    // Encode as 6 x 32-byte slots = 192 bytes
    // Slot 0: symbol (bytes32)
    // Slot 1: name (bytes32)
    // Slot 2: decimals (uint8 in last byte)
    // Slot 3: issuer (address in last 20 bytes)
    // Slot 4: maxSupply (uint256)
    // Slot 5: status (uint8 in last byte)
    let mut output = [0u8; 192];
    output[0..32].copy_from_slice(&encode_string32(&asset.symbol));
    output[32..64].copy_from_slice(&encode_string32(&asset.name));
    output[95] = asset.decimals;
    output[108..128].copy_from_slice(asset.issuer.as_slice());
    output[128..160].copy_from_slice(&encode_u256(asset.max_supply));
    output[191] = asset.status as u8;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── Write operations ──────────────────────────────────────────────────

fn require_caller() -> Result<Address, PrecompileError> {
    current_caller()
        .ok_or_else(|| PrecompileError::Other("caller not available".into()))
}

fn check_compliance(
    asset_id: u64,
    addr: &Address,
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

fn asset_transfer(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 5000;
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

    let from = require_caller()?;

    check_compliance(asset_id, &from)?;
    check_compliance(asset_id, &to)?;

    state_hook::with_account_state(|acc| {
        acc.transfer(asset_id, from, to, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| {
        PrecompileError::Other("account state not available".into())
    })
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_batch_transfer(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 100 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let recipients = decode_address_array(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid recipients array".into())
    })?;
    let amounts = decode_u128_array(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid amounts array".into())
    })?;

    if recipients.len() != amounts.len() {
        return Err(PrecompileError::Other(
            "recipients and amounts length mismatch".into(),
        ));
    }
    if recipients.is_empty() {
        return Err(PrecompileError::Other("empty batch".into()));
    }

    const GAS_COST_PER: u64 = 5000;
    let total_gas = GAS_COST_PER * recipients.len() as u64;
    if gas_limit < total_gas {
        return Err(PrecompileError::OutOfGas);
    }

    let from = require_caller()?;

    check_compliance(asset_id, &from)?;
    for to in &recipients {
        check_compliance(asset_id, to)?;
    }

    state_hook::with_account_state(|acc| {
        for (to, amount) in recipients.iter().zip(amounts.iter()) {
            acc.transfer(asset_id, from, *to, *amount)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
        }
        Ok::<_, PrecompileError>(())
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: total_gas,
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_approve(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 3000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 100 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let spender = decode_address(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid spender address".into())
    })?;
    let amount = decode_u128(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;

    let owner = require_caller()?;

    state_hook::with_account_state(|acc| {
        acc.allowances.set_allowance(asset_id, owner, spender, amount);
    })
    .ok_or_else(|| {
        PrecompileError::Other("account state not available".into())
    })?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_transfer_from(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 6000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 132 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let from = decode_address(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid from address".into())
    })?;
    let to = decode_address(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid to address".into())
    })?;
    let amount = decode_u128(input, 100).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;

    let spender = require_caller()?;

    check_compliance(asset_id, &from)?;
    check_compliance(asset_id, &to)?;

    state_hook::with_account_state(|acc| {
        // Check allowance
        let allowance = acc.allowances.get_allowance(asset_id, &from, &spender);
        if allowance < amount {
            return Err(PrecompileError::Other("insufficient allowance".into()));
        }
        acc.allowances
            .spend_allowance(asset_id, from, spender, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
        acc.transfer(asset_id, from, to, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| {
        PrecompileError::Other("account state not available".into())
    })
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_mint(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 10000;
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

    let caller = require_caller()?;

    // Verify issuer
    let is_issuer = state_hook::with_registry(|reg| {
        reg.get_asset(asset_id)
            .map(|a| a.issuer == caller)
            .unwrap_or(false)
    })
    .ok_or_else(|| {
        PrecompileError::Other("asset registry not available".into())
    })?;

    if !is_issuer {
        return Err(PrecompileError::Other("not asset issuer".into()));
    }

    // Update registry supply
    state_hook::with_registry(|reg| {
        reg.mint_supply(asset_id, &caller, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| {
        PrecompileError::Other("asset registry not available".into())
    })
    .and_then(|r| r)?;

    // Credit balance
    state_hook::with_account_state(|acc| {
        acc.credit_balance(asset_id, to, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| {
        PrecompileError::Other("account state not available".into())
    })
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_burn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 8000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 68 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let amount = decode_u128(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;

    let caller = require_caller()?;

    // Verify issuer
    let is_issuer = state_hook::with_registry(|reg| {
        reg.get_asset(asset_id)
            .map(|a| a.issuer == caller)
            .unwrap_or(false)
    })
    .ok_or_else(|| {
        PrecompileError::Other("asset registry not available".into())
    })?;

    if !is_issuer {
        return Err(PrecompileError::Other("not asset issuer".into()));
    }

    // Update registry supply
    state_hook::with_registry(|reg| {
        reg.burn_supply(asset_id, &caller, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| {
        PrecompileError::Other("asset registry not available".into())
    })
    .and_then(|r| r)?;

    // Debit balance from caller
    state_hook::with_account_state(|acc| {
        acc.burn(asset_id, caller, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| {
        PrecompileError::Other("account state not available".into())
    })
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

fn asset_register_asset(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 50000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    // Minimum input: selector + 4 args * 32 bytes = 132 bytes
    if input.len() < 132 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    // registerAsset(string symbol, string name, uint8 decimals, uint256 maxSupply)
    let symbol = decode_string(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid symbol".into())
    })?;
    let name = decode_string(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid name".into())
    })?;
    let decimals = decode_u64(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid decimals".into())
    })? as u8;
    let max_supply = decode_u128(input, 100).ok_or_else(|| {
        PrecompileError::Other("invalid maxSupply".into())
    })?;

    let caller = require_caller()?;

    let asset_id = state_hook::with_registry(|reg| {
        reg.register_asset(
            symbol,
            name,
            decimals,
            caller,
            0, // compliance_policy: default None
            0, // registered_at: will be set by caller with block timestamp
            max_supply,
        )
        .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| {
        PrecompileError::Other("asset registry not available".into())
    })
    .and_then(|r| r)?;

    let output = encode_u64_slot(asset_id);

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::from(output.to_vec()),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;
    use call_protocol::{AccountState, AssetRegistry};

    #[test]
    fn test_asset_address() {
        assert_eq!(
            ASSET_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000201")
        );
    }

    #[test]
    fn test_decode_u64() {
        let mut input = [0u8; 36];
        input[28..36].copy_from_slice(&42u64.to_be_bytes());
        assert_eq!(decode_u64(&input, 4), Some(42));
    }

    #[test]
    fn test_decode_address() {
        let mut input = [0u8; 36];
        let addr = Address::repeat_byte(0xAB);
        input[12..32].copy_from_slice(addr.as_slice());
        assert_eq!(decode_address(&input, 0), Some(addr));
    }

    #[test]
    fn test_encode_string32() {
        let encoded = encode_string32("TEST");
        assert_eq!(&encoded[0..4], b"TEST");
        assert_eq!(&encoded[4..32], &[0u8; 28]);
    }

    #[test]
    fn test_get_balance_no_hook() {
        // Without state hook, getBalance should fail
        let sel = [0xd2, 0x14, 0x25, 0xdf];
        let input = [&sel[..], &[0u8; 64]].concat();
        let result = asset_get_balance(&input, 10000);
        assert!(matches!(result, Err(PrecompileError::Other(_))));
    }

    #[test]
    fn test_get_balance_with_hook() {
        let mut account = AccountState::new();
        account.balances.set_balance(1, Address::repeat_byte(0xAB), 5000).unwrap();
        let mut registry = AssetRegistry::new();

        let _guard = crate::state_hook::StateHookGuard::new(
            &mut account,
            &mut registry,
            &mut call_protocol::compliance::ComplianceEngine::default(),
            &mut call_shielded::ShieldedState::new(),
            None,
            None,
        );

        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0xd2, 0x14, 0x25, 0xdf]);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(Address::repeat_byte(0xAB).as_slice());

        let result = asset_get_balance(&input, 10000).unwrap();
        assert_eq!(result.gas_used, 800);
        // Balance 5000 encoded as uint256
        let mut expected = [0u8; 32];
        expected[16..].copy_from_slice(&5000u128.to_be_bytes());
        assert_eq!(result.bytes.as_ref(), &expected);
    }

    #[test]
    fn test_transfer_with_hook() {
        let mut account = AccountState::new();
        account.balances.set_balance(1, Address::repeat_byte(0xAB), 1000).unwrap();
        let mut registry = AssetRegistry::new();
        registry.register_asset("TEST".into(), "Test".into(), 18, Address::repeat_byte(0xAB), 0, 0, 0).unwrap();

        let _guard = crate::state_hook::StateHookGuard::new(
            &mut account,
            &mut registry,
            &mut call_protocol::compliance::ComplianceEngine::default(),
            &mut call_shielded::ShieldedState::new(),
            None,
            None,
        );

        // Set caller context
        crate::CURRENT_CALLER.with(|c| c.set(Some(Address::repeat_byte(0xAB))));

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0xd1, 0x5d, 0xcd, 0x62]);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(Address::repeat_byte(0xCD).as_slice());
        input[84..100].copy_from_slice(&500u128.to_be_bytes());

        let result = asset_transfer(&input, 10000);
        assert!(result.is_ok(), "transfer failed: {:?}", result.err());
        assert_eq!(result.unwrap().gas_used, 5000);

        // Verify state
        assert_eq!(account.get_balance(1, &Address::repeat_byte(0xAB)), 500);
        assert_eq!(account.get_balance(1, &Address::repeat_byte(0xCD)), 500);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_approve_and_transfer_from() {
        let mut account = AccountState::new();
        account.balances.set_balance(1, Address::repeat_byte(0xAB), 1000).unwrap();
        let mut registry = AssetRegistry::new();
        registry.register_asset("TEST".into(), "Test".into(), 18, Address::repeat_byte(0xAB), 0, 0, 0).unwrap();

        let _guard = crate::state_hook::StateHookGuard::new(
            &mut account,
            &mut registry,
            &mut call_protocol::compliance::ComplianceEngine::default(),
            &mut call_shielded::ShieldedState::new(),
            None,
            None,
        );

        // Owner approves spender
        crate::CURRENT_CALLER.with(|c| c.set(Some(Address::repeat_byte(0xAB))));

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0x7e, 0x2e, 0xad, 0x93]);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(Address::repeat_byte(0xEF).as_slice());
        input[84..100].copy_from_slice(&300u128.to_be_bytes());

        let result = asset_approve(&input, 10000);
        assert!(result.is_ok());

        // Spender does transferFrom
        crate::CURRENT_CALLER.with(|c| c.set(Some(Address::repeat_byte(0xEF))));

        let mut input = vec![0u8; 132];
        input[0..4].copy_from_slice(&[0xa1, 0x3e, 0x0f, 0xba]);
        input[28..36].copy_from_slice(&1u64.to_be_bytes());
        input[48..68].copy_from_slice(Address::repeat_byte(0xAB).as_slice());
        input[80..100].copy_from_slice(Address::repeat_byte(0xCD).as_slice());
        input[116..132].copy_from_slice(&200u128.to_be_bytes());

        let result = asset_transfer_from(&input, 10000);
        assert!(result.is_ok(), "transfer_from failed: {:?}", result.err());

        assert_eq!(account.get_balance(1, &Address::repeat_byte(0xAB)), 800);
        assert_eq!(account.get_balance(1, &Address::repeat_byte(0xCD)), 200);
        assert_eq!(
            account.allowances.get_allowance(1, &Address::repeat_byte(0xAB), &Address::repeat_byte(0xEF)),
            100
        );

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_mint_and_burn() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let issuer = Address::repeat_byte(0x11);
        let id = registry.register_asset("GOLD".into(), "Gold Token".into(), 18, issuer, 0, 0, 10000).unwrap();

        let _guard = crate::state_hook::StateHookGuard::new(
            &mut account,
            &mut registry,
            &mut call_protocol::compliance::ComplianceEngine::default(),
            &mut call_shielded::ShieldedState::new(),
            None,
            None,
        );

        crate::CURRENT_CALLER.with(|c| c.set(Some(issuer)));

        // Mint
        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0xf2, 0xbe, 0x45, 0x99]);
        input[28..36].copy_from_slice(&id.to_be_bytes());
        input[48..68].copy_from_slice(Address::repeat_byte(0x22).as_slice());
        input[84..100].copy_from_slice(&500u128.to_be_bytes());

        let result = asset_mint(&input, 10000);
        assert!(result.is_ok(), "mint failed: {:?}", result.err());
        assert_eq!(account.get_balance(id, &Address::repeat_byte(0x22)), 500);
        assert_eq!(registry.get_asset(id).unwrap().protocol_supply, 500);

        // Mint some to issuer so we can burn from them
        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0xf2, 0xbe, 0x45, 0x99]);
        input[28..36].copy_from_slice(&id.to_be_bytes());
        input[48..68].copy_from_slice(issuer.as_slice());
        input[84..100].copy_from_slice(&400u128.to_be_bytes());

        let result = asset_mint(&input, 10000);
        assert!(result.is_ok(), "mint to issuer failed: {:?}", result.err());
        assert_eq!(account.get_balance(id, &issuer), 400);
        assert_eq!(registry.get_asset(id).unwrap().protocol_supply, 900);

        // Burn from issuer
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x1a, 0x6e, 0x5f, 0x3b]);
        input[28..36].copy_from_slice(&id.to_be_bytes());
        input[52..68].copy_from_slice(&200u128.to_be_bytes());

        let result = asset_burn(&input, 10000);
        assert!(result.is_ok(), "burn failed: {:?}", result.err());
        assert_eq!(account.get_balance(id, &Address::repeat_byte(0x22)), 500); // recipient unchanged
        assert_eq!(account.get_balance(id, &issuer), 200); // issuer burned 200
        assert_eq!(registry.get_asset(id).unwrap().protocol_supply, 700);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_batch_transfer_with_hook() {
        let mut account = AccountState::new();
        account.balances.set_balance(1, Address::repeat_byte(0xAB), 1000).unwrap();
        let mut registry = AssetRegistry::new();
        registry.register_asset("TEST".into(), "Test".into(), 18, Address::repeat_byte(0xAB), 0, 0, 0).unwrap();

        let _guard = crate::state_hook::StateHookGuard::new(
            &mut account,
            &mut registry,
            &mut call_protocol::compliance::ComplianceEngine::default(),
            &mut call_shielded::ShieldedState::new(),
            None,
            None,
        );

        crate::CURRENT_CALLER.with(|c| c.set(Some(Address::repeat_byte(0xAB))));

        // ABI encode: batchTransfer(uint64,address[],uint128[])
        // Static: asset_id(32), offset_to(32), offset_amounts(32)
        // Dynamic to: len(32), addr1(32), addr2(32)
        // Dynamic amounts: len(32), amount1(32), amount2(32)
        let addr1 = Address::repeat_byte(0xCD);
        let addr2 = Address::repeat_byte(0xEF);
        let amount1 = 200u128;
        let amount2 = 300u128;

        let static_size = 3 * 32;
        let to_array_size = 32 + 2 * 32;
        let amounts_array_size = 32 + 2 * 32;
        let offset_to = static_size;
        let offset_amounts = static_size + to_array_size;

        let mut input = vec![0u8; 4 + static_size + to_array_size + amounts_array_size];
        input[0..4].copy_from_slice(&[0x5f, 0x91, 0x61, 0xbb]);
        // asset_id
        input[4 + 24..4 + 32].copy_from_slice(&1u64.to_be_bytes());
        // offset_to
        input[36 + 24..36 + 32].copy_from_slice(&(offset_to as u64).to_be_bytes());
        // offset_amounts
        input[68 + 24..68 + 32].copy_from_slice(&(offset_amounts as u64).to_be_bytes());

        // to array at absolute 4 + offset_to
        let to_abs = 4 + offset_to;
        input[to_abs + 24..to_abs + 32].copy_from_slice(&2u64.to_be_bytes());
        input[to_abs + 32 + 12..to_abs + 32 + 32].copy_from_slice(addr1.as_slice());
        input[to_abs + 64 + 12..to_abs + 64 + 32].copy_from_slice(addr2.as_slice());

        // amounts array at absolute 4 + offset_amounts
        let amt_abs = 4 + offset_amounts;
        input[amt_abs + 24..amt_abs + 32].copy_from_slice(&2u64.to_be_bytes());
        input[amt_abs + 32 + 16..amt_abs + 32 + 32].copy_from_slice(&amount1.to_be_bytes());
        input[amt_abs + 64 + 16..amt_abs + 64 + 32].copy_from_slice(&amount2.to_be_bytes());

        let result = asset_batch_transfer(&input, 20000);
        assert!(result.is_ok(), "batch_transfer failed: {:?}", result.err());
        assert_eq!(result.unwrap().gas_used, 10000); // 5000 * 2

        // Verify state
        assert_eq!(account.get_balance(1, &Address::repeat_byte(0xAB)), 500); // 1000 - 200 - 300
        assert_eq!(account.get_balance(1, &addr1), 200);
        assert_eq!(account.get_balance(1, &addr2), 300);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_batch_transfer_length_mismatch() {
        let mut account = AccountState::new();
        account.balances.set_balance(1, Address::repeat_byte(0xAB), 1000).unwrap();
        let mut registry = AssetRegistry::new();
        registry.register_asset("TEST".into(), "Test".into(), 18, Address::repeat_byte(0xAB), 0, 0, 0).unwrap();

        let _guard = crate::state_hook::StateHookGuard::new(
            &mut account,
            &mut registry,
            &mut call_protocol::compliance::ComplianceEngine::default(),
            &mut call_shielded::ShieldedState::new(),
            None,
            None,
        );

        crate::CURRENT_CALLER.with(|c| c.set(Some(Address::repeat_byte(0xAB))));

        // 2 recipients but 1 amount
        let static_size = 3 * 32;
        let to_array_size = 32 + 2 * 32;
        let amounts_array_size = 32 + 1 * 32;
        let offset_to = static_size;
        let offset_amounts = static_size + to_array_size;

        let mut input = vec![0u8; 4 + static_size + to_array_size + amounts_array_size];
        input[0..4].copy_from_slice(&[0x5f, 0x91, 0x61, 0xbb]);
        input[4 + 24..4 + 32].copy_from_slice(&1u64.to_be_bytes());
        input[36 + 24..36 + 32].copy_from_slice(&(offset_to as u64).to_be_bytes());
        input[68 + 24..68 + 32].copy_from_slice(&(offset_amounts as u64).to_be_bytes());

        let to_abs = 4 + offset_to;
        input[to_abs + 24..to_abs + 32].copy_from_slice(&2u64.to_be_bytes());
        input[to_abs + 32 + 12..to_abs + 32 + 32].copy_from_slice(Address::repeat_byte(0xCD).as_slice());
        input[to_abs + 64 + 12..to_abs + 64 + 32].copy_from_slice(Address::repeat_byte(0xEF).as_slice());

        let amt_abs = 4 + offset_amounts;
        input[amt_abs + 24..amt_abs + 32].copy_from_slice(&1u64.to_be_bytes());
        input[amt_abs + 32 + 16..amt_abs + 32 + 32].copy_from_slice(&100u128.to_be_bytes());

        let result = asset_batch_transfer(&input, 20000);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("mismatch"));

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_register_asset() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();

        let _guard = crate::state_hook::StateHookGuard::new(
            &mut account,
            &mut registry,
            &mut call_protocol::compliance::ComplianceEngine::default(),
            &mut call_shielded::ShieldedState::new(),
            None,
            None,
        );

        let caller = Address::repeat_byte(0x33);
        crate::CURRENT_CALLER.with(|c| c.set(Some(caller)));

        // ABI encode: registerAsset("TEST", "Test Token", 18, 1000000)
        // String offsets are relative to start of args (byte 4)
        // Args: offset_symbol(32), offset_name(32), decimals(32), maxSupply(32)
        // symbol data at offset 128 -> absolute 132
        // name data follows symbol data
        let symbol = b"TEST";
        let name = b"Test Token";
        let symbol_data_len = 32 + 32; // len slot + data slot
        let name_offset = 128 + symbol_data_len;

        let mut input = vec![0u8; 4 + 4 * 32 + symbol_data_len + 32 + 32];
        input[0..4].copy_from_slice(&[0xb2, 0xbf, 0x15, 0xdd]);
        // offset_symbol = 128
        input[4 + 24..4 + 32].copy_from_slice(&128u64.to_be_bytes());
        // offset_name
        input[36 + 24..36 + 32].copy_from_slice(&(name_offset as u64).to_be_bytes());
        // decimals = 18
        input[68 + 31] = 18;
        // maxSupply = 1000000
        input[100 + 16..100 + 32].copy_from_slice(&1_000_000u128.to_be_bytes());

        // symbol data at absolute 132
        let sym_abs = 4 + 128;
        input[sym_abs + 24..sym_abs + 32].copy_from_slice(&(symbol.len() as u64).to_be_bytes());
        input[sym_abs + 32..sym_abs + 32 + symbol.len()].copy_from_slice(symbol);

        // name data at absolute 4 + name_offset
        let name_abs = 4 + name_offset;
        input[name_abs + 24..name_abs + 32].copy_from_slice(&(name.len() as u64).to_be_bytes());
        input[name_abs + 32..name_abs + 32 + name.len()].copy_from_slice(name);

        let result = asset_register_asset(&input, 100000).unwrap();
        assert_eq!(result.gas_used, 50000);

        let asset_id = u64::from_be_bytes([
            result.bytes[24], result.bytes[25], result.bytes[26], result.bytes[27],
            result.bytes[28], result.bytes[29], result.bytes[30], result.bytes[31],
        ]);
        assert_eq!(asset_id, 1);

        let asset = registry.get_asset(asset_id).unwrap();
        assert_eq!(asset.symbol, "TEST");
        assert_eq!(asset.name, "Test Token");
        assert_eq!(asset.decimals, 18);
        assert_eq!(asset.issuer, caller);
        assert_eq!(asset.max_supply, 1_000_000);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }
}
