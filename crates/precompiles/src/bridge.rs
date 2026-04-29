//! Bridge precompile at 0x103

use call_primitives::{Address, AssetId, Balance, Hash};
use alloy_primitives::address;
use std::sync::{Arc, RwLock};

#[allow(dead_code)]
pub(crate) const BRIDGE_ADDRESS: alloy_primitives::Address =
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

// ── BridgePrecompile (stateful) ───────────────────────────────────────

use crate::StatefulPrecompile;
use crate::storage::StorageCtx;

fn u256_to_u128(v: alloy_primitives::U256) -> u128 {
    u128::from_be_bytes(v.to_be_bytes::<32>()[16..32].try_into().unwrap())
}

fn u128_to_u256(v: u128) -> alloy_primitives::U256 {
    let mut bytes = [0u8; 32];
    bytes[16..32].copy_from_slice(&v.to_be_bytes());
    alloy_primitives::U256::from_be_bytes::<32>(bytes)
}

#[derive(Debug, Default, Clone, Copy)]
pub struct BridgePrecompile;

impl BridgePrecompile {
    fn get_total_deposits(&self) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1500;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let deposits = StorageCtx::sload(BRIDGE_ADDRESS, alloy_primitives::U256::from(0))
            .map(u256_to_u128)
            .unwrap_or(0);

        let mut output = [0u8; 32];
        output[16..].copy_from_slice(&deposits.to_be_bytes());

        let out = revm_precompile::PrecompileOutput::new(0, output.to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn get_total_withdrawals(&self) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1500;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let withdrawals = StorageCtx::sload(BRIDGE_ADDRESS, alloy_primitives::U256::from(1))
            .map(u256_to_u128)
            .unwrap_or(0);

        let mut output = [0u8; 32];
        output[16..].copy_from_slice(&withdrawals.to_be_bytes());

        let out = revm_precompile::PrecompileOutput::new(0, output.to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }
}

impl StatefulPrecompile for BridgePrecompile {
    fn call(&mut self, calldata: &[u8], _msg_sender: alloy_primitives::Address) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }
        match &calldata[..4] {
            &[0xa8, 0x7e, 0x4f, 0x2a] => self.get_total_deposits(),
            &[0x9c, 0x3e, 0x6d, 0x1b] => self.get_total_withdrawals(),
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
