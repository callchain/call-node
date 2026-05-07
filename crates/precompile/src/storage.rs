//! EVM storage abstraction for stateful precompiles.
//!
//! Precompile business logic accesses EVM storage through the [`StorageProvider`]
//! trait, which is passed explicitly via the [`StatefulPrecompile::call`] method.
//!
//! - [`StorageProvider`] trait abstracts EVM storage access.
//! - [`EvmStorageProvider`] is the production impl backed by revm's live [`JournalTr`].
//! - [`HashMapStorageProvider`] is a test double.

use alloy_primitives::{Address, LogData, U256};
use revm::context_interface::journaled_state::account::JournaledAccountTr;
use revm::context_interface::journaled_state::JournalCheckpoint;
use revm::context_interface::JournalTr;
use revm::database_interface::Database;
use revm::primitives::Log;
use revm_precompile::{PrecompileError, PrecompileOutput};
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

    /// Add to the native EVM balance of an address.
    fn balance_add(&mut self, address: Address, amount: U256) -> Result<(), PrecompileError>;

    /// Subtract from the native EVM balance of an address.
    fn balance_sub(&mut self, address: Address, amount: U256) -> Result<(), PrecompileError>;

    /// Read the native EVM balance of an address.
    fn balance_get(&mut self, address: Address) -> Result<U256, PrecompileError>;
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
        let _ = self
            .journal
            .load_account(address)
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

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other(
                "static call cannot mutate state".into(),
            ));
        }
        let _ = self
            .journal
            .load_account(address)
            .map_err(|e| PrecompileError::Other(format!("load_account error: {:?}", e).into()))?;

        let result = self
            .journal
            .sstore(address, key, value)
            .map_err(|e| PrecompileError::Other(format!("sstore error: {:?}", e).into()))?;
        // Cancun SSTORE gas accounting (simplified but safe upper bound)
        let static_gas = 20000u64;
        let dynamic_gas = if result.is_cold { 2100 } else { 0 };
        let total = static_gas + dynamic_gas;
        self.deduct_gas(total)?;

        // Refunds (Cancun rules, simplified)
        let s = &result.data;
        if (s.original_value == value && s.present_value != value)
            || (s.present_value != U256::ZERO && value == U256::ZERO)
        {
            self.refund_gas(4800);
        }
        Ok(())
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?; // TLOAD = warm read
        Ok(self.journal.tload(address, key))
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other(
                "static call cannot mutate state".into(),
            ));
        }
        self.deduct_gas(100)?; // TSTORE = warm write
        self.journal.tstore(address, key, value);
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<(), PrecompileError> {
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

    fn balance_add(&mut self, address: Address, amount: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other(
                "static call cannot mutate balance".into(),
            ));
        }
        let success = {
            let mut account = self.journal.load_account_mut(address).map_err(|e| {
                PrecompileError::Other(format!("load_account_mut error: {:?}", e).into())
            })?;
            account.data.incr_balance(amount)
        };
        if !success {
            return Err(PrecompileError::Other("balance overflow".into()));
        }
        self.deduct_gas(100)
    }

    fn balance_sub(&mut self, address: Address, amount: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other(
                "static call cannot mutate balance".into(),
            ));
        }
        let success = {
            let mut account = self.journal.load_account_mut(address).map_err(|e| {
                PrecompileError::Other(format!("load_account_mut error: {:?}", e).into())
            })?;
            account.data.decr_balance(amount)
        };
        if !success {
            return Err(PrecompileError::Other("insufficient native balance".into()));
        }
        self.deduct_gas(100)
    }

    fn balance_get(&mut self, address: Address) -> Result<U256, PrecompileError> {
        let balance = {
            let account = self.journal.load_account(address).map_err(|e| {
                PrecompileError::Other(format!("load_account error: {:?}", e).into())
            })?;
            account.data.info.balance
        };
        self.deduct_gas(100)?;
        Ok(balance)
    }
}

// ── Helpers ───────────────────────────────────────────────────────────

/// Gas cost for decoding calldata (per 32-byte word).
pub const INPUT_PER_WORD_COST: u64 = 6;

/// Compute gas cost for calldata of the given length.
pub fn input_cost(len: usize) -> u64 {
    len.div_ceil(32)
        .saturating_mul(INPUT_PER_WORD_COST as usize) as u64
}

/// Compute a storage slot from multiple concatenated byte slices.
pub fn storage_slot(parts: &[&[u8]]) -> U256 {
    let mut hasher = alloy_primitives::Keccak256::new();
    for part in parts {
        hasher.update(part);
    }
    U256::from_be_slice(hasher.finalize().as_slice())
}

/// Fill gas accounting on a [`PrecompileOutput`] from a [`StorageProvider`].
pub fn fill_precompile_output(
    mut output: PrecompileOutput,
    storage: &dyn StorageProvider,
) -> PrecompileOutput {
    output.gas_used = storage.gas_used();
    if !output.reverted {
        output.gas_refunded = storage.gas_refunded();
    }
    output
}

// ── Test Double: HashMapStorageProvider ───────────────────────────────

#[derive(Debug, Default)]
#[allow(clippy::type_complexity)]
pub struct HashMapStorageProvider {
    persistent: HashMap<(Address, U256), U256>,
    transient: HashMap<(Address, U256), U256>,
    events: HashMap<Address, Vec<LogData>>,
    balances: HashMap<Address, U256>,
    gas_remaining: u64,
    gas_refunded: i64,
    gas_limit: u64,
    is_static: bool,
    chain_id: u64,
    timestamp: U256,
    block_number: u64,
    beneficiary: Address,
    accessed_slots: HashMap<(Address, U256), U256>,
    checkpoints: Vec<(
        HashMap<(Address, U256), U256>,
        HashMap<(Address, U256), U256>,
        HashMap<Address, Vec<LogData>>,
        HashMap<Address, U256>,
        HashMap<(Address, U256), U256>,
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

    fn is_warm(&self, address: Address, key: U256) -> bool {
        self.accessed_slots.contains_key(&(address, key))
    }

    fn warm_slot(&mut self, address: Address, key: U256) {
        if !self.accessed_slots.contains_key(&(address, key)) {
            let original = self
                .persistent
                .get(&(address, key))
                .copied()
                .unwrap_or_default();
            self.accessed_slots.insert((address, key), original);
        }
    }

    pub fn set_block_number(&mut self, block_number: u64) {
        self.block_number = block_number;
    }

    pub fn set_timestamp(&mut self, timestamp: U256) {
        self.timestamp = timestamp;
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

    pub fn get_balance(&self, address: Address) -> U256 {
        self.balances.get(&address).copied().unwrap_or_default()
    }
}

impl StorageProvider for HashMapStorageProvider {
    fn sload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        let is_warm = self.is_warm(address, key);
        let gas = if is_warm { 100 } else { 2100 };
        self.deduct_gas(gas)?;
        self.warm_slot(address, key);
        Ok(self
            .persistent
            .get(&(address, key))
            .copied()
            .unwrap_or_default())
    }

    fn sstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call".into()));
        }
        let is_warm = self.is_warm(address, key);
        let static_gas = 20000u64;
        let dynamic_gas = if is_warm { 0 } else { 2100 };
        self.deduct_gas(static_gas + dynamic_gas)?;

        let present = self
            .persistent
            .get(&(address, key))
            .copied()
            .unwrap_or_default();
        let original = self
            .accessed_slots
            .get(&(address, key))
            .copied()
            .unwrap_or_default();

        if (original == value && present != value) || (present != U256::ZERO && value == U256::ZERO)
        {
            self.refund_gas(4800);
        }

        self.warm_slot(address, key);
        self.persistent.insert((address, key), value);
        Ok(())
    }

    fn tload(&mut self, address: Address, key: U256) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?;
        Ok(self
            .transient
            .get(&(address, key))
            .copied()
            .unwrap_or_default())
    }

    fn tstore(&mut self, address: Address, key: U256, value: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other("static call".into()));
        }
        self.deduct_gas(100)?;
        self.transient.insert((address, key), value);
        Ok(())
    }

    fn emit_event(&mut self, address: Address, event: LogData) -> Result<(), PrecompileError> {
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
            self.balances.clone(),
            self.accessed_slots.clone(),
            self.gas_remaining,
            self.gas_refunded,
        ));
        JournalCheckpoint::default()
    }

    fn checkpoint_commit(&mut self, _checkpoint: JournalCheckpoint) {
        self.checkpoints.pop();
    }

    fn checkpoint_revert(&mut self, _checkpoint: JournalCheckpoint) {
        if let Some((
            persistent,
            transient,
            events,
            balances,
            accessed_slots,
            gas_remaining,
            gas_refunded,
        )) = self.checkpoints.pop()
        {
            self.persistent = persistent;
            self.transient = transient;
            self.events = events;
            self.balances = balances;
            self.accessed_slots = accessed_slots;
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

    fn balance_add(&mut self, address: Address, amount: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other(
                "static call cannot mutate balance".into(),
            ));
        }
        let current = self.balances.get(&address).copied().unwrap_or_default();
        let new = current
            .checked_add(amount)
            .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;
        self.balances.insert(address, new);
        self.deduct_gas(100)
    }

    fn balance_sub(&mut self, address: Address, amount: U256) -> Result<(), PrecompileError> {
        if self.is_static {
            return Err(PrecompileError::Other(
                "static call cannot mutate balance".into(),
            ));
        }
        let current = self.balances.get(&address).copied().unwrap_or_default();
        let new = current
            .checked_sub(amount)
            .ok_or_else(|| PrecompileError::Other("insufficient native balance".into()))?;
        self.balances.insert(address, new);
        self.deduct_gas(100)
    }

    fn balance_get(&mut self, address: Address) -> Result<U256, PrecompileError> {
        self.deduct_gas(100)?;
        Ok(self.balances.get(&address).copied().unwrap_or_default())
    }
}

impl call_protocol::storage_backend::StorageBackend for HashMapStorageProvider {
    fn load(&mut self, address: Address, slot: U256) -> U256 {
        self.persistent
            .get(&(address, slot))
            .copied()
            .unwrap_or_default()
    }

    fn store(&mut self, address: Address, slot: U256, value: U256) {
        self.persistent.insert((address, slot), value);
    }
}

/// Implement `StorageBackend` for the trait object directly so precompiles
/// can pass `&mut dyn StorageProvider` to domain storage structs without
/// needing the unsafe `JournalBackend` wrapper.
impl call_protocol::storage_backend::StorageBackend for dyn StorageProvider {
    fn load(&mut self, address: Address, slot: U256) -> U256 {
        self.sload(address, slot).unwrap_or_default()
    }

    fn store(&mut self, address: Address, slot: U256, value: U256) {
        let _ = self.sstore(address, slot, value);
    }
}

// ── StorageRef ────────────────────────────────────────────────────────

/// Safe wrapper around `&mut dyn StorageProvider` that implements
/// [`StorageBackend`].
///
/// This replaces the unsafe `JournalBackend`.  Key differences from the old
/// `JournalBackend`:
///
/// * `load` takes `&mut self` (not `&self`) — no `mut_from_ref` anti-pattern.
/// * No `as_mut(&self) -> &mut dyn StorageProvider` — impossible to fabricate
///   a long-lived `&mut` alias from a shared `&StorageRef`.
/// * The temporary `&mut dyn StorageProvider` created inside each `load`/`store`
///   call lives only for the duration of that single call, so two copies of
/// `StorageRef` can safely be held by different domain storage structs as long
///   as their `&mut self` methods are not invoked concurrently.
pub struct StorageRef {
    ptr: *mut (),
    vtable: *mut (),
}

impl StorageRef {
    pub fn new(storage: &mut dyn StorageProvider) -> Self {
        let fat: *mut dyn StorageProvider = storage;
        // SAFETY: `*mut dyn Trait` is a fat pointer (data ptr + vtable ptr).
        // We decompose it so `StorageRef` can be `Copy`.  This is sound as long
        // as the original `&mut dyn StorageProvider` remains valid for the
        // duration of the precompile call, which it does by construction.
        let (ptr, vtable): (*mut (), *mut ()) = unsafe { std::mem::transmute(fat) };
        Self { ptr, vtable }
    }
}

impl Copy for StorageRef {}
impl Clone for StorageRef {
    fn clone(&self) -> Self {
        *self
    }
}

impl call_protocol::storage_backend::StorageBackend for StorageRef {
    fn load(&mut self, address: Address, slot: U256) -> U256 {
        let fat: *mut dyn StorageProvider = unsafe { std::mem::transmute((self.ptr, self.vtable)) };
        unsafe { (*fat).sload(address, slot).unwrap_or_default() }
    }

    fn store(&mut self, address: Address, slot: U256, value: U256) {
        let fat: *mut dyn StorageProvider = unsafe { std::mem::transmute((self.ptr, self.vtable)) };
        unsafe {
            let _ = (*fat).sstore(address, slot, value);
        }
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

        provider.sstore(addr, key, U256::from(100)).unwrap();
        let val = provider.sload(addr, key).unwrap();
        assert_eq!(val, U256::from(100));
    }

    #[test]
    fn test_checkpoint_commit() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::ZERO;
        let key = U256::from(1);

        provider.sstore(addr, key, U256::from(42)).unwrap();
        let cp = provider.checkpoint();
        provider.sstore(addr, key, U256::from(99)).unwrap();
        provider.checkpoint_commit(cp);
        assert_eq!(provider.sload(addr, key).unwrap(), U256::from(99));
    }

    #[test]
    fn test_checkpoint_revert() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::ZERO;
        let key = U256::from(1);

        provider.sstore(addr, key, U256::from(42)).unwrap();
        let cp = provider.checkpoint();
        provider.sstore(addr, key, U256::from(99)).unwrap();
        provider.checkpoint_revert(cp);
        assert_eq!(provider.sload(addr, key).unwrap(), U256::from(42));
    }

    #[test]
    fn test_balance_add_sub_get() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::repeat_byte(0x01);

        assert_eq!(provider.balance_get(addr).unwrap(), U256::from(0));
        provider.balance_add(addr, U256::from(500)).unwrap();
        assert_eq!(provider.balance_get(addr).unwrap(), U256::from(500));
        provider.balance_sub(addr, U256::from(200)).unwrap();
        assert_eq!(provider.balance_get(addr).unwrap(), U256::from(300));
    }

    #[test]
    fn test_balance_checkpoint_revert() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::repeat_byte(0x01);

        provider.balance_add(addr, U256::from(100)).unwrap();
        let cp = provider.checkpoint();
        provider.balance_add(addr, U256::from(50)).unwrap();
        assert_eq!(provider.balance_get(addr).unwrap(), U256::from(150));
        provider.checkpoint_revert(cp);
        assert_eq!(provider.balance_get(addr).unwrap(), U256::from(100));
    }

    #[test]
    fn test_storage_slot() {
        let slot = storage_slot(&[b"validators"]);
        assert_eq!(
            slot,
            U256::from_be_slice(keccak256(b"validators").as_slice())
        );
    }

    #[test]
    fn test_input_cost() {
        assert_eq!(input_cost(0), 0);
        assert_eq!(input_cost(32), 6);
        assert_eq!(input_cost(64), 12);
        assert_eq!(input_cost(33), 12);
    }
}
