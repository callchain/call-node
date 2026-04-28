//! Shielded precompile at 0x202
//!
//! Privacy operations: shieldedDeposit, shieldedWithdraw, shieldedTransfer.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult, PrecompileOutput};

use crate::{current_caller, state_hook};
use call_shielded::{Note, NoteCommitment, Nullifier, ShieldedTransfer, ZkProof};

#[allow(dead_code)]
pub(crate) const SHIELDED_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000202");

// ── ABI decoding helpers ──────────────────────────────────────────────

fn decode_u64(input: &[u8], slot_offset: usize) -> Option<u64> {
    let start = slot_offset + 24;
    if input.len() < start + 8 {
        return None;
    }
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&input[start..start + 8]);
    Some(u64::from_be_bytes(buf))
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

fn decode_address(input: &[u8], slot_offset: usize) -> Option<alloy_primitives::Address> {
    let start = slot_offset + 12;
    if input.len() < start + 20 {
        return None;
    }
    Some(alloy_primitives::Address::from_slice(&input[start..start + 20]))
}

fn decode_bytes32(input: &[u8], slot_offset: usize) -> Option<[u8; 32]> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&input[slot_offset..slot_offset + 32]);
    Some(out)
}

/// Read a uint256 as usize from a 32-byte slot (saturating)
fn decode_u256_usize(input: &[u8], slot_offset: usize) -> Option<usize> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    let bytes = &input[slot_offset..slot_offset + 32];
    let mut buf = [0u8; 8];
    buf.copy_from_slice(&bytes[24..32]);
    let val = u64::from_be_bytes(buf);
    Some(val as usize)
}

/// Decode a dynamic `bytes` type from ABI input.
/// `slot_offset` points to the 32-byte offset slot.
fn decode_bytes(input: &[u8], slot_offset: usize) -> Option<Vec<u8>> {
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

/// Decode a dynamic `bytes32[]` array from ABI input.
fn decode_bytes32_array(input: &[u8], slot_offset: usize) -> Option<Vec<[u8; 32]>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset;
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
        let offset = elem_start + i * 32;
        let mut elem = [0u8; 32];
        elem.copy_from_slice(&input[offset..offset + 32]);
        out.push(elem);
    }
    Some(out)
}

/// Decode a dynamic `bytes[]` array from ABI input.
fn decode_bytes_array(input: &[u8], slot_offset: usize) -> Option<Vec<Vec<u8>>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let arr_start = 4 + data_offset;
    if input.len() < arr_start + 32 {
        return None;
    }
    let len = decode_u256_usize(input, arr_start)?;
    // Each element is an offset (relative to arr_start) followed by length+data
    let mut out = Vec::with_capacity(len);
    for i in 0..len {
        let elem_offset_slot = arr_start + 32 + i * 32;
        if input.len() < elem_offset_slot + 32 {
            return None;
        }
        let elem_rel_offset = decode_u256_usize(input, elem_offset_slot)?;
        // In Solidity ABI, element offsets for dynamic types in dynamic arrays
        // are relative to the start of the offsets section (arr_start + 32)
        let elem_abs_offset = arr_start + 32 + elem_rel_offset;
        if input.len() < elem_abs_offset + 32 {
            return None;
        }
        let elem_len = decode_u256_usize(input, elem_abs_offset)?;
        let elem_data_start = elem_abs_offset + 32;
        if input.len() < elem_data_start + elem_len {
            return None;
        }
        out.push(input[elem_data_start..elem_data_start + elem_len].to_vec());
    }
    Some(out)
}

fn require_caller() -> Result<alloy_primitives::Address, PrecompileError> {
    current_caller()
        .ok_or_else(|| PrecompileError::Other("caller not available".into()))
}

// ── Shielded precompile entry point ───────────────────────────────────

pub fn shielded_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    match &input[..4] {
        &[0x43, 0x28, 0xf9, 0x2f] => shielded_deposit(input, gas_limit),
        &[0x5f, 0x82, 0xf0, 0x03] => shielded_withdraw(input, gas_limit),
        &[0xdc, 0x9a, 0x6c, 0xf6] => shielded_transfer(input, gas_limit),
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}

// ── shieldedDeposit(uint64 assetId, uint256 amount, bytes32 commitment, bytes encryptedNote) ──

fn shielded_deposit(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 50000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    // Minimum: selector + 3 fixed slots + 1 dynamic offset slot = 4 + 4*32 = 132
    if input.len() < 132 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let amount = decode_u128(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;
    let commitment = decode_bytes32(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid commitment".into())
    })?;
    let encrypted_note = decode_bytes(input, 100).ok_or_else(|| {
        PrecompileError::Other("invalid encrypted_note".into())
    })?;

    let caller = require_caller()?;

    // Deduct from caller's transparent balance
    state_hook::with_account_state(|acc| {
        acc.deduct_balance(asset_id, caller, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))
    .and_then(|r| r)?;

    // Register note in shielded pool
    let note = Note::from_encrypted_bytes(&encrypted_note)
        .map_err(|e| PrecompileError::Other(format!("invalid encrypted note: {e}").into()))?;
    let note_cm = NoteCommitment::new(call_primitives::Hash::from_slice(&commitment));

    state_hook::with_shielded_state(|shielded| {
        shielded
            .process_deposit(note_cm, note)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("shielded state not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── shieldedWithdraw(uint64 assetId, address target, uint256 amount, bytes proof, bytes32 nullifier) ──

fn shielded_withdraw(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 50000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    // Minimum: selector + 4 fixed slots + 1 dynamic offset slot = 4 + 5*32 = 164
    if input.len() < 164 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let target = decode_address(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid target address".into())
    })?;
    let amount = decode_u128(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid amount".into())
    })?;
    let proof = decode_bytes(input, 100).ok_or_else(|| {
        PrecompileError::Other("invalid proof".into())
    })?;
    let nullifier = decode_bytes32(input, 132).ok_or_else(|| {
        PrecompileError::Other("invalid nullifier".into())
    })?;

    // Verify ZK proof structure
    let zk_proof = ZkProof {
        proof_data: proof,
        nullifiers: vec![Nullifier::new(call_primitives::Hash::from_slice(&nullifier))],
        commitments: vec![],
        asset_id,
    };
    if !call_shielded::verify_zk_proof(&zk_proof) {
        return Err(PrecompileError::Other(
            "invalid ZK proof structure".into(),
        ));
    }

    // Real Groth16 verification when real-prover feature is enabled
    #[cfg(feature = "real-prover")]
    {
        let merkle_root = state_hook::with_shielded_state(|shielded| shielded.merkle_root())
            .ok_or_else(|| PrecompileError::Other("shielded state not available".into()))?;
        let merkle_root_bytes: [u8; 32] = merkle_root.into();
        let valid = call_shielded::verify_shielded_proof(
            &zk_proof, "withdraw", Some(&merkle_root_bytes), Some(amount),
        )
        .map_err(|e| PrecompileError::Other(format!("proof verification error: {e}").into()))?;
        if !valid {
            return Err(PrecompileError::Other("ZK proof verification failed".into()));
        }
    }

    // Process withdraw in shielded pool
    state_hook::with_shielded_state(|shielded| {
        shielded
            .process_withdraw(Nullifier::new(call_primitives::Hash::from_slice(&nullifier)))
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("shielded state not available".into()))
    .and_then(|r| r)?;

    // Credit transparent balance to target
    // Note: existing instruction handler credits asset_id 0 (CALL) regardless of asset_id
    state_hook::with_account_state(|acc| {
        acc.credit_balance(0, target, amount)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("account state not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── shieldedTransfer(uint64 assetId, bytes proof, bytes32[] nullifiers, bytes32[] commitments, bytes[] encryptedNotes) ──

fn shielded_transfer(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 100000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    // Minimum: selector + 1 fixed slot + 4 dynamic offset slots = 4 + 5*32 = 164
    if input.len() < 164 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let asset_id = decode_u64(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid asset_id".into())
    })?;
    let proof = decode_bytes(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid proof".into())
    })?;
    let nullifiers = decode_bytes32_array(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid nullifiers".into())
    })?;
    let commitments = decode_bytes32_array(input, 100).ok_or_else(|| {
        PrecompileError::Other("invalid commitments".into())
    })?;
    let encrypted_notes = decode_bytes_array(input, 132).ok_or_else(|| {
        PrecompileError::Other("invalid encrypted_notes".into())
    })?;

    // Reconstruct ZkProof
    let zk_proof = ZkProof {
        proof_data: proof,
        nullifiers: nullifiers
            .into_iter()
            .map(|h| Nullifier::new(call_primitives::Hash::from_slice(&h)))
            .collect(),
        commitments: commitments
            .into_iter()
            .map(|h| NoteCommitment::new(call_primitives::Hash::from_slice(&h)))
            .collect(),
        asset_id,
    };

    // Verify ZK proof structure
    if !call_shielded::verify_zk_proof(&zk_proof) {
        return Err(PrecompileError::Other(
            "invalid ZK proof structure".into(),
        ));
    }

    // Real Groth16 verification when real-prover feature is enabled
    #[cfg(feature = "real-prover")]
    {
        let merkle_root = state_hook::with_shielded_state(|shielded| shielded.merkle_root())
            .ok_or_else(|| PrecompileError::Other("shielded state not available".into()))?;
        let merkle_root_bytes: [u8; 32] = merkle_root.into();
        let valid = call_shielded::verify_shielded_proof(
            &zk_proof, "transfer", Some(&merkle_root_bytes), None,
        )
        .map_err(|e| PrecompileError::Other(format!("proof verification error: {e}").into()))?;
        if !valid {
            return Err(PrecompileError::Other("ZK proof verification failed".into()));
        }
    }

    // Decrypt output notes from encrypted_notes field
    let output_notes: Vec<Note> = encrypted_notes
        .into_iter()
        .filter_map(|data| Note::from_encrypted_bytes(&data).ok())
        .collect();

    let transfer = ShieldedTransfer {
        input_notes: vec![], // input notes are not transmitted; proven via ZK
        output_notes,
        proof: zk_proof,
    };

    state_hook::with_shielded_state(|shielded| {
        shielded
            .process_transfer(&transfer)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("shielded state not available".into()))
    .and_then(|r| r)?;

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
    use call_primitives::Address;
    use call_protocol::{AccountState, AssetRegistry};
    use call_protocol::compliance::ComplianceEngine;
    use call_shielded::{ShieldedState, ViewingKey};

    fn setup_state_hook(
        account: &mut AccountState,
        registry: &mut AssetRegistry,
        compliance: &mut ComplianceEngine,
        shielded: &mut ShieldedState,
    ) -> crate::state_hook::StateHookGuard {
        use call_oracle::OracleManager;
        use call_governance::GovernanceManager;

        let mut oracle = OracleManager::default();
        let mut gov = GovernanceManager::default();

        crate::state_hook::StateHookGuard::new(
            account,
            registry,
            compliance,
            shielded,
            Some(&mut oracle),
            Some(&mut gov),
        )
    }

    /// ABI-encode a `bytes` value: length (32 bytes) + data (padded to 32-byte words)
    fn abi_encode_bytes(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0u8; 32]; // length
        out[24..32].copy_from_slice(&(data.len() as u64).to_be_bytes());
        let padded_len = ((data.len() + 31) / 32) * 32;
        out.extend_from_slice(data);
        out.extend_from_slice(&vec![0u8; padded_len - data.len()]);
        out
    }

    /// ABI-encode a `bytes32[]` array: length + elements
    fn abi_encode_bytes32_array(arr: &[[u8; 32]]) -> Vec<u8> {
        let mut out = vec![0u8; 32]; // length
        out[24..32].copy_from_slice(&(arr.len() as u64).to_be_bytes());
        for elem in arr {
            out.extend_from_slice(elem);
        }
        out
    }

    /// ABI-encode a `bytes[]` array: length + offsets + element data
    fn abi_encode_bytes_array(arr: &[Vec<u8>]) -> Vec<u8> {
        let mut out = vec![0u8; 32]; // length
        out[24..32].copy_from_slice(&(arr.len() as u64).to_be_bytes());

        // Compute total size of offset section
        let offset_section_len = arr.len() * 32;
        let mut elem_data = Vec::new();

        for elem in arr {
            let offset = offset_section_len + elem_data.len();
            let mut offset_buf = [0u8; 32];
            offset_buf[24..32].copy_from_slice(&(offset as u64).to_be_bytes());
            out.extend_from_slice(&offset_buf);

            elem_data.extend_from_slice(&abi_encode_bytes(elem));
        }
        out.extend_from_slice(&elem_data);
        out
    }

    #[test]
    fn test_shielded_address() {
        assert_eq!(
            SHIELDED_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000202")
        );
    }

    #[test]
    fn test_shielded_deposit() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut shielded = ShieldedState::new();

        // Register asset and give caller balance
        let asset_id = registry
            .register_asset("CALL".into(), "Call".into(), 18, Address::repeat_byte(0x11), 0, 0, 0)
            .unwrap();
        account.credit_balance(asset_id, Address::repeat_byte(0x22), 1000).unwrap();

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut shielded);

        // Set caller context
        crate::CURRENT_CALLER.with(|c| c.set(Some(Address::repeat_byte(0x22))));

        // Create a note and encrypt it
        let vk = ViewingKey::generate(&[1u8; 32]);
        let note = Note::new(500, asset_id, &vk, call_primitives::Hash::repeat_byte(0xAB));
        let encrypted_note = note.to_encrypted_bytes();
        let commitment = note.commitment().as_hash().0;

        // Encode: shieldedDeposit(assetId, amount, commitment, encryptedNote)
        // Fixed slots: assetId(32), amount(32), commitment(32), offset(32)
        // Total fixed = 128 bytes; encryptedNote data follows
        let mut input = vec![0u8; 4 + 128];
        input[0..4].copy_from_slice(&[0x43, 0x28, 0xf9, 0x2f]);
        input[4 + 24..4 + 32].copy_from_slice(&asset_id.to_be_bytes());
        input[36 + 16..36 + 32].copy_from_slice(&500u128.to_be_bytes());
        input[68..100].copy_from_slice(&commitment);
        // offset to encryptedNote data = 128 (relative to byte 4)
        input[100 + 24..100 + 32].copy_from_slice(&128u64.to_be_bytes());

        let enc_data = abi_encode_bytes(&encrypted_note);
        input.extend_from_slice(&enc_data);

        let result = shielded_deposit(&input, 100000);
        assert!(result.is_ok(), "shieldedDeposit failed: {:?}", result);

        // Verify balance deducted
        assert_eq!(account.get_balance(asset_id, &Address::repeat_byte(0x22)), 500);

        // Verify note registered in shielded pool
        assert!(shielded.get_note(&note.commitment()).is_some());

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_shielded_withdraw() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut shielded = ShieldedState::new();

        // First deposit a note so we can withdraw it
        let asset_id = registry
            .register_asset("CALL".into(), "Call".into(), 18, Address::repeat_byte(0x11), 0, 0, 0)
            .unwrap();

        let vk = ViewingKey::generate(&[1u8; 32]);
        let note = Note::new(500, asset_id, &vk, call_primitives::Hash::repeat_byte(0xAB));
        let encrypted_note = note.to_encrypted_bytes();
        let commitment = note.commitment().as_hash().0;
        let nullifier = note.nullifier().as_hash().0;

        shielded.process_deposit(NoteCommitment::new(commitment.into()), note.clone()).unwrap();

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut shielded);

        crate::CURRENT_CALLER.with(|c| c.set(Some(Address::repeat_byte(0x22))));

        // Encode: shieldedWithdraw(assetId, target, amount, proof, nullifier)
        // Fixed slots: assetId(32), target(32), amount(32), proof_offset(32), nullifier(32)
        // Total fixed = 160 bytes; proof data follows
        let proof_data = vec![1u8; 64]; // mock proof data
        let mut input = vec![0u8; 4 + 160];
        input[0..4].copy_from_slice(&[0x5f, 0x82, 0xf0, 0x03]);
        input[4 + 24..4 + 32].copy_from_slice(&asset_id.to_be_bytes());
        input[36 + 12..36 + 32].copy_from_slice(Address::repeat_byte(0x44).as_slice());
        input[68 + 16..68 + 32].copy_from_slice(&500u128.to_be_bytes());
        // offset to proof data = 160 (relative to byte 4)
        input[100 + 24..100 + 32].copy_from_slice(&160u64.to_be_bytes());
        input[132..164].copy_from_slice(&nullifier);

        let proof_enc = abi_encode_bytes(&proof_data);
        input.extend_from_slice(&proof_enc);

        let result = shielded_withdraw(&input, 100000);
        assert!(result.is_ok(), "shieldedWithdraw failed: {:?}", result);

        // Verify nullifier spent
        assert!(shielded.nullifier_set.is_spent(&Nullifier::new(nullifier.into())));

        // Verify target credited with CALL (asset_id 0)
        assert_eq!(account.get_balance(0, &Address::repeat_byte(0x44)), 500);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_shielded_transfer() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();
        let mut shielded = ShieldedState::new();

        // Deposit a note first so we have something to transfer from
        let asset_id = registry
            .register_asset("CALL".into(), "Call".into(), 18, Address::repeat_byte(0x11), 0, 0, 0)
            .unwrap();

        let vk = ViewingKey::generate(&[1u8; 32]);
        let note = Note::new(500, asset_id, &vk, call_primitives::Hash::repeat_byte(0xAB));
        let commitment = note.commitment().as_hash().0;
        let nullifier = note.nullifier().as_hash().0;

        shielded.process_deposit(NoteCommitment::new(commitment.into()), note.clone()).unwrap();

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance, &mut shielded);

        // Encode: shieldedTransfer(assetId, proof, nullifiers[], commitments[], encryptedNotes[])
        let proof_data = vec![1u8; 64];
        let nullifiers_arr = vec![nullifier];
        let output_vk = ViewingKey::generate(&[2u8; 32]);
        let output_note = Note::new(400, asset_id, &output_vk, call_primitives::Hash::repeat_byte(0xCD));
        let output_enc = output_note.to_encrypted_bytes();
        let output_cm = output_note.commitment().as_hash().0;
        let commitments_arr = vec![output_cm];
        let encrypted_notes_arr = vec![output_enc];

        // Build the ABI encoding manually
        // Fixed slots: assetId(32), proof_offset(32), nullifiers_offset(32), commitments_offset(32), encryptedNotes_offset(32)
        // Total fixed = 160 bytes
        let proof_enc = abi_encode_bytes(&proof_data);
        let nullifiers_enc = abi_encode_bytes32_array(&nullifiers_arr);
        let commitments_enc = abi_encode_bytes32_array(&commitments_arr);
        let encrypted_notes_enc = abi_encode_bytes_array(&encrypted_notes_arr);

        let mut input = vec![0u8; 4 + 160];
        input[0..4].copy_from_slice(&[0xdc, 0x9a, 0x6c, 0xf6]);
        input[4 + 24..4 + 32].copy_from_slice(&asset_id.to_be_bytes());

        // Offsets relative to byte 4
        let proof_offset = 160;
        let nullifiers_offset = proof_offset + proof_enc.len();
        let commitments_offset = nullifiers_offset + nullifiers_enc.len();
        let encrypted_notes_offset = commitments_offset + commitments_enc.len();

        input[36 + 24..36 + 32].copy_from_slice(&(proof_offset as u64).to_be_bytes());
        input[68 + 24..68 + 32].copy_from_slice(&(nullifiers_offset as u64).to_be_bytes());
        input[100 + 24..100 + 32].copy_from_slice(&(commitments_offset as u64).to_be_bytes());
        input[132 + 24..132 + 32].copy_from_slice(&(encrypted_notes_offset as u64).to_be_bytes());

        input.extend_from_slice(&proof_enc);
        input.extend_from_slice(&nullifiers_enc);
        input.extend_from_slice(&commitments_enc);
        input.extend_from_slice(&encrypted_notes_enc);

        let result = shielded_transfer(&input, 200000);
        assert!(result.is_ok(), "shieldedTransfer failed: {:?}", result);

        // Verify nullifier spent
        assert!(shielded.nullifier_set.is_spent(&Nullifier::new(nullifier.into())));

        // Verify output note registered
        assert!(shielded.get_note(&output_note.commitment()).is_some());
    }
}
