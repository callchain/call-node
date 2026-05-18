//! Validator precompile entry point (0x204).
//!
//! Thin wrapper that routes EVM calls to [`ValidatorStorage`] backed by
//! [`StorageProvider`].  Business logic lives in [`ValidatorStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use crate::ValidatorStorage;
use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompile::{
    address_to_u256, check_compliance, dispatch, require_caller, storage::StorageProvider,
    u128_to_u256, u64_to_u256, StorageRef, VALIDATOR_ADDRESS,
};
use call_primitives::{Address, U256};
use revm_precompile::{PrecompileError, PrecompileResult};

sol! {
    interface IProtocolValidator {
        function stake(bytes32 pubkey, uint128 amount) external;
        function unstake(uint64 validatorId) external;
        function claimUnbonded(uint64 validatorId) external;
        function getValidatorStake(address validator) external view returns (uint128 stake);
        function getValidatorStatus(address validator) external view returns (uint8 status);
        function getValidatorPubkey(address validator) external view returns (bytes32 pubkey);
        function getUnbondHeight(address validator) external view returns (uint64 height);
        function getValidatorByIndex(uint64 index) external view returns (address validator);
        function getValidatorCount() external view returns (uint64 count);
        function getActiveValidatorCount() external view returns (uint64 count);
    }
}

/// Stateful validator precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct ValidatorPrecompile;

impl ValidatorPrecompile {
    fn stake(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolValidator::stakeCall, _>(
            calldata,
            20000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                check_compliance(caller, storage)?;
                let mut validator_store = ValidatorStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                validator_store
                    .stake(&mut asset_store, call.pubkey.into(), call.amount, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let validator_id = validator_store.read_validator_id(caller);
                let topic0 =
                    alloy_primitives::keccak256(b"Staked(address,uint64,bytes32,uint128)");
                let topic1 = alloy_primitives::B256::from(
                    address_to_u256(caller).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(96);
                data.extend_from_slice(&u64_to_u256(validator_id).to_be_bytes::<32>());
                data.extend_from_slice(call.pubkey.as_ref());
                data.extend_from_slice(&u128_to_u256(call.amount).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1],
                    alloy_primitives::Bytes::from(data),
                ) {
                    let _ = storage.emit_event(VALIDATOR_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn unstake(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolValidator::unstakeCall, _>(
            calldata,
            20000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut validator_store = ValidatorStorage::new(sr);
                let stake = validator_store.read_stake(caller);
                let block_number = storage.block_number();
                validator_store
                    .unstake(call.validatorId, caller, block_number)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let topic0 =
                    alloy_primitives::keccak256(b"Unstaked(address,uint64,uint128,uint64)");
                let topic1 = alloy_primitives::B256::from(
                    address_to_u256(caller).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(96);
                data.extend_from_slice(&u64_to_u256(call.validatorId).to_be_bytes::<32>());
                data.extend_from_slice(&u128_to_u256(stake).to_be_bytes::<32>());
                data.extend_from_slice(&u64_to_u256(block_number).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1],
                    alloy_primitives::Bytes::from(data),
                ) {
                    let _ = storage.emit_event(VALIDATOR_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn claim_unbonded(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolValidator::claimUnbondedCall, _>(
            calldata,
            15000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                check_compliance(caller, storage)?;
                let mut validator_store = ValidatorStorage::new(sr);
                let mut asset_store = AssetStorage::new(sr);
                let block_number = storage.block_number();
                let amount = validator_store
                    .claim_unbonded(&mut asset_store, call.validatorId, caller, block_number)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let topic0 = alloy_primitives::keccak256(b"Claimed(address,uint64,uint128)");
                let topic1 = alloy_primitives::B256::from(
                    address_to_u256(caller).to_be_bytes::<32>(),
                );
                let mut data = Vec::with_capacity(64);
                data.extend_from_slice(&u64_to_u256(call.validatorId).to_be_bytes::<32>());
                data.extend_from_slice(&u128_to_u256(amount).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1],
                    alloy_primitives::Bytes::from(data),
                ) {
                    let _ = storage.emit_event(VALIDATOR_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn get_validator_stake(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorStakeCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = ValidatorStorage::new(sr);
                Ok(store.read_stake(call.validator))
            },
        )
    }

    fn get_validator_status(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorStatusCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = ValidatorStorage::new(sr);
                Ok(U256::from(store.read_status(call.validator)))
            },
        )
    }

    fn get_validator_pubkey(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorPubkeyCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = ValidatorStorage::new(sr);
                Ok(store.read_pubkey(call.validator))
            },
        )
    }

    fn get_unbond_height(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getUnbondHeightCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = ValidatorStorage::new(sr);
                Ok(store.read_unbond_height(call.validator))
            },
        )
    }

    fn get_validator_by_index(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorByIndexCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = ValidatorStorage::new(sr);
                let count = store.read_validator_count();
                if call.index == 0 || call.index > count {
                    return Err(PrecompileError::Other(
                        "validator: index out of range".into(),
                    ));
                }
                Ok(store.read_validator_by_index(call.index))
            },
        )
    }

    fn get_validator_count(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorCountCall, _, _>(
            calldata,
            500,
            storage,
            |_call, _storage| {
                let mut store = ValidatorStorage::new(sr);
                Ok(u64_to_u256(store.read_validator_count()))
            },
        )
    }

    fn get_active_validator_count(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getActiveValidatorCountCall, _, _>(
            calldata,
            500,
            storage,
            |_call, _storage| {
                let mut store = ValidatorStorage::new(sr);
                Ok(u64_to_u256(store.read_active_validator_count()))
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for ValidatorPrecompile {
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
        let selector: [u8; 4] = calldata[..4].try_into().unwrap_or([0u8; 4]);
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolValidator::stakeCall::SELECTOR => {
                self.stake(calldata, msg_sender, storage, sr)
            }
            IProtocolValidator::unstakeCall::SELECTOR => {
                self.unstake(calldata, msg_sender, storage, sr)
            }
            IProtocolValidator::claimUnbondedCall::SELECTOR => {
                self.claim_unbonded(calldata, msg_sender, storage, sr)
            }
            IProtocolValidator::getValidatorStakeCall::SELECTOR => {
                self.get_validator_stake(calldata, storage, sr)
            }
            IProtocolValidator::getValidatorStatusCall::SELECTOR => {
                self.get_validator_status(calldata, storage, sr)
            }
            IProtocolValidator::getValidatorPubkeyCall::SELECTOR => {
                self.get_validator_pubkey(calldata, storage, sr)
            }
            IProtocolValidator::getUnbondHeightCall::SELECTOR => {
                self.get_unbond_height(calldata, storage, sr)
            }
            IProtocolValidator::getValidatorByIndexCall::SELECTOR => {
                self.get_validator_by_index(calldata, storage, sr)
            }
            IProtocolValidator::getValidatorCountCall::SELECTOR => {
                self.get_validator_count(calldata, storage, sr)
            }
            IProtocolValidator::getActiveValidatorCountCall::SELECTOR => {
                self.get_active_validator_count(calldata, storage, sr)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CALL_ASSET_ID;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{
        slot_balance, u128_to_u256, u256_to_u128, u256_to_u64, StatefulPrecompile,
        VALIDATOR_ADDRESS,
    };
    use call_primitives::Address;

    #[test]
    fn test_validator_address() {
        assert_eq!(
            VALIDATOR_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000204")
        );
    }

    #[test]
    fn test_validator_precompile_stake_and_get() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x11);

        // Seed sender balance
        provider
            .sstore(
                call_precompile::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, sender),
                u128_to_u256(10_000_000),
            )
            .unwrap();

        let mut precompile = ValidatorPrecompile;

        // stake(pubkey, amount=5_000_000)
        let input = IProtocolValidator::stakeCall {
            pubkey: [0xAAu8; 32].into(),
            amount: 5_000_000,
        }
        .abi_encode();

        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "stake failed: {:?}", result.err());

        // getValidatorStake(sender)
        let input = IProtocolValidator::getValidatorStakeCall { validator: sender }.abi_encode();

        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let stake = u256_to_u128(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(stake, 5_000_000);

        // getValidatorStatus(sender)
        let input = IProtocolValidator::getValidatorStatusCall { validator: sender }.abi_encode();

        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1); // active
    }

    #[test]
    fn test_validator_precompile_unstake_and_claim() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x11);

        // Seed sender balance
        provider
            .sstore(
                call_precompile::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, sender),
                u128_to_u256(10_000_000),
            )
            .unwrap();

        let mut precompile = ValidatorPrecompile;

        // Disable safety floor for this test (single validator)
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                crate::slot_safety_floor(),
                alloy_primitives::U256::ZERO,
            )
            .unwrap();

        // stake first
        let input = IProtocolValidator::stakeCall {
            pubkey: [0xAAu8; 32].into(),
            amount: 5_000_000,
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // unstake(validatorId=1)
        let input = IProtocolValidator::unstakeCall { validatorId: 1 }.abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "unstake failed: {:?}", result.err());

        // status should be 2 (unbonding)
        let input = IProtocolValidator::getValidatorStatusCall { validator: sender }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 2);

        // claimUnbonded should fail: period not elapsed
        let input = IProtocolValidator::claimUnbondedCall { validatorId: 1 }.abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_err(), "claim should fail before period elapsed");
    }

    #[test]
    fn test_validator_precompile_read_methods() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x11);

        // Seed sender balance
        provider
            .sstore(
                call_precompile::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, sender),
                u128_to_u256(10_000_000),
            )
            .unwrap();

        let mut precompile = ValidatorPrecompile;

        // Disable safety floor for this test (single validator)
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                crate::slot_safety_floor(),
                alloy_primitives::U256::ZERO,
            )
            .unwrap();

        // stake first
        let input = IProtocolValidator::stakeCall {
            pubkey: [0xAAu8; 32].into(),
            amount: 5_000_000,
        }
        .abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // getValidatorPubkey(sender)
        let input = IProtocolValidator::getValidatorPubkeyCall { validator: sender }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(&result.bytes[..], &[0xAAu8; 32]);

        // getValidatorByIndex(1)
        let input = IProtocolValidator::getValidatorByIndexCall { index: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let returned_addr = Address::from_slice(&result.bytes[12..32]);
        assert_eq!(returned_addr, sender);

        // unstake to set unbond height
        let input = IProtocolValidator::unstakeCall { validatorId: 1 }.abi_encode();
        precompile.call(&input, sender, &mut provider).unwrap();

        // getUnbondHeight(sender)
        let input = IProtocolValidator::getUnbondHeightCall { validator: sender }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let height = u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(height, 0); // block_number defaults to 0 in test provider

        // getValidatorCount() and getActiveValidatorCount()
        let input = IProtocolValidator::getValidatorCountCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let count = u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(count, 1);

        let input = IProtocolValidator::getActiveValidatorCountCall {}.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let active = u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(active, 0); // unstaked, so active count is 0
    }

    #[test]
    fn test_validator_precompile_count_views() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let v1 = Address::repeat_byte(0x11);
        let v2 = Address::repeat_byte(0x22);
        provider
            .sstore(
                call_precompile::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, v1),
                u128_to_u256(10_000_000),
            )
            .unwrap();
        provider
            .sstore(
                call_precompile::ASSET_ADDRESS,
                slot_balance(CALL_ASSET_ID, v2),
                u128_to_u256(10_000_000),
            )
            .unwrap();

        let mut precompile = ValidatorPrecompile;

        // Disable safety floor
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                crate::slot_safety_floor(),
                alloy_primitives::U256::ZERO,
            )
            .unwrap();

        // Initially both counts are 0
        let input = IProtocolValidator::getValidatorCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        )), 0);

        let input = IProtocolValidator::getActiveValidatorCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        )), 0);

        // Stake v1
        let input = IProtocolValidator::stakeCall {
            pubkey: [0xAAu8; 32].into(),
            amount: 5_000_000,
        }.abi_encode();
        precompile.call(&input, v1, &mut provider).unwrap();

        let input = IProtocolValidator::getValidatorCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        )), 1);

        let input = IProtocolValidator::getActiveValidatorCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        )), 1);

        // Stake v2
        let input = IProtocolValidator::stakeCall {
            pubkey: [0xBBu8; 32].into(),
            amount: 5_000_000,
        }.abi_encode();
        precompile.call(&input, v2, &mut provider).unwrap();

        let input = IProtocolValidator::getValidatorCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        )), 2);

        let input = IProtocolValidator::getActiveValidatorCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        )), 2);

        // Unstake v1 (active drops to 1, count stays 2)
        let input = IProtocolValidator::unstakeCall { validatorId: 1 }.abi_encode();
        precompile.call(&input, v1, &mut provider).unwrap();

        let input = IProtocolValidator::getValidatorCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        )), 2);

        let input = IProtocolValidator::getActiveValidatorCountCall {}.abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider).unwrap();
        assert_eq!(u256_to_u64(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        )), 1);
    }
}
