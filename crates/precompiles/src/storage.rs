//! EVM storage abstraction for stateful precompiles.
//!
//! Follows the tempo pattern: precompile business logic accesses EVM storage
//! through [`StorageCtx`] without carrying provider references.
//!
//! - [`StorageProvider`] trait abstracts EVM storage access.
//! - [`EvmStorageProvider`] is the production impl backed by revm's live [`JournalTr`].
//! - [`StorageCtx`] is a thread-local singleton accessed by precompile code.
//! - [`HashMapStorageProvider`] is a test double.

use alloy_primitives::{Address, LogData, U256};
use revm::context_interface::journaled_state::JournalCheckpoint;
use revm::context_interface::JournalTr;
use revm::database_interface::Database;
use revm::primitives::Log;
use revm_precompile::{PrecompileError, PrecompileOutput};
use scoped_tls::scoped_thread_local;
use std::cell::RefCell;
use std::collections::HashMap;

// ── Trait ─────────────────────────────────────────────────────────────

/// Abstracts EVM storage access for precompiles.
///
/// Implemented by a journal-backed provider in production and by a test
/// double in unit tests.
pub trait StorageProvider {
    /// Persistent storage read (SLOAD).
    fn sload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError>;

    /// Persistent storage write (SSTORE).
    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError>;

    /// Transient storage read (TLOAD).
    fn tload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError>;

    /// Transient storage write (TSTORE).
    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError>;

    /// Emit a log event.
    fn emit_event(&mut self, address: Address, event: LogData) -> Result<(), PrecompileError>;

    /// Create a journal checkpoint.
    fn checkpoint(&mut self) -> JournalCheckpoint;

    /// Commit changes since the checkpoint.
    fn checkpoint_commit(&mut self, checkpoint: JournalCheckpoint);

    /// Revert changes since the checkpoint.
    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint);

    /// Deduct gas. Returns `OutOfGas` if insufficient.
    fn deduct_gas(&mut self, gas: u64) -> Result<(), PrecompileError>;

    /// Add gas refund.
    fn refund_gas(&mut self, gas: i64);

    /// Gas consumed so far.
    fn gas_used(&self) -> u64;

    /// Gas refunded so far.
    fn gas_refunded(&self) -> i64;

    /// True if the current call context is static.
    fn is_static(&self) -> bool;

    /// Current chain ID.
    fn chain_id(&self) -> u64;

    /// Current block timestamp.
    fn timestamp(&self) -> U256;

    /// Current block number.
    fn block_number(&self) -> u64;

    /// Current block beneficiary (coinbase).
    fn beneficiary(&self) -> Address;
}

// ── Production: EvmStorageProvider ────────────────────────────────────

/// Production [`StorageProvider`] backed by revm's live journal.
pub struct EvmStorageProvider<'a, J: JournalTr> {
    journal: &'a mut J,
    gas_remaining: u64,
    gas_refunded: i64,
    gas_limit: u64,
    is_static: bool,
    chain_id: u64,
    timestamp: U256,
    block_number: u64,
    beneficiary: Address,
}

impl<'a, J: JournalTr> EvmStorageProvider<'a, J> {
    pub fn new(
        journal: &'a mut J,
        gas_limit: u64,
        is_static: bool,
        chain_id: u64,
        timestamp: U256,
        block_number: u64,
        beneficiary: Address,
    ) -> Self {
        Self {
            journal,
            gas_remaining: gas_limit,
            gas_refunded: 0,
            gas_limit,
            is_static,
            chain_id,
            timestamp,
            block_number,
            beneficiary,
        }
    }
}

impl<'a, J: JournalTr> StorageProvider for EvmStorageProvider<'a, J>
where
    <J::Database as Database>::Error: core::fmt::Debug,
{
    fn sload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        // Ensure account is loaded into journal before accessing storage.
        // Precompiles may read/write arbitrary addresses (e.g. ASSET_ADDRESS)
        // that are not the call target and thus not warmed by revm's frame setup.
        let _ = self.journal.load_account(address)
            .map_err(|e| PrecompileError::Other(format!("load_account error: {:?}", e).into()))?;

        let result = self
            .journal
            .sload(address, key)
            .map_err(|e| PrecompileError::Other(format!("sload error: {:?}", e).into()))?;
        // Cancun: warm = 100, cold = 2100
        let gas = if result.is_cold { 2100 } else { 100 };
        self.deduct_gas(gas)?;
        Ok(result.data)
    }

    fn sstore(
        &mut self,
        address: Address,
        key: U256,
        value: U256,
    ) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        // Ensure account is loaded into journal before accessing storage.
        let _ = self.journal.load_account(address)
            .map_err(|e| PrecompileError::Other(format!("load_account error: {:?}", e).into()))?;

        let result = self
            .journal
            .sstore(address, key, value)
            .map_err(|e| PrecompileError::Other(format!("sstore error: {:?}", e).into()))?;
        // Cancun SSTORE gas accounting (simplified but safe upper bound)
        let static_gas = 20000u64;
        let dynamic_gas = if result.is_cold { 2100 } else { 0 };
        self.deduct_gas(static_gas + dynamic_gas)?;

        // Refunds (Cancun rules, simplified)
        let s = &result.data;
        if s.original_value == value && s.present_value != value {
            // Reset to original value
            self.refund_gas(4800);
        } else if s.present_value != U256::ZERO && value == U256::ZERO {
            // Clearing a slot
            self.refund_gas(4800);
        }
        Ok(())
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?; // TLOAD = warm read
        Ok(self.journal.tload(address, key))
    }

    fn tstore(
        &mut self,
        address: Address,
        key: U256,
        value: U256,
    ) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call cannot mutate state".into()));
        }
        self.deduct_gas(100)?; // TSTORE = warm write
        self.journal.tstore(address, key, value);
        Ok(())
    }

    fn emit_event(
        &mut self,
        address: Address,
        event: LogData,
    ) -> Result<(), PrecompileError> {
        let topics_gas = 375u64 * event.topics().len() as u64;
        let data_gas = 8u64 * event.data.len() as u64;
        self.deduct_gas(375 + topics_gas + data_gas)?;
        self.journal.log(Log {
            address,
            data: event,
        });
        Ok(())
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        self.journal.checkpoint()
    }

    fn checkpoint_commit(&mut self, _checkpoint: JournalCheckpoint) {
        self.journal.checkpoint_commit();
    }

    fn checkpoint_revert(&mut self, checkpoint: JournalCheckpoint) {
        self.journal.checkpoint_revert(checkpoint);
    }

    fn deduct_gas(&mut self, gas: u64) -> Result<(), PrecompileError> {
        self.gas_remaining = self
            .gas_remaining
            .checked_sub(gas)
            .ok_or(PrecompileError::OutOfGas)?;
        Ok(())
    }

    fn refund_gas(&mut self, gas: i64) {
        self.gas_refunded = self.gas_refunded.saturating_add(gas);
    }

    fn gas_used(&self) -> u64 {
        self.gas_limit - self.gas_remaining
    }

    fn gas_refunded(&self) -> i64 {
        self.gas_refunded
    }

    fn is_static(&self) -> bool {
        self.is_static
    }

    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn timestamp(&self) -> U256 {
        self.timestamp
    }

    fn block_number(&self) -> u64 {
        self.block_number
    }

    fn beneficiary(&self) -> Address {
        self.beneficiary
    }
}

// ── Thread-Local Context ──────────────────────────────────────────────

scoped_thread_local!(
    static TL_STORAGE: RefCell<&mut (dyn StorageProvider + 'static)>
);

/// Thread-local storage accessor for precompiles.
///
/// All storage operations must happen within a [`StorageCtx::enter`] closure.
#[derive(Debug, Default, Clone, Copy)]
pub struct StorageCtx;

impl StorageCtx {
    /// Enter a storage context. `provider` must outlive the closure.
    pub fn enter<R>(provider: &mut dyn StorageProvider, f: impl FnOnce() -> R) -> R {
        // SAFETY: scoped_tls ensures the pointer is only accessible within the closure scope.
        let provider_static: &mut (dyn StorageProvider + 'static) =
            unsafe { std::mem::transmute(provider) };
        let cell = RefCell::new(provider_static);
        TL_STORAGE.set(&cell, f)
    }

    // Read operations (do not require mutable access to StorageCtx)

    pub fn sload(address: Address, key: U256) -> Option<U256> {
        Self::with_storage(|s| s.sload(address, key).ok())
    }

    pub fn tload(address: Address, key: U256) -> Option<U256> {
        Self::with_storage(|s| s.tload(address, key).ok())
    }

    pub fn is_static() -> bool {
        Self::with_storage(|s| s.is_static())
    }

    pub fn gas_used() -> u64 {
        Self::with_storage(|s| s.gas_used())
    }

    pub fn gas_refunded() -> i64 {
        Self::with_storage(|s| s.gas_refunded())
    }

    pub fn chain_id() -> u64 {
        Self::with_storage(|s| s.chain_id())
    }

    pub fn timestamp() -> U256 {
        Self::with_storage(|s| s.timestamp())
    }

    pub fn block_number() -> u64 {
        Self::with_storage(|s| s.block_number())
    }

    pub fn beneficiary() -> Address {
        Self::with_storage(|s| s.beneficiary())
    }

    // Write operations

    pub fn sstore(address: Address, key: U256, value: U256) -> Option<()> {
        Self::with_storage(|s| s.sstore(address, key, value).ok())
    }

    pub fn tstore(address: Address, key: U256, value: U256) -> Option<()> {
        Self::with_storage(|s| s.tstore(address, key, value).ok())
    }

    pub fn emit_event(address: Address, event: LogData) -> Option<()> {
        Self::with_storage(|s| s.emit_event(address, event).ok())
    }

    pub fn checkpoint() -> CheckpointGuard {
        CheckpointGuard {
            checkpoint: Some(Self::with_storage(|s| s.checkpoint())),
        }
    }

    pub fn deduct_gas(gas: u64) -> Option<()> {
        Self::with_storage(|s| s.deduct_gas(gas).ok())
    }

    pub fn refund_gas(gas: i64) {
        Self::with_storage(|s| s.refund_gas(gas))
    }

    fn with_storage<F, R>(f: F) -> R
    where
        F: FnOnce(&mut dyn StorageProvider) -> R,
    {
        assert!(
            TL_STORAGE.is_set(),
            "No storage context. StorageCtx::enter must be called first"
        );
        TL_STORAGE.with(|cell| {
            let mut guard = cell.borrow_mut();
            f(&mut **guard)
        })
    }
}

// ── Checkpoint Guard ──────────────────────────────────────────────────

/// RAII guard for atomic state mutation batching.
///
/// On drop, automatically reverts all state changes made since the checkpoint
/// unless [`commit`](Self::commit) was called.
pub struct CheckpointGuard {
    checkpoint: Option<JournalCheckpoint>,
}

impl CheckpointGuard {
    /// Commits all state changes since the checkpoint.
    pub fn commit(mut self) {
        if let Some(cp) = self.checkpoint.take() {
            StorageCtx::with_storage(|s| s.checkpoint_commit(cp));
        }
    }
}

impl Drop for CheckpointGuard {
    fn drop(&mut self) {
        if let Some(cp) = self.checkpoint.take() {
            StorageCtx::with_storage(|s| s.checkpoint_revert(cp));
        }
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Gas cost for decoding calldata (per 32-byte word).
pub const INPUT_PER_WORD_COST: u64 = 6;

/// Compute gas cost for calldata of the given length.
pub fn input_cost(len: usize) -> u64 {
    len.div_ceil(32).saturating_mul(INPUT_PER_WORD_COST as usize) as u64
}

/// Compute a storage slot from multiple concatenated byte slices.
pub fn storage_slot(parts: &[&[u8]]) -> U256 {
    let mut hasher = alloy_primitives::Keccak256::new();
    for part in parts {
        hasher.update(part);
    }
    U256::from_be_slice(hasher.finalize().as_slice())
}

/// Fill gas accounting on a [`PrecompileOutput`] from the current [`StorageCtx`].
pub fn fill_precompile_output(mut output: PrecompileOutput) -> PrecompileOutput {
    output.gas_used = StorageCtx::gas_used();
    if !output.reverted {
        output.gas_refunded = StorageCtx::gas_refunded();
    }
    output
}

// ── Test Double: HashMapStorageProvider ───────────────────────────────

#[derive(Debug, Default)]
pub struct HashMapStorageProvider {
    persistent: HashMap<(Address, U256), U256>,
    transient: HashMap<(Address, U256), U256>,
    events: HashMap<Address, Vec<LogData>>,
    gas_remaining: u64,
    gas_refunded: i64,
    gas_limit: u64,
    is_static: bool,
    chain_id: u64,
    timestamp: U256,
    block_number: u64,
    beneficiary: Address,
    // Checkpoint stack for revert semantics
    checkpoints: Vec<(
        HashMap<(Address, U256), U256>,
        HashMap<(Address, U256), U256>,
        HashMap<Address, Vec<LogData>>,
        u64,
        i64,
    )>,
}

impl HashMapStorageProvider {
    pub fn new(gas_limit: u64) -> Self {
        Self {
            gas_limit,
            gas_remaining: gas_limit,
            ..Default::default()
        }
    }

    pub fn with_block(gas_limit: u64, chain_id: u64, block_number: u64) -> Self {
        Self {
            gas_limit,
            gas_remaining: gas_limit,
            chain_id,
            block_number,
            ..Default::default()
        }
    }

    pub fn get(&self, address: Address, key: U256) -> Option<U256> {
        self.persistent.get(&(address, key)).copied()
    }

    pub fn set(&mut self, address: Address, key: U256, value: U256) {
        self.persistent.insert((address, key), value);
    }

    pub fn events(&self, address: Address) -> Vec<LogData> {
        self.events.get(&address).cloned().unwrap_or_default()
    }
}

impl StorageProvider for HashMapStorageProvider {
    fn sload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?; // always warm in tests
        Ok(self.persistent.get(&(address, key)).copied().unwrap_or_default())
    }

    fn sstore(
        &mut self,
        address: Address,
        key: U256,
        value: U256,
    ) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call".into()));
        }
        self.deduct_gas(20000)?;
        self.persistent.insert((address, key), value);
        Ok(())
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?;
        Ok(self.transient.get(&(address, key)).copied().unwrap_or_default())
    }

    fn tstore(
        &mut self,
        address: Address,
        key: U256,
        value: U256,
    ) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call".into()));
        }
        self.deduct_gas(100)?;
        self.transient.insert((address, key), value);
        Ok(())
    }

    fn emit_event(
        &mut self,
        address: Address,
        event: LogData,
    ) -> Result<(), PrecompileError> {
        let topics_gas = 375u64 * event.topics().len() as u64;
        let data_gas = 8u64 * event.data.len() as u64;
        self.deduct_gas(375 + topics_gas + data_gas)?;
        self.events.entry(address).or_default().push(event);
        Ok(())
    }

    fn checkpoint(&mut self) -> JournalCheckpoint {
        self.checkpoints.push((
            self.persistent.clone(),
            self.transient.clone(),
            self.events.clone(),
            self.gas_remaining,
            self.gas_refunded,
        ));
        JournalCheckpoint::default()
    }

    fn checkpoint_commit(&mut self, _checkpoint: JournalCheckpoint) {
        self.checkpoints.pop();
    }

    fn checkpoint_revert(&mut self, _checkpoint: JournalCheckpoint) {
        if let Some((persistent, transient, events, gas_remaining, gas_refunded)) =
            self.checkpoints.pop()
        {
            self.persistent = persistent;
            self.transient = transient;
            self.events = events;
            self.gas_remaining = gas_remaining;
            self.gas_refunded = gas_refunded;
        }
    }

    fn deduct_gas(&mut self, gas: u64) -> Result<(), PrecompileError> {
        self.gas_remaining = self
            .gas_remaining
            .checked_sub(gas)
            .ok_or(PrecompileError::OutOfGas)?;
        Ok(())
    }

    fn refund_gas(&mut self, gas: i64) {
        self.gas_refunded = self.gas_refunded.saturating_add(gas);
    }

    fn gas_used(&self) -> u64 {
        self.gas_limit - self.gas_remaining
    }

    fn gas_refunded(&self) -> i64 {
        self.gas_refunded
    }

    fn is_static(&self) -> bool {
        self.is_static
    }

    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn timestamp(&self) -> U256 {
        self.timestamp
    }

    fn block_number(&self) -> u64 {
        self.block_number
    }

    fn beneficiary(&self) -> Address {
        self.beneficiary
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use alloy_primitives::keccak256;

    #[test]
    fn test_hashmap_storage_provider() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::repeat_byte(0x01);
        let key = U256::from(42);

        StorageCtx::enter(&mut provider, || {
            StorageCtx::sstore(addr, key, U256::from(100));
            let val = StorageCtx::sload(addr, key).unwrap();
            assert_eq!(val, U256::from(100));
        });
    }

    #[test]
    fn test_checkpoint_guard_commit() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::ZERO;
        let key = U256::from(1);

        StorageCtx::enter(&mut provider, || {
            StorageCtx::sstore(addr, key, U256::from(42));
            let guard = StorageCtx::checkpoint();
            StorageCtx::sstore(addr, key, U256::from(99));
            guard.commit();
            assert_eq!(StorageCtx::sload(addr, key).unwrap(), U256::from(99));
        });
    }

    #[test]
    fn test_checkpoint_guard_revert() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::ZERO;
        let key = U256::from(1);

        StorageCtx::enter(&mut provider, || {
            StorageCtx::sstore(addr, key, U256::from(42));
            {
                let _guard = StorageCtx::checkpoint();
                StorageCtx::sstore(addr, key, U256::from(99));
            }
            // reverted
            assert_eq!(StorageCtx::sload(addr, key).unwrap(), U256::from(42));
        });
    }

    #[test]
    fn test_storage_slot() {
        let slot = storage_slot(&[b"validators"]);
        assert_eq!(slot, U256::from_be_slice(keccak256(b"validators").as_slice()));
    }

    #[test]
    fn test_input_cost() {
        assert_eq!(input_cost(0), 0);
        assert_eq!(input_cost(32), 6);
        assert_eq!(input_cost(64), 12);
        assert_eq!(input_cost(33), 12);
    }
}
