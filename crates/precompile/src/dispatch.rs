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

/// Extra gas charged per SLOAD observed during a precompile call.
const SLOAD_DISPATCH_COST: u64 = 50;
/// Extra gas charged per SSTORE observed during a precompile call.
const SSTORE_DISPATCH_COST: u64 = 500;

/// Compute dynamic overhead from base gas + observed storage ops.
fn calculate_overhead(base_gas: u64, sloads: u64, sstores: u64) -> u64 {
    base_gas
        .saturating_add(sloads.saturating_mul(SLOAD_DISPATCH_COST))
        .saturating_add(sstores.saturating_mul(SSTORE_DISPATCH_COST))
}

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
/// 4. Compute dynamic overhead from `gas` + observed storage ops.
/// 5. Deduct overhead; on failure the call is treated as out-of-gas.
/// 6. Encode the return value and fill gas accounting.
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
    let (sloads, sstores) = storage.gas_counters();
    let overhead = calculate_overhead(gas, sloads, sstores);
    storage.deduct_gas(overhead)?;
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
    let (sloads, sstores) = storage.gas_counters();
    let overhead = calculate_overhead(gas, sloads, sstores);
    storage.deduct_gas(overhead)?;
    let output = PrecompileOutput::new(0, Bytes::default());
    Ok(fill_precompile_output(output, storage))
}

/// Execute a **state-mutating** precompile method.
///
/// Same lifecycle as [`view`] but wraps the handler in a `storage.checkpoint()`:
/// - On success the checkpoint is **committed**.
/// - On error (or out-of-gas on overhead) the checkpoint is **reverted**.
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
    let (sloads, sstores) = storage.gas_counters();
    let overhead = calculate_overhead(gas, sloads, sstores);
    match result {
        Ok(value) => {
            if storage.deduct_gas(overhead).is_ok() {
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
    let (sloads, sstores) = storage.gas_counters();
    let overhead = calculate_overhead(gas, sloads, sstores);
    match result {
        Ok(()) => {
            if storage.deduct_gas(overhead).is_ok() {
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
    use super::calculate_overhead;
    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_overhead_zero_ops_equals_base(base in 0u64..10_000_000u64) {
            assert_eq!(calculate_overhead(base, 0, 0), base);
        }

        #[test]
        fn prop_overhead_monotonic_sloads(
            base in 0u64..1_000_000u64,
            sloads_a in 0u64..1_000_000u64,
            sloads_b in 0u64..1_000_000u64,
            sstores in 0u64..100_000u64
        ) {
            let result_a = calculate_overhead(base, sloads_a, sstores);
            let result_b = calculate_overhead(base, sloads_b, sstores);
            if sloads_a <= sloads_b {
                assert!(result_a <= result_b, "overhead must be monotonic in sloads");
            } else {
                assert!(result_a >= result_b, "overhead must be monotonic in sloads");
            }
        }

        #[test]
        fn prop_overhead_monotonic_sstores(
            base in 0u64..1_000_000u64,
            sloads in 0u64..1_000_000u64,
            sstores_a in 0u64..100_000u64,
            sstores_b in 0u64..100_000u64
        ) {
            let result_a = calculate_overhead(base, sloads, sstores_a);
            let result_b = calculate_overhead(base, sloads, sstores_b);
            if sstores_a <= sstores_b {
                assert!(result_a <= result_b, "overhead must be monotonic in sstores");
            } else {
                assert!(result_a >= result_b, "overhead must be monotonic in sstores");
            }
        }

        #[test]
        fn prop_overhead_never_underflows(
            base in 0u64..u64::MAX,
            sloads in 0u64..u64::MAX,
            sstores in 0u64..u64::MAX
        ) {
            let result = calculate_overhead(base, sloads, sstores);
            assert!(result >= base || result == u64::MAX, "saturating add should never go below base unless overflowed to MAX");
        }

        #[test]
        fn prop_overhead_components_additive(
            base in 0u64..100_000u64,
            sloads in 0u64..10_000u64,
            sstores in 0u64..1_000u64
        ) {
            // For values that don't saturate, overhead should equal base + 50*sloads + 500*sstores
            let expected = base + sloads * 50 + sstores * 500;
            let result = calculate_overhead(base, sloads, sstores);
            assert_eq!(result, expected, "non-saturating inputs must produce exact sum");
        }
    }
}
