//! Unified dispatch framework for Callchain precompiles.
//!
//! Provides gas-aware, checkpoint-wrapped helpers that decode ABI calldata
//! via [`alloy_sol_types`] and encode return values automatically.
//!
//! Typical usage in a precompile method:
//! ```ignore
//! fn get_balance(&self, calldata: &[u8], storage: &mut dyn StorageProvider) -> PrecompileResult {
//!     dispatch::view::<IProtocolAsset::getBalanceCall, _, _>(calldata, 800, storage, |call, storage| {
//!         let store = AssetStorage::new(JournalBackend::new(storage));
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
/// 1. Deduct `gas` via `storage`.
/// 2. Decode `calldata` into `T`.
/// 3. Run `handler` (receives the decoded call).
/// 4. Encode the return value.
/// 5. Fill gas accounting into the output.
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
    storage.deduct_gas(gas)?;
    let decoded = decode_call::<T>(calldata)?;
    let result = handler(decoded, storage)?;
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
    storage.deduct_gas(gas)?;
    let decoded = decode_call::<T>(calldata)?;
    handler(decoded, storage)?;
    let output = PrecompileOutput::new(0, Bytes::default());
    Ok(fill_precompile_output(output, storage))
}

/// Execute a **state-mutating** precompile method.
///
/// Same as [`view`] but wraps the handler in a `storage.checkpoint()`:
/// - On success the checkpoint is **committed**.
/// - On error the checkpoint is **reverted**.
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
    storage.deduct_gas(gas)?;
    let decoded = decode_call::<T>(calldata)?;
    let checkpoint = storage.checkpoint();
    let result = handler(decoded, storage);
    match result {
        Ok(value) => {
            storage.checkpoint_commit(checkpoint);
            let output = PrecompileOutput::new(0, encode_return(value));
            Ok(fill_precompile_output(output, storage))
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
    storage.deduct_gas(gas)?;
    let decoded = decode_call::<T>(calldata)?;
    let checkpoint = storage.checkpoint();
    let result = handler(decoded, storage);
    match result {
        Ok(()) => {
            storage.checkpoint_commit(checkpoint);
            let output = PrecompileOutput::new(0, Bytes::default());
            Ok(fill_precompile_output(output, storage))
        }
        Err(e) => {
            storage.checkpoint_revert(checkpoint);
            Err(e)
        }
    }
}
