//! Compliance precompile entry point (0x205).
//!
//! Thin wrapper that routes EVM calls to [`ComplianceStorage`] backed by
//! [`JournalBackend`]. Business logic lives in [`ComplianceStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use crate::ComplianceStorage;
use alloy_sol_types::{sol, SolCall};
use call_precompile::{
    dispatch, journal_backend::JournalBackend, require_caller, storage::StorageProvider,
};
use call_primitives::Address;
use revm_precompile::{PrecompileError, PrecompileResult};

sol! {
    interface IProtocolCompliance {
        function updateCompliance(uint64 assetId, address target, uint8 status) external;
        function checkCompliance(uint64 assetId, address target) external view returns (bool);
    }
}

/// Stateful compliance precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct CompliancePrecompile;

impl CompliancePrecompile {
    fn update_compliance(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolCompliance::updateComplianceCall, _>(
            calldata,
            6000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = ComplianceStorage::new(JournalBackend::new(storage));
                store
                    .update_compliance(call.assetId, call.target, call.status, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn check_compliance(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolCompliance::checkComplianceCall, _, _>(
            calldata,
            1000,
            storage,
            |call, storage| {
                let store = ComplianceStorage::new(JournalBackend::new(storage));
                Ok(store.check_compliance(call.assetId, call.target))
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for CompliancePrecompile {
    #[allow(clippy::expect_used)]
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let selector: [u8; 4] = calldata[..4].try_into().expect("slice length checked above");
        match selector {
            IProtocolCompliance::updateComplianceCall::SELECTOR => {
                self.update_compliance(calldata, msg_sender, storage)
            }
            IProtocolCompliance::checkComplianceCall::SELECTOR => {
                self.check_compliance(calldata, storage)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{
        storage::{storage_slot, StorageProvider},
        u8_to_u256, StatefulPrecompile, ASSET_ADDRESS,
    };
    use call_primitives::Address;

    fn address_to_u256_word(addr: Address) -> alloy_primitives::U256 {
        let mut bytes = [0u8; 32];
        bytes[12..32].copy_from_slice(addr.as_slice());
        alloy_primitives::U256::from_be_bytes::<32>(bytes)
    }

    #[test]
    fn test_compliance_precompile_update_and_check() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);
        let target = Address::repeat_byte(0x22);

        // Seed asset metadata: register asset_id=1 with issuer and policy_id=1
        let asset_id = 1u64;
        provider
            .sstore(
                ASSET_ADDRESS,
                storage_slot(&[&asset_id.to_be_bytes()[..], b"issuer"]),
                address_to_u256_word(issuer),
            )
            .unwrap();
        provider
            .sstore(
                ASSET_ADDRESS,
                storage_slot(&[&asset_id.to_be_bytes()[..], b"compliance"]),
                u8_to_u256(1),
            )
            .unwrap();

        let mut precompile = CompliancePrecompile;

        // updateCompliance(assetId=1, target, status=Restricted=3)
        let input = IProtocolCompliance::updateComplianceCall {
            assetId: 1,
            target,
            status: 3,
        }
        .abi_encode();

        let result = precompile.call(&input, issuer, &mut provider);
        assert!(
            result.is_ok(),
            "update_compliance failed: {:?}",
            result.err()
        );

        // checkCompliance(assetId=1, target) -> false (Restricted)
        let input = IProtocolCompliance::checkComplianceCall { assetId: 1, target }.abi_encode();

        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        // bool false = 0 in last byte
        assert_eq!(result.bytes[31], 0);

        // checkCompliance(assetId=999, target) -> true (no policy, asset not registered)
        let input = IProtocolCompliance::checkComplianceCall {
            assetId: 999,
            target,
        }
        .abi_encode();

        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        // bool true = 1 in last byte
        assert_eq!(result.bytes[31], 1);
    }

    #[test]
    fn test_compliance_precompile_not_issuer() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);

        let asset_id = 1u64;
        provider
            .sstore(
                ASSET_ADDRESS,
                storage_slot(&[&asset_id.to_be_bytes()[..], b"issuer"]),
                address_to_u256_word(issuer),
            )
            .unwrap();
        provider
            .sstore(
                ASSET_ADDRESS,
                storage_slot(&[&asset_id.to_be_bytes()[..], b"compliance"]),
                u8_to_u256(1),
            )
            .unwrap();

        let mut precompile = CompliancePrecompile;

        let input = IProtocolCompliance::updateComplianceCall {
            assetId: 1,
            target: Address::repeat_byte(0x22),
            status: 3,
        }
        .abi_encode();

        let result = precompile.call(&input, Address::repeat_byte(0x99), &mut provider);
        assert!(result.is_err());
    }
}
