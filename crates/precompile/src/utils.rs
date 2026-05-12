//! Shared utility helpers for Callchain precompiles.
//!
//! ABI decode/encode, U256 conversions, and common storage slot layouts.
//! Precompiles should import from here instead of depending on each other.

use alloy_primitives::{address, Address, U256};
use revm_precompile::{PrecompileError, PrecompileOutput};

use crate::storage::storage_slot;
use crate::VALIDATOR_ADDRESS;

/// Asset precompile address (0x201).
pub const ASSET_ADDRESS: Address = address!("0000000000000000000000000000000000000201");

// ── ABI decoding helpers ──────────────────────────────────────────────

/// Read a uint64 from a 32-byte ABI-encoded slot (big-endian, right-aligned)
pub fn decode_u64(input: &[u8], slot_offset: usize) -> Option<u64> {
    let start = slot_offset + 24;
    if input.len() < start + 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&input[start..start + 8]);
    Some(u64::from_be_bytes(buf))
}

/// Read a u128 from a 32-byte ABI-encoded slot (big-endian, right-aligned)
pub fn decode_u128(input: &[u8], slot_offset: usize) -> Option<u128> {
    let start = slot_offset + 16;
    if input.len() < start + 16 {
        return None;
    }
    let mut buf = [0u8; 16];
    buf.copy_from_slice(&input[start..start + 16]);
    Some(u128::from_be_bytes(buf))
}

/// Read an Address from a 32-byte ABI-encoded slot (right-aligned)
pub fn decode_address(input: &[u8], slot_offset: usize) -> Option<Address> {
    let start = slot_offset + 12;
    if input.len() < start + 20 {
        return None;
    }
    Some(Address::from_slice(&input[start..start + 20]))
}

/// Read a u8 from the last byte of a 32-byte ABI-encoded slot
pub fn decode_u8(input: &[u8], slot_offset: usize) -> Option<u8> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    Some(input[slot_offset + 31])
}

/// Read a bytes32 value from a 32-byte ABI-encoded slot
pub fn decode_bytes32(input: &[u8], slot_offset: usize) -> Option<[u8; 32]> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let mut buf = [0u8; 32];
    buf.copy_from_slice(&input[slot_offset..slot_offset + 32]);
    Some(buf)
}

/// Read a uint256 as usize from a 32-byte slot (saturating)
pub fn decode_u256_usize(input: &[u8], slot_offset: usize) -> Option<usize> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let bytes = &input[slot_offset..slot_offset + 32];
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[24..32]);
    let val = u64::from_be_bytes(buf);
    Some(val as usize)
}

/// Read dynamic bytes from ABI-encoded input.
/// `slot_offset` points to the 32-byte offset slot.
pub fn decode_bytes(input: &[u8], slot_offset: usize) -> Option<Vec<u8>> {
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
    Some(input[data_start..data_start + len].to_vec())
}

/// Read a dynamic string from ABI-encoded input.
/// `slot_offset` points to the 32-byte offset slot.
pub fn decode_string(input: &[u8], slot_offset: usize) -> Option<String> {
    decode_bytes(input, slot_offset).map(|b| String::from_utf8_lossy(&b).into_owned())
}

/// Decode a dynamic `address[]` from an ABI-encoded offset slot.
pub fn decode_address_array(input: &[u8], slot_offset: usize) -> Option<Vec<Address>> {
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
pub fn decode_u128_array(input: &[u8], slot_offset: usize) -> Option<Vec<u128>> {
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

/// Decode a dynamic `bytes32[]` from an ABI-encoded offset slot.
pub fn decode_bytes32_array(input: &[u8], slot_offset: usize) -> Option<Vec<[u8; 32]>> {
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
        let val = decode_bytes32(input, elem_start + i * 32)?;
        out.push(val);
    }
    Some(out)
}

// ── ABI encoding helpers ──────────────────────────────────────────────

/// Encode a uint128 into 32 bytes (big-endian, right-aligned)
pub fn encode_u128(value: u128) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[16..].copy_from_slice(&value.to_be_bytes());
    out
}

/// Encode a uint64 into 32 bytes (big-endian, right-aligned)
pub fn encode_u64(value: u64) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[24..].copy_from_slice(&value.to_be_bytes());
    out
}

/// Encode a u8 into 32 bytes (last byte)
pub fn encode_u8(value: u8) -> [u8; 32] {
    let mut out = [0u8; 32];
    out[31] = value;
    out
}

/// Encode a uint256 (alias for encode_u128, since values fit in u128)
pub fn encode_u256(value: u128) -> [u8; 32] {
    encode_u128(value)
}

/// Encode a bool into 32 bytes
pub fn encode_bool(value: bool) -> [u8; 32] {
    encode_u8(if value { 1 } else { 0 })
}

// ── U256 conversion helpers ───────────────────────────────────────────

/// Read a u128 from the low 16 bytes of a U256.
#[allow(clippy::unwrap_used)]
pub fn u256_to_u128(v: U256) -> u128 {
    u128::from_be_bytes(
        v.to_be_bytes::<32>()[16..32]
            .try_into()
            .expect("invariant: 16-byte slice"),
    )
}

/// Write a u128 into the low 16 bytes of a U256.
pub fn u128_to_u256(v: u128) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[16..32].copy_from_slice(&v.to_be_bytes());
    U256::from_be_bytes::<32>(bytes)
}

/// Read a u64 from the low 8 bytes of a U256.
#[allow(clippy::unwrap_used)]
pub fn u256_to_u64(v: U256) -> u64 {
    u64::from_be_bytes(
        v.to_be_bytes::<32>()[24..32]
            .try_into()
            .expect("invariant: 8-byte slice"),
    )
}

/// Write a u64 into the low 8 bytes of a U256.
pub fn u64_to_u256(v: u64) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[24..32].copy_from_slice(&v.to_be_bytes());
    U256::from_be_bytes::<32>(bytes)
}

/// Read a u8 from the last byte of a U256.
pub fn u256_to_u8(v: U256) -> u8 {
    v.to_be_bytes::<32>()[31]
}

/// Write a u8 into the last byte of a U256.
pub fn u8_to_u256(v: u8) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[31] = v;
    U256::from_be_bytes::<32>(bytes)
}

/// Read an Address from the low 20 bytes of a U256.
pub fn u256_to_address(v: U256) -> Address {
    Address::from_slice(&v.to_be_bytes::<32>()[12..32])
}

/// Write an Address into the low 20 bytes of a U256.
pub fn address_to_u256(addr: Address) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(addr.as_slice());
    U256::from_be_bytes::<32>(bytes)
}

// ── String helpers ────────────────────────────────────────────────────

/// Read a short bytes32 string from a U256 storage word.
pub fn read_string32(v: U256) -> String {
    let bytes = v.to_be_bytes::<32>();
    let len = bytes.iter().take_while(|b| **b != 0).count();
    String::from_utf8_lossy(&bytes[..len]).into_owned()
}

/// Write a short string into a bytes32 U256 storage word.
pub fn write_string32(s: &str) -> U256 {
    let mut bytes = [0u8; 32];
    let src = s.as_bytes();
    let len = src.len().min(32);
    bytes[..len].copy_from_slice(&src[..len]);
    U256::from_be_bytes::<32>(bytes)
}

// ── Precompile output helpers ─────────────────────────────────────────

/// Return an empty successful precompile output.
pub fn ok_empty(storage: &dyn crate::storage::StorageProvider) -> crate::PrecompileResult {
    let output = PrecompileOutput::new(0, alloy_primitives::Bytes::new());
    Ok(crate::storage::fill_precompile_output(output, storage))
}

// ── Common storage slot helpers ───────────────────────────────────────

/// Compute the EVM storage slot for an asset balance.
pub fn slot_balance(asset_id: u64, addr: Address) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], addr.as_slice()])
}

/// Compute the EVM storage slot for asset metadata.
pub fn slot_asset_meta(asset_id: u64, suffix: &[u8]) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], suffix])
}

/// Compute the EVM storage slot for the EVM contract address of an asset.
pub fn slot_evm_contract(asset_id: u64) -> U256 {
    slot_asset_meta(asset_id, b"evm_contract")
}

/// Compute the EVM storage slot for the ERC-20 `balanceOf` mapping base slot of an asset.
pub fn slot_erc20_balance_of_base(asset_id: u64) -> U256 {
    slot_asset_meta(asset_id, b"erc20_balance_of_slot")
}

/// Compute the EVM storage slot for the ERC-20 `totalSupply` slot of an asset.
pub fn slot_erc20_total_supply(asset_id: u64) -> U256 {
    slot_asset_meta(asset_id, b"erc20_total_supply_slot")
}

/// Compute the EVM storage slot for an allowance.
pub fn slot_allowance(asset_id: u64, owner: Address, spender: Address) -> U256 {
    storage_slot(&[
        &asset_id.to_be_bytes()[..],
        owner.as_slice(),
        spender.as_slice(),
    ])
}

/// Compute the validator storage slot for an address.
pub fn slot_validator_by_addr(addr: Address) -> U256 {
    storage_slot(&[addr.as_slice(), b"validator_id"])
}

/// Compute compliance storage slot for an address under a policy.
pub fn slot_compliance(addr: Address, policy_id: u8) -> U256 {
    storage_slot(&[addr.as_slice(), &[policy_id]])
}

// ── Balance helpers ───────────────────────────────────────────────────

/// Load an asset balance from storage.
pub fn load_bal(
    storage: &mut dyn crate::storage::StorageProvider,
    asset_id: u64,
    addr: Address,
) -> u128 {
    storage
        .sload(ASSET_ADDRESS, slot_balance(asset_id, addr))
        .map(u256_to_u128)
        .unwrap_or(0)
}

/// Save an asset balance to storage.
pub fn save_bal(
    storage: &mut dyn crate::storage::StorageProvider,
    asset_id: u64,
    addr: Address,
    amount: u128,
) {
    let _ = storage.sstore(
        ASSET_ADDRESS,
        slot_balance(asset_id, addr),
        u128_to_u256(amount),
    );
}

/// Credit a balance (checked add).
pub fn credit_bal(
    storage: &mut dyn crate::storage::StorageProvider,
    asset_id: u64,
    addr: Address,
    amount: u128,
) -> Result<(), PrecompileError> {
    let bal = load_bal(storage, asset_id, addr)
        .checked_add(amount)
        .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;
    save_bal(storage, asset_id, addr, bal);
    Ok(())
}

/// Debit a balance (checked sub).
pub fn debit_bal(
    storage: &mut dyn crate::storage::StorageProvider,
    asset_id: u64,
    addr: Address,
    amount: u128,
) -> Result<(), PrecompileError> {
    let bal = load_bal(storage, asset_id, addr)
        .checked_sub(amount)
        .ok_or_else(|| PrecompileError::Other("insufficient balance".into()))?;
    save_bal(storage, asset_id, addr, bal);
    Ok(())
}

// ── Caller validation ─────────────────────────────────────────────────

/// Ensure the caller address is available (not in a static/delegate context).
pub fn require_caller(msg_sender: Address) -> Result<Address, PrecompileError> {
    if msg_sender == Address::ZERO {
        return Err(PrecompileError::Other("caller not available".into()));
    }
    Ok(msg_sender)
}

// ── Validator validation ──────────────────────────────────────────────

/// Check whether an address is a registered validator.
pub fn is_validator(storage: &mut dyn crate::storage::StorageProvider, sender: Address) -> bool {
    storage
        .sload(VALIDATOR_ADDRESS, slot_validator_by_addr(sender))
        .map(|v| u256_to_u64(v) != 0)
        .unwrap_or(false)
}

/// Require the sender to be a registered validator.
pub fn require_validator(
    storage: &mut dyn crate::storage::StorageProvider,
    sender: Address,
) -> Result<(), PrecompileError> {
    if !is_validator(storage, sender) {
        return Err(PrecompileError::Other(
            "sender not a registered validator".into(),
        ));
    }
    Ok(())
}
