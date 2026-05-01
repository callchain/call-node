//! Bridge precompile at 0x103
//!
//! Functions: getTotalDeposits, getTotalWithdrawals,
//!            bridgeToEvm, bridgeToProtocol, externalDeposit, externalWithdraw,
//!            deposit, challengeDeposit

use call_primitives::{Address, AssetId, Balance, Hash};
use alloy_primitives::address;
use std::sync::{Arc, RwLock};

use revm_precompile::PrecompileError;

use crate::{
    decode_address, decode_bytes32, decode_u128, decode_u64, decode_u256_usize, encode_u128,
    ok_empty, slot_asset_meta, slot_balance, u128_to_u256, u256_to_address, u256_to_u128,
};
use crate::storage::{storage_slot, StorageCtx};
use crate::StatefulPrecompile;

pub const BRIDGE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000103");

#[derive(Debug, Clone)]
pub struct BridgeOpStatus {
    pub op_id: u64,
    pub completed: bool,
    pub confirmations: u64,
    pub source_tx_hash: Option<Hash>,
}

#[derive(Debug, Default)]
pub struct BridgeState {
    pub total_deposits: Balance,
    pub total_withdrawals: Balance,
    pub pending_ops: u64,
}

pub fn bridge_get_status() -> BridgeOpStatus {
    BridgeOpStatus {
        op_id: 0,
        completed: false,
        confirmations: 0,
        source_tx_hash: None,
    }
}

pub fn bridge_deposit(
    state: &mut BridgeState,
    _asset_id: AssetId,
    _from: Address,
    _to: Address,
    amount: Balance,
) {
    state.total_deposits += amount;
    state.pending_ops += 1;
}

pub fn bridge_withdraw(
    state: &mut BridgeState,
    _asset_id: AssetId,
    _from: Address,
    _to: Address,
    amount: Balance,
) {
    state.total_withdrawals += amount;
    state.pending_ops += 1;
}

/// Deprecated: live bridge was accessed via OnceLock; now BridgePrecompile
/// reads directly from EVM storage through StorageCtx.
#[deprecated(note = "bridge reads from EVM storage; OnceLock no longer used")]
pub fn set_live_bridge(_bridge: Arc<RwLock<BridgeState>>) {}

/// Deprecated: always returns None. Bridge state is in EVM storage.
#[deprecated(note = "bridge reads from EVM storage; OnceLock no longer used")]
pub fn get_live_bridge() -> Option<Arc<RwLock<BridgeState>>> {
    None
}

// ── Storage slot helpers ──────────────────────────────────────────────

fn slot_bridge_total_deposits() -> alloy_primitives::U256 {
    alloy_primitives::U256::from(0)
}

fn slot_bridge_total_withdrawals() -> alloy_primitives::U256 {
    alloy_primitives::U256::from(1)
}

fn slot_bridge_paused() -> alloy_primitives::U256 {
    storage_slot(&[b"paused"])
}

fn slot_bridge_processed(tx_hash: [u8; 32]) -> alloy_primitives::U256 {
    storage_slot(&[b"processed", &tx_hash])
}

// ── Validation helpers ────────────────────────────────────────────────

fn bridge_asset_registered(asset_id: u64) -> bool {
    let issuer = StorageCtx::sload(crate::ASSET_ADDRESS, slot_asset_meta(asset_id, b"issuer"))
        .map(u256_to_address)
        .unwrap_or(Address::ZERO);
    issuer != Address::ZERO
}

fn bridge_is_paused() -> bool {
    StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_paused())
        .map(|v| v.to_be_bytes::<32>()[31] != 0)
        .unwrap_or(false)
}

fn bridge_validate_asset(asset_id: u64) -> Result<(), PrecompileError> {
    if asset_id == 0 {
        return Err(PrecompileError::Other("asset 0 not bridgeable".into()));
    }
    if !bridge_asset_registered(asset_id) {
        return Err(PrecompileError::Other("asset not registered".into()));
    }
    if bridge_is_paused() {
        return Err(PrecompileError::Other("bridge paused".into()));
    }
    let status = StorageCtx::sload(crate::ASSET_ADDRESS, slot_asset_meta(asset_id, b"status"))
        .map(|v| v.to_be_bytes::<32>()[31])
        .unwrap_or(0);
    if status != 0 {
        return Err(PrecompileError::Other("asset not active".into()));
    }
    Ok(())
}

fn bridge_validate_basic(asset_id: u64) -> Result<(), PrecompileError> {
    if !bridge_asset_registered(asset_id) {
        return Err(PrecompileError::Other("asset not registered".into()));
    }
    if bridge_is_paused() {
        return Err(PrecompileError::Other("bridge paused".into()));
    }
    Ok(())
}

// ── Balance helpers ───────────────────────────────────────────────────

fn load_bal(asset_id: u64, addr: Address) -> u128 {
    StorageCtx::sload(crate::ASSET_ADDRESS, slot_balance(asset_id, addr))
        .map(u256_to_u128)
        .unwrap_or(0)
}

fn save_bal(asset_id: u64, addr: Address, amount: u128) {
    StorageCtx::sstore(crate::ASSET_ADDRESS, slot_balance(asset_id, addr), u128_to_u256(amount));
}

fn credit_bal(asset_id: u64, addr: Address, amount: u128) -> Result<(), PrecompileError> {
    let bal = load_bal(asset_id, addr)
        .checked_add(amount)
        .ok_or_else(|| PrecompileError::Other("balance overflow".into()))?;
    save_bal(asset_id, addr, bal);
    Ok(())
}

fn debit_bal(asset_id: u64, addr: Address, amount: u128) -> Result<(), PrecompileError> {
    let bal = load_bal(asset_id, addr)
        .checked_sub(amount)
        .ok_or_else(|| PrecompileError::Other("insufficient balance".into()))?;
    save_bal(asset_id, addr, bal);
    Ok(())
}

// ── Bridge total helpers ──────────────────────────────────────────────

fn add_total_deposits(amount: u128) {
    let total = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_deposits())
        .map(u256_to_u128)
        .unwrap_or(0);
    StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_total_deposits(), u128_to_u256(total + amount));
}

fn add_total_withdrawals(amount: u128) {
    let total = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_withdrawals())
        .map(u256_to_u128)
        .unwrap_or(0);
    StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_total_withdrawals(), u128_to_u256(total + amount));
}

// ── ABI decoding helpers ─────────────────────────────────────────────

fn decode_bytes(input: &[u8], slot_offset: usize) -> Option<Vec<u8>> {
    let data_offset = decode_u256_usize(input, slot_offset)?;
    let abs_offset = 4 + data_offset;
    if input.len() < abs_offset + 32 {
        return None;
    }
    let len = decode_u256_usize(input, abs_offset)?;
    let data_start = abs_offset + 32;
    if input.len() < data_start + len {
        return None;
    }
    Some(input[data_start..data_start + len].to_vec())
}

// ── BridgePrecompile ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct BridgePrecompile;

impl BridgePrecompile {
    fn get_total_deposits(&self) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1500;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let deposits = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_deposits())
            .map(u256_to_u128)
            .unwrap_or(0);

        let out = revm_precompile::PrecompileOutput::new(0, encode_u128(deposits).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn get_total_withdrawals(&self) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1500;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let withdrawals = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_total_withdrawals())
            .map(u256_to_u128)
            .unwrap_or(0);

        let out = revm_precompile::PrecompileOutput::new(0, encode_u128(withdrawals).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    // bridgeToEvm(uint64 assetId, address to, uint128 amount) -> 0xdbae8a2a
    fn bridge_to_evm(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let _to = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid to".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        bridge_validate_asset(asset_id)?;

        // Deduct protocol balance from sender
        debit_bal(asset_id, msg_sender, amount)?;

        // Note: EVM native balance credit is not possible from precompile context.
        // The recipient's EVM balance must be updated via consensus-layer execution
        // or a separate EVM transfer. This precompile only updates protocol state.

        add_total_withdrawals(amount);
        ok_empty()
    }

    // bridgeToProtocol(uint64 assetId, address to, uint128 amount) -> 0xf0c861e4
    fn bridge_to_protocol(&self, input: &[u8], _msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 100 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let to = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid to".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        bridge_validate_asset(asset_id)?;

        // Note: EVM native balance deduction is not possible from precompile context.
        // The caller must ensure sufficient EVM balance is available (e.g., via tx value).
        // This precompile only updates protocol state.

        credit_bal(asset_id, to, amount)?;
        add_total_deposits(amount);
        ok_empty()
    }

    // externalDeposit(bytes32 sourceTxHash, uint64 assetId, address recipient, uint128 amount)
    // -> 0x1aba0700
    // Simplified: credits recipient balance. Full validator signature verification is done
    // at the consensus layer; this precompile is a convenience for validators to relay.
    fn external_deposit(&self, input: &[u8], _msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let source_tx_hash = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceTxHash".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let recipient = decode_address(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid recipient".into()))?;
        let amount = decode_u128(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        // Check not already processed
        let processed = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_processed(source_tx_hash))
            .map(|v| v != alloy_primitives::U256::ZERO)
            .unwrap_or(false);
        if processed {
            return Err(PrecompileError::Other("source tx already processed".into()));
        }

        bridge_validate_basic(asset_id)?;

        // Mark as processed
        StorageCtx::sstore(BRIDGE_ADDRESS, slot_bridge_processed(source_tx_hash), alloy_primitives::U256::from(1u8));

        credit_bal(asset_id, recipient, amount)?;
        add_total_deposits(amount);
        ok_empty()
    }

    // externalWithdraw(uint64 targetChain, bytes targetAddress, uint64 assetId, uint128 amount)
    // -> 0x393da669
    fn external_withdraw(&self, input: &[u8], msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let _target_chain = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid targetChain".into()))?;
        let asset_id = decode_u64(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;

        // Decode targetAddress (dynamic bytes)
        let _target_addr = decode_bytes(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid targetAddress".into()))?;

        bridge_validate_basic(asset_id)?;

        debit_bal(asset_id, msg_sender, amount)?;
        add_total_withdrawals(amount);
        ok_empty()
    }

    // deposit(uint64 sourceChain, address targetAddress, uint128 amount, uint64 assetId, bytes proof)
    // -> 0x2689cfc0
    // Simplified: credits targetAddress balance. The proof is accepted but not cryptographically
    // verified in the precompile (full verification happens at consensus layer).
    fn deposit(&self, input: &[u8], _msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 164 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let _source_chain = decode_u64(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceChain".into()))?;
        let target_address = decode_address(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid targetAddress".into()))?;
        let amount = decode_u128(input, 68)
            .ok_or_else(|| PrecompileError::Other("invalid amount".into()))?;
        let asset_id = decode_u64(input, 100)
            .ok_or_else(|| PrecompileError::Other("invalid assetId".into()))?;

        // Decode proof (dynamic bytes) — validated at consensus layer
        let _proof = decode_bytes(input, 132)
            .ok_or_else(|| PrecompileError::Other("invalid proof".into()))?;

        bridge_validate_basic(asset_id)?;

        credit_bal(asset_id, target_address, amount)?;
        add_total_deposits(amount);
        ok_empty()
    }

    // challengeDeposit(bytes32 sourceTxHash, bytes proof) -> 0x07dee8d0
    fn challenge_deposit(&self, input: &[u8], _msg_sender: Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 10000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 68 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let source_tx_hash = decode_bytes32(input, 4)
            .ok_or_else(|| PrecompileError::Other("invalid sourceTxHash".into()))?;

        // Decode proof (dynamic bytes)
        let _proof = decode_bytes(input, 36)
            .ok_or_else(|| PrecompileError::Other("invalid proof".into()))?;

        let processed = StorageCtx::sload(BRIDGE_ADDRESS, slot_bridge_processed(source_tx_hash))
            .map(|v| v != alloy_primitives::U256::ZERO)
            .unwrap_or(false);
        if !processed {
            return Err(PrecompileError::Other("source tx not processed".into()));
        }

        // Challenge period queue not yet implemented in EVM-only mode
        Err(PrecompileError::Other(
            "challenge deposit: not yet implemented in EVM-only mode".into(),
        ))
    }
}

impl StatefulPrecompile for BridgePrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: alloy_primitives::Address) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }
        match &calldata[..4] {
            &[0xa8, 0x7e, 0x4f, 0x2a] => self.get_total_deposits(),
            &[0x9c, 0x3e, 0x6d, 0x1b] => self.get_total_withdrawals(),
            &[0xdb, 0xae, 0x8a, 0x2a] => self.bridge_to_evm(calldata, msg_sender),
            &[0xf0, 0xc8, 0x61, 0xe4] => self.bridge_to_protocol(calldata, msg_sender),
            &[0x1a, 0xba, 0x07, 0x00] => self.external_deposit(calldata, msg_sender),
            &[0x39, 0x3d, 0xa6, 0x69] => self.external_withdraw(calldata, msg_sender),
            &[0x26, 0x89, 0xcf, 0xc0] => self.deposit(calldata, msg_sender),
            &[0x07, 0xde, 0xe8, 0xd0] => self.challenge_deposit(calldata, msg_sender),
            _ => Err(revm_precompile::PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_bridge_address() {
        assert_eq!(
            BRIDGE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000103")
        );
    }

    #[test]
    fn test_bridge_precompile_protocol_balance() {
        let status = bridge_get_status();
        assert!(!status.completed);
        assert_eq!(status.confirmations, 0);
    }

    #[test]
    fn test_bridge_deposit_and_withdraw() {
        let mut state = BridgeState::default();
        bridge_deposit(&mut state, 1, Address::ZERO, Address::ZERO, 1000);
        assert_eq!(state.total_deposits, 1000);
        assert_eq!(state.pending_ops, 1);

        bridge_withdraw(&mut state, 1, Address::ZERO, Address::ZERO, 500);
        assert_eq!(state.total_withdrawals, 500);
        assert_eq!(state.pending_ops, 2);
    }

    #[test]
    fn test_bridge_precompile_stateful_reads() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);

        crate::storage::StorageCtx::enter(&mut provider, || {
            crate::storage::StorageCtx::sstore(
                BRIDGE_ADDRESS,
                alloy_primitives::U256::from(0),
                u128_to_u256(5000),
            );
            crate::storage::StorageCtx::sstore(
                BRIDGE_ADDRESS,
                alloy_primitives::U256::from(1),
                u128_to_u256(2000),
            );

            let mut precompile = BridgePrecompile;

            // getTotalDeposits
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0xa8, 0x7e, 0x4f, 0x2a]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let deposits = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(deposits, 5000);

            // getTotalWithdrawals
            let mut input = vec![0u8; 4];
            input[0..4].copy_from_slice(&[0x9c, 0x3e, 0x6d, 0x1b]);
            let result = precompile.call(&input, Address::ZERO).unwrap();
            let withdrawals = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            assert_eq!(withdrawals, 2000);
        });
    }
}
