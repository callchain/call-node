//! Compliance precompile entry point (0x205).
//!
//! Thin wrapper that routes EVM calls to [`ComplianceStorage`] backed by
//! EVM storage. Business logic lives in [`ComplianceStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use crate::ComplianceStorage;
use alloy_sol_types::{sol, SolCall};
use call_precompile::{dispatch, require_caller, storage::StorageProvider, StorageRef};
use call_primitives::Address;
use revm_precompile::{PrecompileError, PrecompileResult};

sol! {
    interface IProtocolCompliance {
        function updateCompliance(address target, uint8 status) external;
        function checkCompliance(address target) external view returns (bool);
        function setComplianceAdmin(address newAdmin) external;
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
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolCompliance::updateComplianceCall, _>(
            calldata,
            6000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = ComplianceStorage::new(sr);
                store
                    .update_compliance(call.target, call.status, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn check_compliance(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolCompliance::checkComplianceCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = ComplianceStorage::new(sr);
                Ok(store.check_compliance(call.target))
            },
        )
    }

    fn set_compliance_admin(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolCompliance::setComplianceAdminCall, _>(
            calldata,
            4000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = ComplianceStorage::new(sr);
                store
                    .set_admin(call.newAdmin, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
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
        let selector: [u8; 4] = calldata[..4]
            .try_into()
            .expect("slice length checked above");
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolCompliance::updateComplianceCall::SELECTOR => {
                self.update_compliance(calldata, msg_sender, storage, sr)
            }
            IProtocolCompliance::checkComplianceCall::SELECTOR => {
                self.check_compliance(calldata, storage, sr)
            }
            IProtocolCompliance::setComplianceAdminCall::SELECTOR => {
                self.set_compliance_admin(calldata, msg_sender, storage, sr)
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
        u8_to_u256, StatefulPrecompile, COMPLIANCE_ADDRESS, GOVERNANCE_ADDRESS,
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
        let target = Address::repeat_byte(0x22);

        let mut precompile = CompliancePrecompile;

        // updateCompliance(target, status=3) called by GOVERNANCE_ADDRESS
        let input = IProtocolCompliance::updateComplianceCall {
            target,
            status: 3,
        }
        .abi_encode();

        let result = precompile.call(&input, GOVERNANCE_ADDRESS, &mut provider);
        assert!(
            result.is_ok(),
            "update_compliance failed: {:?}",
            result.err()
        );

        // checkCompliance(target) -> false (Restricted)
        let input = IProtocolCompliance::checkComplianceCall { target }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        // bool false = 0 in last byte
        assert_eq!(result.bytes[31], 0);
    }

    #[test]
    fn test_compliance_precompile_not_governance() {
        let mut provider = HashMapStorageProvider::new(1_000_000);

        let mut precompile = CompliancePrecompile;

        let input = IProtocolCompliance::updateComplianceCall {
            target: Address::repeat_byte(0x22),
            status: 3,
        }
        .abi_encode();

        let result = precompile.call(&input, Address::repeat_byte(0x99), &mut provider);
        assert!(result.is_err());
    }

    #[test]
    fn test_compliance_precompile_clear_status() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let target = Address::repeat_byte(0x22);

        let mut precompile = CompliancePrecompile;

        // Set restricted
        let input = IProtocolCompliance::updateComplianceCall {
            target,
            status: 3,
        }
        .abi_encode();
        precompile.call(&input, GOVERNANCE_ADDRESS, &mut provider).unwrap();

        // Check: restricted
        let input = IProtocolCompliance::checkComplianceCall { target }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 0); // false

        // Clear
        let input = IProtocolCompliance::updateComplianceCall {
            target,
            status: 0,
        }
        .abi_encode();
        precompile.call(&input, GOVERNANCE_ADDRESS, &mut provider).unwrap();

        // Check: clear
        let input = IProtocolCompliance::checkComplianceCall { target }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1); // true
    }

    #[test]
    fn test_compliance_precompile_set_admin() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let new_admin = Address::repeat_byte(0x44);

        let mut precompile = CompliancePrecompile;

        // setComplianceAdmin by GOVERNANCE_ADDRESS (initial admin)
        let input = IProtocolCompliance::setComplianceAdminCall {
            newAdmin: new_admin,
        }
        .abi_encode();
        let result = precompile.call(&input, GOVERNANCE_ADDRESS, &mut provider);
        assert!(result.is_ok());
    }

    #[test]
    fn test_compliance_precompile_set_admin_unauthorized() {
        let mut provider = HashMapStorageProvider::new(1_000_000);

        let mut precompile = CompliancePrecompile;

        let input = IProtocolCompliance::setComplianceAdminCall {
            newAdmin: Address::repeat_byte(0x44),
        }
        .abi_encode();
        let result = precompile.call(&input, Address::repeat_byte(0x99), &mut provider);
        assert!(result.is_err());
    }
}
