//! Validator precompile entry point (0x204).
//!
//! Thin wrapper that routes EVM calls to [`ValidatorStorage`] backed by
//! [`JournalBackend`].  Business logic lives in [`ValidatorStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use crate::ValidatorStorage;
use alloy_sol_types::{sol, SolCall};
use call_asset::AssetStorage;
use call_precompile::{
    dispatch, journal_backend::JournalBackend, require_caller, storage::StorageProvider,
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
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolValidator::stakeCall, _>(
            calldata,
            20000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let backend = JournalBackend::new(storage);
                let mut validator_store = ValidatorStorage::new(backend);
                let mut asset_store = AssetStorage::new(backend);
                validator_store
                    .stake(&mut asset_store, call.pubkey.into(), call.amount, caller)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn unstake(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolValidator::unstakeCall, _>(
            calldata,
            20000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let mut validator_store = ValidatorStorage::new(JournalBackend::new(storage));
                let block_number = storage.block_number();
                validator_store
                    .unstake(call.validatorId, caller, block_number)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn claim_unbonded(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolValidator::claimUnbondedCall, _>(
            calldata,
            15000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                let backend = JournalBackend::new(storage);
                let mut validator_store = ValidatorStorage::new(backend);
                let mut asset_store = AssetStorage::new(backend);
                let block_number = storage.block_number();
                validator_store
                    .claim_unbonded(&mut asset_store, call.validatorId, caller, block_number)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn get_validator_stake(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorStakeCall, _, _>(
            calldata,
            1000,
            storage,
            |call, storage| {
                let store = ValidatorStorage::new(JournalBackend::new(storage));
                Ok(store.read_stake(call.validator))
            },
        )
    }

    fn get_validator_status(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorStatusCall, _, _>(
            calldata,
            1000,
            storage,
            |call, storage| {
                let store = ValidatorStorage::new(JournalBackend::new(storage));
                Ok(U256::from(store.read_status(call.validator)))
            },
        )
    }

    fn get_validator_pubkey(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorPubkeyCall, _, _>(
            calldata,
            1000,
            storage,
            |call, storage| {
                let store = ValidatorStorage::new(JournalBackend::new(storage));
                Ok(store.read_pubkey(call.validator))
            },
        )
    }

    fn get_unbond_height(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getUnbondHeightCall, _, _>(
            calldata,
            1000,
            storage,
            |call, storage| {
                let store = ValidatorStorage::new(JournalBackend::new(storage));
                Ok(store.read_unbond_height(call.validator))
            },
        )
    }

    fn get_validator_by_index(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolValidator::getValidatorByIndexCall, _, _>(
            calldata,
            1000,
            storage,
            |call, storage| {
                let store = ValidatorStorage::new(JournalBackend::new(storage));
                Ok(store.read_validator_by_index(call.index))
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for ValidatorPrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let selector: [u8; 4] = calldata[..4].try_into().unwrap();
        match selector {
            IProtocolValidator::stakeCall::SELECTOR => self.stake(calldata, msg_sender, storage),
            IProtocolValidator::unstakeCall::SELECTOR => {
                self.unstake(calldata, msg_sender, storage)
            }
            IProtocolValidator::claimUnbondedCall::SELECTOR => {
                self.claim_unbonded(calldata, msg_sender, storage)
            }
            IProtocolValidator::getValidatorStakeCall::SELECTOR => {
                self.get_validator_stake(calldata, storage)
            }
            IProtocolValidator::getValidatorStatusCall::SELECTOR => {
                self.get_validator_status(calldata, storage)
            }
            IProtocolValidator::getValidatorPubkeyCall::SELECTOR => {
                self.get_validator_pubkey(calldata, storage)
            }
            IProtocolValidator::getUnbondHeightCall::SELECTOR => {
                self.get_unbond_height(calldata, storage)
            }
            IProtocolValidator::getValidatorByIndexCall::SELECTOR => {
                self.get_validator_by_index(calldata, storage)
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
        provider.sstore(
            call_precompile::ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, sender),
            u128_to_u256(10_000_000),
        );

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
        provider.sstore(
            call_precompile::ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, sender),
            u128_to_u256(10_000_000),
        );

        let mut precompile = ValidatorPrecompile;

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
        provider.sstore(
            call_precompile::ASSET_ADDRESS,
            slot_balance(CALL_ASSET_ID, sender),
            u128_to_u256(10_000_000),
        );

        let mut precompile = ValidatorPrecompile;

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
    }
}
