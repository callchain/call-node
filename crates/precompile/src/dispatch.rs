//! Unified dispatch framework for Callchain precompiles.
//!
//! Provides gas-aware, checkpoint-wrapped helpers that decode ABI calldata
//! via [`alloy_sol_types`] and encode return values automatically.
//!
//! Typical usage in a precompile method:
//! ```ignore
//! fn get_balance(&self, calldata: &[u8], storage: &mut dyn StorageProvider, sr: StorageRef) -> PrecompileResult {
//!     dispatch::view::<IProtocolAsset::getBalanceCall, _, _>(calldata, 800, storage, |call, _storage| {
//!         let mut store = AssetStorage::new(sr);
//!         Ok(store.read_balance(call.assetId, call.account))
//!     })
//! }
//! ```

use alloy_primitives::Bytes;
use alloy_sol_types::{SolCall, SolValue};
use revm_precompile::{PrecompileError, PrecompileOutput, PrecompileResult};

use crate::storage::{fill_precompile_output, StorageProvider};

/// Decode ABI calldata into a typed [`SolCall`].
///
/// `validate = true` checks that the calldata length is an exact multiple
/// of 32 bytes after the selector.
pub fn decode_call<T: SolCall>(calldata: &[u8]) -> Result<T, PrecompileError> {
    T::abi_decode(calldata).map_err(|e| PrecompileError::Other(format!("decode error: {e}").into()))
}

/// Encode a [`SolValue`] return value into ABI bytes.
pub fn encode_return<R: SolValue>(result: R) -> Bytes {
    result.abi_encode().into()
}

/// Execute a **read-only** precompile method.
///
/// 1. Reset storage-operation counters.
/// 2. Decode `calldata` into `T`.
/// 3. Run `handler` (receives the decoded call).
///    Storage operations inside the handler charge gas automatically.
/// 4. Charge the base `gas` for compute work not covered by storage ops.
/// 5. Encode the return value and fill gas accounting.
pub fn view<T, R, F>(
    calldata: &[u8],
    gas: u64,
    storage: &mut dyn StorageProvider,
    handler: F,
) -> PrecompileResult
where
    T: SolCall,
    R: SolValue,
    F: FnOnce(T, &mut dyn StorageProvider) -> Result<R, PrecompileError>,
{
    storage.reset_gas_counters();
    let decoded = decode_call::<T>(calldata)?;
    let result = handler(decoded, storage)?;
    storage.charge_gas(gas)?;
    let output = PrecompileOutput::new(0, encode_return(result));
    Ok(fill_precompile_output(output, storage))
}

/// Execute a **read-only** precompile method with **no return value**.
pub fn view_void<T, F>(
    calldata: &[u8],
    gas: u64,
    storage: &mut dyn StorageProvider,
    handler: F,
) -> PrecompileResult
where
    T: SolCall,
    F: FnOnce(T, &mut dyn StorageProvider) -> Result<(), PrecompileError>,
{
    storage.reset_gas_counters();
    let decoded = decode_call::<T>(calldata)?;
    handler(decoded, storage)?;
    storage.charge_gas(gas)?;
    let output = PrecompileOutput::new(0, Bytes::default());
    Ok(fill_precompile_output(output, storage))
}

/// Execute a **state-mutating** precompile method.
///
/// Same lifecycle as [`view`] but wraps the handler in a `storage.checkpoint()`:
/// - On success the checkpoint is **committed**.
/// - On error (or out-of-gas on base gas) the checkpoint is **reverted**.
pub fn mutate<T, R, F>(
    calldata: &[u8],
    gas: u64,
    storage: &mut dyn StorageProvider,
    handler: F,
) -> PrecompileResult
where
    T: SolCall,
    R: SolValue,
    F: FnOnce(T, &mut dyn StorageProvider) -> Result<R, PrecompileError>,
{
    storage.reset_gas_counters();
    let decoded = decode_call::<T>(calldata)?;
    let checkpoint = storage.checkpoint();
    let result = handler(decoded, storage);
    match result {
        Ok(value) => {
            if storage.charge_gas(gas).is_ok() {
                storage.checkpoint_commit(checkpoint);
                let output = PrecompileOutput::new(0, encode_return(value));
                Ok(fill_precompile_output(output, storage))
            } else {
                storage.checkpoint_revert(checkpoint);
                Err(PrecompileError::OutOfGas)
            }
        }
        Err(e) => {
            storage.checkpoint_revert(checkpoint);
            Err(e)
        }
    }
}

/// Execute a **state-mutating** precompile method with **no return value**.
///
/// Same checkpoint semantics as [`mutate`].
pub fn mutate_void<T, F>(
    calldata: &[u8],
    gas: u64,
    storage: &mut dyn StorageProvider,
    handler: F,
) -> PrecompileResult
where
    T: SolCall,
    F: FnOnce(T, &mut dyn StorageProvider) -> Result<(), PrecompileError>,
{
    storage.reset_gas_counters();
    let decoded = decode_call::<T>(calldata)?;
    let checkpoint = storage.checkpoint();
    let result = handler(decoded, storage);
    match result {
        Ok(()) => {
            if storage.charge_gas(gas).is_ok() {
                storage.checkpoint_commit(checkpoint);
                let output = PrecompileOutput::new(0, Bytes::default());
                Ok(fill_precompile_output(output, storage))
            } else {
                storage.checkpoint_revert(checkpoint);
                Err(PrecompileError::OutOfGas)
            }
        }
        Err(e) => {
            storage.checkpoint_revert(checkpoint);
            Err(e)
        }
    }
}

#[cfg(test)]
mod prop_tests {
    use super::*;
    use crate::storage::HashMapStorageProvider;
    use alloy_primitives::{Address, U256};
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_charge_gas_increases_gas_used(base in 0u64..1_000_000u64) {
            let mut storage = HashMapStorageProvider::new(10_000_000);
            storage.charge_gas(base).unwrap();
            assert_eq!(storage.gas_used(), base, "charge_gas must increase gas_used by exact amount");
        }

        #[test]
        fn prop_storage_auto_charges_on_sload(gas_limit in 1u64..10_000_000u64) {
            let mut storage = HashMapStorageProvider::new(gas_limit);
            let addr = Address::repeat_byte(0x01);
            let key = U256::from(1);
            let before = storage.gas_used();
            // First sload is cold.
            let _ = storage.sload(addr, key);
            assert!(storage.gas_used() > before, "sload must auto-charge gas");
        }
    }
}
