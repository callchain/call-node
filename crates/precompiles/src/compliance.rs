//! Compliance precompile at 0x205
//!
//! Compliance engine access: updateCompliance, checkCompliance.

use alloy_primitives::address;

use call_protocol::compliance::ComplianceStatus;
use crate::{
    decode_address, decode_u64, decode_u8, encode_u8, ok_empty, slot_compliance,
    u256_to_address,
};

pub const COMPLIANCE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000205");

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
use crate::storage::StorageCtx;

/// Read u8 from last byte of a U256.
fn u8_from_u256(v: alloy_primitives::U256) -> u8 {
    v.to_be_bytes::<32>()[31]
}

/// Write u8 into last byte of a U256.
fn u256_from_u8(v: u8) -> alloy_primitives::U256 {
    let mut bytes = [0u8; 32];
    bytes[31] = v;
    alloy_primitives::U256::from_be_bytes::<32>(bytes)
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
            .map(u8_from_u256)
            .unwrap_or(0);

        // Store compliance status
        let slot = slot_compliance(target, policy_id);
        StorageCtx::sstore(COMPLIANCE_ADDRESS, slot, u256_from_u8(status_u8));

        ok_empty()
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
                .map(u8_from_u256)
                .unwrap_or(0);
            // Clear (0) = pass, anything else = fail
            status == 0
        };

        let out = revm_precompile::PrecompileOutput::new(0, encode_u8(if result { 1 } else { 0 }).to_vec().into());
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

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;

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
                u256_from_u8_addr(issuer),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"compliance"]),
                u256_from_u8(1),
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
                u256_from_u8_addr(issuer),
            );
            crate::storage::StorageCtx::sstore(
                crate::ASSET_ADDRESS,
                crate::storage::storage_slot(&[&asset_id.to_be_bytes()[..], b"compliance"]),
                u256_from_u8(1),
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

    fn u256_from_u8_addr(addr: Address) -> alloy_primitives::U256 {
        let mut bytes = [0u8; 32];
        bytes[12..32].copy_from_slice(addr.as_slice());
        alloy_primitives::U256::from_be_bytes::<32>(bytes)
    }
}
