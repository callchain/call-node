//! Unified dispatch framework for Callchain precompiles.
//!
//! Provides gas-aware, checkpoint-wrapped helpers that decode ABI calldata
//! via [`alloy_sol_types`] and encode return values automatically.
//!
//! Typical usage in a precompile method:
//! ```ignore
//! fn get_balance(&self, calldata: &[u8]) -> PrecompileResult {
//!     dispatch::view::<IProtocolAsset::getBalanceCall, _, _>(calldata, 800, |call| {
//!         let store = AssetStorage::new(JournalBackend);
//!         Ok(store.read_balance(call.assetId, call.account))
//!     })
//! }
//! ```

use alloy_primitives::Bytes;
use alloy_sol_types::{SolCall, SolValue};
use revm_precompile::{PrecompileError, PrecompileOutput, PrecompileResult};

/// Decode ABI calldata into a typed [`SolCall`].
///
/// `validate = true` checks that the calldata length is an exact multiple
/// of 32 bytes after the selector.
pub fn decode_call<T: SolCall>(calldata: &[u8]) -> Result<T, PrecompileError> {
    T::abi_decode(calldata)
        .map_err(|e| PrecompileError::Other(format!("decode error: {e}").into()))
}

/// Encode a [`SolValue`] return value into ABI bytes.
pub fn encode_return<R: SolValue>(result: R) -> Bytes {
    result.abi_encode().into()
}

/// Execute a **read-only** precompile method.
///
/// 1. Deduct `gas` via [`StorageCtx`].
/// 2. Decode `calldata` into `T`.
/// 3. Run `handler`.
/// 4. Encode the return value.
/// 5. Fill gas accounting into the output.
pub fn view<T, R, F>(calldata: &[u8], gas: u64, handler: F) -> PrecompileResult
where
    T: SolCall,
    R: SolValue,
    F: FnOnce(T) -> Result<R, PrecompileError>,
{
    crate::storage::StorageCtx::deduct_gas(gas)
        .ok_or(PrecompileError::OutOfGas)?;
    let decoded = decode_call::<T>(calldata)?;
    let result = handler(decoded)?;
    let output = PrecompileOutput::new(0, encode_return(result));
    Ok(crate::storage::fill_precompile_output(output))
}

/// Execute a **read-only** precompile method with **no return value**.
pub fn view_void<T, F>(calldata: &[u8], gas: u64, handler: F) -> PrecompileResult
where
    T: SolCall,
    F: FnOnce(T) -> Result<(), PrecompileError>,
{
    crate::storage::StorageCtx::deduct_gas(gas)
        .ok_or(PrecompileError::OutOfGas)?;
    let decoded = decode_call::<T>(calldata)?;
    handler(decoded)?;
    let output = PrecompileOutput::new(0, Bytes::default());
    Ok(crate::storage::fill_precompile_output(output))
}

/// Execute a **state-mutating** precompile method.
///
/// Same as [`view`] but wraps the handler in a [`StorageCtx::checkpoint`]:
/// - On success the checkpoint is **committed**.
/// - On error the guard drops and all state changes are **reverted**.
pub fn mutate<T, R, F>(calldata: &[u8], gas: u64, handler: F) -> PrecompileResult
where
    T: SolCall,
    R: SolValue,
    F: FnOnce(T) -> Result<R, PrecompileError>,
{
    crate::storage::StorageCtx::deduct_gas(gas)
        .ok_or(PrecompileError::OutOfGas)?;
    let decoded = decode_call::<T>(calldata)?;
    let guard = crate::storage::StorageCtx::checkpoint();
    let result = handler(decoded)?;
    guard.commit();
    let output = PrecompileOutput::new(0, encode_return(result));
    Ok(crate::storage::fill_precompile_output(output))
}

/// Execute a **state-mutating** precompile method with **no return value**.
///
/// Same checkpoint semantics as [`mutate`].
pub fn mutate_void<T, F>(calldata: &[u8], gas: u64, handler: F) -> PrecompileResult
where
    T: SolCall,
    F: FnOnce(T) -> Result<(), PrecompileError>,
{
    crate::storage::StorageCtx::deduct_gas(gas)
        .ok_or(PrecompileError::OutOfGas)?;
    let decoded = decode_call::<T>(calldata)?;
    let guard = crate::storage::StorageCtx::checkpoint();
    handler(decoded)?;
    guard.commit();
    let output = PrecompileOutput::new(0, Bytes::default());
    Ok(crate::storage::fill_precompile_output(output))
}
