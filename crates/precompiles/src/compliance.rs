//! Compliance precompile at 0x205
//!
//! Compliance engine access: updateCompliance, checkCompliance.

use alloy_primitives::address;
use revm_precompile::{PrecompileError, PrecompileResult, PrecompileOutput};

use crate::{current_caller, state_hook};
use call_protocol::instructions::ComplianceStatus;

#[allow(dead_code)]
pub(crate) const COMPLIANCE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000205");

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

fn decode_address(input: &[u8], slot_offset: usize) -> Option<alloy_primitives::Address> {
    let start = slot_offset + 12;
    if input.len() < start + 20 {
        return None;
    }
    Some(alloy_primitives::Address::from_slice(&input[start..start + 20]))
}

fn decode_u8(input: &[u8], slot_offset: usize) -> Option<u8> {
    if input.len() < slot_offset + 32 {
        return None;
    }
    Some(input[slot_offset + 31])
}

fn require_caller() -> Result<alloy_primitives::Address, PrecompileError> {
    current_caller()
        .ok_or_else(|| PrecompileError::Other("caller not available".into()))
}

fn decode_compliance_status(status_u8: u8) -> Option<ComplianceStatus> {
    match status_u8 {
        0 => Some(ComplianceStatus::Clear),
        1 => Some(ComplianceStatus::UnderReview),
        2 => Some(ComplianceStatus::Flagged),
        3 => Some(ComplianceStatus::Restricted),
        _ => None,
    }
}

// ── CompliancePrecompile (stateful) ───────────────────────────────────

use crate::StatefulPrecompile;
use crate::storage::{storage_slot, StorageCtx};

/// Compute compliance storage slot for an address under a policy.
fn slot_compliance(addr: alloy_primitives::Address, policy_id: u8) -> alloy_primitives::U256 {
    storage_slot(&[addr.as_slice(), &[policy_id]])
}

/// Read u8 from last byte of a U256.
fn u256_to_u8(v: alloy_primitives::U256) -> u8 {
    v.to_be_bytes::<32>()[31]
}

/// Write u8 into last byte of a U256.
fn u8_to_u256(v: u8) -> alloy_primitives::U256 {
    let mut bytes = [0u8; 32];
    bytes[31] = v;
    alloy_primitives::U256::from_be_bytes::<32>(bytes)
}

/// Read an Address from low 20 bytes of a U256.
fn u256_to_address(v: alloy_primitives::U256) -> alloy_primitives::Address {
    alloy_primitives::Address::from_slice(&v.to_be_bytes::<32>()[12..32])
}

/// Write an Address into low 20 bytes of a U256.
fn address_to_u256(addr: alloy_primitives::Address) -> alloy_primitives::U256 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(addr.as_slice());
    alloy_primitives::U256::from_be_bytes::<32>(bytes)
}

/// Read a u64 from low 8 bytes of a U256.
fn u256_to_u64(v: alloy_primitives::U256) -> u64 {
    u64::from_be_bytes(v.to_be_bytes::<32>()[24..32].try_into().unwrap())
}

/// Encode a ComplianceStatus to u8.
fn encode_compliance_status(status: ComplianceStatus) -> u8 {
    match status {
        ComplianceStatus::Clear => 0,
        ComplianceStatus::UnderReview => 1,
        ComplianceStatus::Flagged => 2,
        ComplianceStatus::Restricted => 3,
    }
}

#[derive(Debug, Default, Clone, Copy)]
pub struct CompliancePrecompile;

impl CompliancePrecompile {
    fn update_compliance(&self, input: &[u8], msg_sender: alloy_primitives::Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 10000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid asset_id".into()))?;
        let target = decode_address(input, 36)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid target address".into()))?;
        let status_u8 = decode_u8(input, 68)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid status".into()))?;

        let _status = decode_compliance_status(status_u8)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid compliance status value".into()))?;

        if msg_sender == alloy_primitives::Address::ZERO {
            return Err(revm_precompile::PrecompileError::Other("caller not available".into()));
        }

        // Verify caller is issuer by reading from ASSET_ADDRESS storage
        let issuer_slot = crate::storage::storage_slot(
            &[&asset_id.to_be_bytes()[..], b"issuer"]
        );
        let issuer = StorageCtx::sload(crate::ASSET_ADDRESS, issuer_slot)
            .map(u256_to_address)
            .unwrap_or(alloy_primitives::Address::ZERO);

        if issuer != msg_sender {
            return Err(revm_precompile::PrecompileError::Other("not asset issuer".into()));
        }

        // Get policy_id from asset metadata
        let policy_slot = crate::storage::storage_slot(
            &[&asset_id.to_be_bytes()[..], b"compliance"]
        );
        let policy_id = StorageCtx::sload(crate::ASSET_ADDRESS, policy_slot)
            .map(u256_to_u8)
            .unwrap_or(0);

        // Store compliance status
        let slot = slot_compliance(target, policy_id);
        StorageCtx::sstore(COMPLIANCE_ADDRESS, slot, u8_to_u256(status_u8));

        let out = revm_precompile::PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn check_compliance(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 68 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let target = decode_address(input, 4)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid target address".into()))?;
        let policy_id = decode_u8(input, 36)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid policy_id".into()))?;

        // policy_id = 0 means no policy -> always passes
        let result = if policy_id == 0 {
            true
        } else {
            let slot = slot_compliance(target, policy_id);
            let status = StorageCtx::sload(COMPLIANCE_ADDRESS, slot)
                .map(u256_to_u8)
                .unwrap_or(0);
            // Clear (0) = pass, anything else = fail
            status == 0
        };

        let mut output = [0u8; 32];
        if result {
            output[31] = 1;
        }

        let out = revm_precompile::PrecompileOutput::new(0, output.to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }
}

impl StatefulPrecompile for CompliancePrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: alloy_primitives::Address) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }
        match &calldata[..4] {
            &[0xa4, 0xb7, 0xf6, 0x6d] => self.update_compliance(calldata, msg_sender),
            &[0x0a, 0xe9, 0xb3, 0x4b] => self.check_compliance(calldata),
            _ => Err(revm_precompile::PrecompileError::Other("unknown selector".into())),
        }
    }
}

// ── Legacy stateless entry point (transition compatibility) ───────────

pub fn compliance_precompile_fn(input: &[u8], gas_limit: u64) -> PrecompileResult {
    if input.len() < 4 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    match &input[..4] {
        &[0xa4, 0xb7, 0xf6, 0x6d] => update_compliance_legacy(input, gas_limit),
        &[0x0a, 0xe9, 0xb3, 0x4b] => check_compliance_legacy(input, gas_limit),
        _ => Err(PrecompileError::Other("unknown selector".into())),
    }
}

// ── updateCompliance(uint64 assetId, address target, uint8 status) ────

fn update_compliance_legacy(input: &[u8], gas_limit: u64) -> PrecompileResult {
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
    let target = decode_address(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid target address".into())
    })?;
    let status_u8 = decode_u8(input, 68).ok_or_else(|| {
        PrecompileError::Other("invalid status".into())
    })?;

    let status = decode_compliance_status(status_u8).ok_or_else(|| {
        PrecompileError::Other("invalid compliance status value".into())
    })?;

    let caller = require_caller()?;

    // Look up asset and verify caller is the issuer
    let asset = state_hook::with_registry(|reg| reg.get_asset(asset_id).cloned())
        .ok_or_else(|| PrecompileError::Other("asset registry not available".into()))?
        .ok_or_else(|| PrecompileError::Other("asset not found".into()))?;

    if asset.issuer != caller {
        return Err(PrecompileError::Other("not asset issuer".into()));
    }

    let policy_id = asset.compliance_policy;

    // Update compliance status
    state_hook::with_compliance(|engine| {
        engine
            .set_address_compliance(target, policy_id, status)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))
    })
    .ok_or_else(|| PrecompileError::Other("compliance engine not available".into()))
    .and_then(|r| r)?;

    Ok(PrecompileOutput {
        bytes: alloy_primitives::Bytes::new(),
        gas_used: GAS_COST,
        gas_refunded: 0,
        reverted: false,
    })
}

// ── checkCompliance(address target, uint8 policyId) -> bool ───────────

fn check_compliance_legacy(input: &[u8], gas_limit: u64) -> PrecompileResult {
    const GAS_COST: u64 = 1000;
    if gas_limit < GAS_COST {
        return Err(PrecompileError::OutOfGas);
    }
    if input.len() < 68 {
        return Err(PrecompileError::Other("invalid input".into()));
    }

    let target = decode_address(input, 4).ok_or_else(|| {
        PrecompileError::Other("invalid target address".into())
    })?;
    let policy_id = decode_u8(input, 36).ok_or_else(|| {
        PrecompileError::Other("invalid policy_id".into())
    })?;

    let result = state_hook::with_compliance(|engine| {
        engine.check_compliance_by_policy_id(&target, policy_id).is_ok()
    })
    .ok_or_else(|| PrecompileError::Other("compliance engine not available".into()))?;

    // Encode bool as uint256 (32 bytes)
    let mut output = [0u8; 32];
    if result {
        output[31] = 1;
    }

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
    use call_protocol::compliance::ComplianceEngine;

    fn setup_state_hook(
        account: &mut AccountState,
        registry: &mut AssetRegistry,
        compliance: &mut ComplianceEngine,
    ) -> crate::state_hook::StateHookGuard {
        use call_oracle::OracleManager;
        use call_governance::GovernanceManager;
        use call_shielded::ShieldedState;

        let mut shielded = ShieldedState::new();
        let mut oracle = OracleManager::default();
        let mut gov = GovernanceManager::default();

        crate::state_hook::StateHookGuard::new(
            account,
            registry,
            compliance,
            &mut shielded,
            Some(&mut oracle),
            Some(&mut gov),
        )
    }

    #[test]
    fn test_compliance_address() {
        assert_eq!(
            COMPLIANCE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000205")
        );
    }

    #[test]
    fn test_update_compliance() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();

        let issuer = Address::repeat_byte(0x11);
        let asset_id = registry
            .register_asset("TEST".into(), "Test".into(), 18, issuer, 0, 100, 0)
            .unwrap();

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance);

        // Set caller as issuer
        crate::CURRENT_CALLER.with(|c| c.set(Some(issuer)));

        // Encode: updateCompliance(assetId, target, status=Restricted)
        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0xa4, 0xb7, 0xf6, 0x6d]);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(Address::repeat_byte(0x22).as_slice());
        input[99] = 3; // Restricted

        let result = compliance_precompile_fn(&input, 20000);
        assert!(result.is_ok(), "updateCompliance failed: {:?}", result);

        // Verify compliance state
        let status = compliance.get_address_compliance(&Address::repeat_byte(0x22), 0);
        assert_eq!(status, ComplianceStatus::Restricted);

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_update_compliance_not_issuer() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();

        let issuer = Address::repeat_byte(0x11);
        let asset_id = registry
            .register_asset("TEST".into(), "Test".into(), 18, issuer, 0, 100, 0)
            .unwrap();

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance);

        // Caller is not issuer
        crate::CURRENT_CALLER.with(|c| c.set(Some(Address::repeat_byte(0x99))));

        let mut input = vec![0u8; 100];
        input[0..4].copy_from_slice(&[0xa4, 0xb7, 0xf6, 0x6d]);
        input[28..36].copy_from_slice(&asset_id.to_be_bytes());
        input[48..68].copy_from_slice(Address::repeat_byte(0x22).as_slice());
        input[99] = 3;

        let result = compliance_precompile_fn(&input, 20000);
        assert!(result.is_err());

        crate::CURRENT_CALLER.with(|c| c.set(None));
    }

    #[test]
    fn test_check_compliance() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();

        // Add address to blacklist (policy_id = 1 = OfacBlacklist)
        compliance.add_to_blacklist(Address::repeat_byte(0x22));

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance);

        // checkCompliance(blacklisted_addr, policyId=1) -> false
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x0a, 0xe9, 0xb3, 0x4b]);
        input[16..36].copy_from_slice(Address::repeat_byte(0x22).as_slice());
        input[67] = 1; // OfacBlacklist

        let result = compliance_precompile_fn(&input, 5000).unwrap();
        assert_eq!(result.gas_used, 1000);
        // Bool false encoded as 32-byte 0
        assert_eq!(result.bytes.as_ref(), &[0u8; 32]);

        // checkCompliance(clean_addr, policyId=1) -> true
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x0a, 0xe9, 0xb3, 0x4b]);
        input[16..36].copy_from_slice(Address::repeat_byte(0x33).as_slice());
        input[67] = 1;

        let result = compliance_precompile_fn(&input, 5000).unwrap();
        // Bool true encoded as 32-byte with last byte = 1
        let mut expected = [0u8; 32];
        expected[31] = 1;
        assert_eq!(result.bytes.as_ref(), &expected);
    }

    #[test]
    fn test_check_compliance_none_policy() {
        let mut account = AccountState::new();
        let mut registry = AssetRegistry::new();
        let mut compliance = ComplianceEngine::default();

        let _guard = setup_state_hook(&mut account, &mut registry, &mut compliance);

        // policyId = 0 = None -> always passes
        let mut input = vec![0u8; 68];
        input[0..4].copy_from_slice(&[0x0a, 0xe9, 0xb3, 0x4b]);
        input[16..36].copy_from_slice(Address::repeat_byte(0x22).as_slice());
        input[67] = 0;

        let result = compliance_precompile_fn(&input, 5000).unwrap();
        let mut expected = [0u8; 32];
        expected[31] = 1;
        assert_eq!(result.bytes.as_ref(), &expected);
    }

    // ── CompliancePrecompile stateful tests ─────────────────────────────

    #[test]
    fn test_compliance_precompile_stateful_update_and_check() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x22);

        crate::storage::StorageCtx::enter(&mut provider, || {
            // Seed asset metadata: register asset_id=1 with issuer and policy_id=1
            let asset_id = 1u64;
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"issuer"]),
                address_to_u256(issuer),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"compliance"]),
                u8_to_u256(1),
            );

            let mut precompile = CompliancePrecompile;

            // updateCompliance(assetId=1, target, status=Restricted=3)
            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0xa4, 0xb7, 0xf6, 0x6d]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(target.as_slice());
            input[99] = 3; // Restricted

            let result = precompile.call(&input, issuer);
            assert!(result.is_ok(), "update_compliance failed: {:?}", result.err());

            // checkCompliance(target, policyId=1) -> false (Restricted)
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x0a, 0xe9, 0xb3, 0x4b]);
            input[16..36].copy_from_slice(target.as_slice());
            input[67] = 1;

            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 0);

            // checkCompliance(target, policyId=0) -> true (no policy)
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x0a, 0xe9, 0xb3, 0x4b]);
            input[16..36].copy_from_slice(target.as_slice());
            input[67] = 0;

            let result = precompile.call(&input, Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1);
        });
    }

    #[test]
    fn test_compliance_precompile_stateful_not_issuer() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let asset_id = 1u64;
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"issuer"]),
                address_to_u256(issuer),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"compliance"]),
                u8_to_u256(1),
            );

            let mut precompile = CompliancePrecompile;

            let mut input = vec![0u8; 100];
            input[0..4].copy_from_slice(&[0xa4, 0xb7, 0xf6, 0x6d]);
            input[28..36].copy_from_slice(&asset_id.to_be_bytes());
            input[48..68].copy_from_slice(Address::repeat_byte(0x22).as_slice());
            input[99] = 3;

            let result = precompile.call(&input, Address::repeat_byte(0x99));
            assert!(result.is_err());
        });
    }
}
