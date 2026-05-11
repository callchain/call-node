//! Asset precompile entry point (0x201).
//!
//! Thin wrapper that routes EVM calls to [`AssetStorage`] backed by
//! EVM storage.  Business logic lives in [`AssetStorage`]; this file
//! only handles ABI decode/encode, gas accounting, selector dispatch and
//! cross-domain concerns (compliance).

use crate::AssetStorage;
use alloy_sol_types::{sol, SolCall};
use call_precompile::storage::StorageProvider;
use call_precompile::{
    dispatch, ok_empty, require_caller, slot_asset_meta, slot_compliance, write_string32,
    StorageRef, ASSET_ADDRESS, COMPLIANCE_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::CALL_ASSET_ID;
use revm_precompile::{PrecompileError, PrecompileResult};

sol! {
    interface IProtocolAsset {
        function getBalance(uint64 assetId, address account) external view returns (uint128 balance);
        function getAssetInfo(uint64 assetId) external view returns (bytes32 symbol, bytes32 name, uint8 decimals, address issuer, uint128 maxSupply, uint8 status);
        function transfer(uint64 assetId, address to, uint128 amount) external;
        function batchTransfer(uint64 assetId, address[] calldata to, uint128[] calldata amounts) external;
        function approve(uint64 assetId, address spender, uint128 amount) external;
        function transferFrom(uint64 assetId, address from, address to, uint128 amount) external;
        function mint(uint64 assetId, address to, uint128 amount) external;
        function issuerMint(uint64 assetId, address to, uint128 amount) external;
        function burn(uint64 assetId, address from, uint128 amount) external;
        function register(string calldata symbol, string calldata name, uint8 decimals, uint128 maxSupply) external returns (uint64 assetId);
    }
}

/// Stateful asset precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct AssetPrecompile;

impl AssetPrecompile {
    /// Check compliance for an address against the asset's compliance policy.
    fn check_compliance(
        asset_id: u64,
        addr: &Address,
        storage: &mut dyn StorageProvider,
    ) -> Result<(), PrecompileError> {
        let policy_id = storage
            .sload(ASSET_ADDRESS, slot_asset_meta(asset_id, b"compliance"))
            .map(|v| v.to_be_bytes::<32>()[31] as u64)
            .unwrap_or(0);

        if policy_id == 0 {
            return Ok(());
        }

        let status = storage
            .sload(COMPLIANCE_ADDRESS, slot_compliance(*addr, policy_id as u8))
            .map(|v| v.to_be_bytes::<32>()[31])
            .unwrap_or(0);

        if status == 0 {
            Ok(())
        } else {
            Err(PrecompileError::Other("compliance check failed".into()))
        }
    }

    fn get_balance(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAsset::getBalanceCall, _, _>(
            calldata,
            800,
            storage,
            |call, _storage| {
                let mut store = AssetStorage::new(sr);
                let balance = store.read_balance(call.assetId, call.account);
                Ok(balance)
            },
        )
    }

    fn get_asset_info(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        storage.deduct_gas(1000)?;
        let call = dispatch::decode_call::<IProtocolAsset::getAssetInfoCall>(calldata)?;
        let mut store = AssetStorage::new(sr);
        let meta = store.read_meta(call.assetId);

        let mut out = [0u8; 192];
        out[0..32].copy_from_slice(&write_string32(&meta.symbol).to_be_bytes::<32>());
        out[32..64].copy_from_slice(&write_string32(&meta.name).to_be_bytes::<32>());
        out[95] = meta.decimals;
        out[108..128].copy_from_slice(meta.issuer.as_slice());
        out[128..160].copy_from_slice(&call_precompile::encode_u128(meta.max_supply));
        out[191] = meta.status;

        let output = revm_precompile::PrecompileOutput::new(0, out.to_vec().into());
        Ok(call_precompile::storage::fill_precompile_output(
            output, storage,
        ))
    }

    fn transfer(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAsset::transferCall, _>(
            calldata,
            5000,
            storage,
            |call, storage| {
                let from = require_caller(msg_sender)?;
                Self::check_compliance(call.assetId, &from, storage)?;
                Self::check_compliance(call.assetId, &call.to, storage)?;
                let mut store = AssetStorage::new(sr);
                store
                    .transfer(call.assetId, from, call.to, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                // Bridge CALL transfers to native EVM balance
                if call.assetId == CALL_ASSET_ID {
                    storage
                        .balance_sub(from, U256::from(call.amount))
                        .map_err(|e| {
                            PrecompileError::Other(format!("native balance sub: {e}").into())
                        })?;
                    storage
                        .balance_add(call.to, U256::from(call.amount))
                        .map_err(|e| {
                            PrecompileError::Other(format!("native balance add: {e}").into())
                        })?;
                }
                Ok(())
            },
        )
    }

    fn batch_transfer(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let call = dispatch::decode_call::<IProtocolAsset::batchTransferCall>(calldata)?;
        if call.to.len() != call.amounts.len() {
            return Err(PrecompileError::Other(
                "recipients and amounts length mismatch".into(),
            ));
        }
        if call.to.is_empty() {
            return Err(PrecompileError::Other("empty batch".into()));
        }
        let total_gas = 5000u64
            .checked_mul(call.to.len() as u64)
            .ok_or(PrecompileError::Other("batch gas overflow".into()))?;
        storage.deduct_gas(total_gas)?;

        let from = require_caller(msg_sender)?;
        Self::check_compliance(call.assetId, &from, storage)?;
        for to in &call.to {
            Self::check_compliance(call.assetId, to, storage)?;
        }

        let pairs: Vec<(Address, u128)> = call.to.into_iter().zip(call.amounts).collect();
        let cp = storage.checkpoint();
        let mut store = AssetStorage::new(sr);
        store
            .batch_transfer(call.assetId, from, &pairs)
            .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
        // Bridge CALL transfers to native EVM balance
        if call.assetId == CALL_ASSET_ID {
            let total: u128 = pairs.iter().map(|(_, amt)| *amt).sum();
            storage
                .balance_sub(from, U256::from(total))
                .map_err(|e| PrecompileError::Other(format!("native balance sub: {e}").into()))?;
            for (to, amount) in &pairs {
                storage.balance_add(*to, U256::from(*amount)).map_err(|e| {
                    PrecompileError::Other(format!("native balance add: {e}").into())
                })?;
            }
        }
        storage.checkpoint_commit(cp);

        ok_empty(storage)
    }

    fn approve(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAsset::approveCall, _>(
            calldata,
            3000,
            storage,
            |call, _storage| {
                let owner = require_caller(msg_sender)?;
                let mut store = AssetStorage::new(sr);
                store.approve(call.assetId, owner, call.spender, call.amount);
                Ok(())
            },
        )
    }

    fn transfer_from(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAsset::transferFromCall, _>(
            calldata,
            6000,
            storage,
            |call, storage| {
                let spender = require_caller(msg_sender)?;
                Self::check_compliance(call.assetId, &call.from, storage)?;
                Self::check_compliance(call.assetId, &call.to, storage)?;
                let mut store = AssetStorage::new(sr);
                store
                    .transfer_from(call.assetId, spender, call.from, call.to, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                // Bridge CALL transfers to native EVM balance
                if call.assetId == CALL_ASSET_ID {
                    storage
                        .balance_sub(call.from, U256::from(call.amount))
                        .map_err(|e| {
                            PrecompileError::Other(format!("native balance sub: {e}").into())
                        })?;
                    storage
                        .balance_add(call.to, U256::from(call.amount))
                        .map_err(|e| {
                            PrecompileError::Other(format!("native balance add: {e}").into())
                        })?;
                }
                Ok(())
            },
        )
    }

    fn mint(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAsset::mintCall, _>(
            calldata,
            10000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AssetStorage::new(sr);
                store
                    .mint(call.assetId, caller, call.to, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn burn(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolAsset::burnCall, _>(
            calldata,
            8000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AssetStorage::new(sr);
                store
                    .burn(call.assetId, caller, call.from, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(())
            },
        )
    }

    fn register(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate::<IProtocolAsset::registerCall, _, _>(
            calldata,
            50000,
            storage,
            |call, _storage| {
                let caller = require_caller(msg_sender)?;
                let mut store = AssetStorage::new(sr);
                let asset_id = store
                    .register(
                        &call.symbol,
                        &call.name,
                        call.decimals,
                        call.maxSupply,
                        caller,
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(asset_id)
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for AssetPrecompile {
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
        let selector: [u8; 4] = calldata[..4]
            .try_into()
            .unwrap_or([0u8; 4]);
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolAsset::getBalanceCall::SELECTOR => self.get_balance(calldata, storage, sr),
            IProtocolAsset::getAssetInfoCall::SELECTOR => self.get_asset_info(calldata, storage, sr),
            IProtocolAsset::transferCall::SELECTOR => self.transfer(calldata, msg_sender, storage, sr),
            IProtocolAsset::batchTransferCall::SELECTOR => {
                self.batch_transfer(calldata, msg_sender, storage, sr)
            }
            IProtocolAsset::approveCall::SELECTOR => self.approve(calldata, msg_sender, storage, sr),
            IProtocolAsset::transferFromCall::SELECTOR => {
                self.transfer_from(calldata, msg_sender, storage, sr)
            }
            IProtocolAsset::mintCall::SELECTOR => self.mint(calldata, msg_sender, storage, sr),
            IProtocolAsset::issuerMintCall::SELECTOR => self.mint(calldata, msg_sender, storage, sr),
            IProtocolAsset::burnCall::SELECTOR => self.burn(calldata, msg_sender, storage, sr),
            IProtocolAsset::registerCall::SELECTOR => self.register(calldata, msg_sender, storage, sr),
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{slot_balance, u128_to_u256, StatefulPrecompile, StorageRef};
    use call_primitives::Address;

    #[test]
    fn test_asset_precompile_get_balance() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::repeat_byte(0xAB);

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, addr), u128_to_u256(5000))
            .unwrap();

        let input = IProtocolAsset::getBalanceCall {
            assetId: 1,
            account: addr,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let balance = call_precompile::u256_to_u128(alloy_primitives::U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(balance, 5000);
    }

    #[test]
    fn test_asset_precompile_transfer() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, from), u128_to_u256(1000))
            .unwrap();
        // Seed native EVM balance for CALL (asset_id=1) bridging
        provider.balance_add(from, U256::from(1000)).unwrap();

        let input = IProtocolAsset::transferCall {
            assetId: 1,
            to,
            amount: 500,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, from, &mut provider);
        assert!(result.is_ok(), "transfer failed: {:?}", result.err());

        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        assert_eq!(store.read_balance(1, from), 500);
        assert_eq!(store.read_balance(1, to), 500);
    }

    #[test]
    fn test_asset_precompile_mint_and_burn() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);

        // Register
        let input = IProtocolAsset::registerCall {
            symbol: "GOLD".into(),
            name: "Gold".into(),
            decimals: 18,
            maxSupply: 10000,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, issuer, &mut provider).unwrap();
        let asset_id = u64::from_be_bytes([
            result.bytes[24],
            result.bytes[25],
            result.bytes[26],
            result.bytes[27],
            result.bytes[28],
            result.bytes[29],
            result.bytes[30],
            result.bytes[31],
        ]);
        assert_eq!(asset_id, 1);

        // Mint
        let input = IProtocolAsset::mintCall {
            assetId: 1,
            to: recipient,
            amount: 500,
        }
        .abi_encode();

        let result = precompile.call(&input, issuer, &mut provider);
        assert!(result.is_ok(), "mint failed: {:?}", result.err());

        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            assert_eq!(store.read_balance(1, recipient), 500);
            assert_eq!(store.read_meta(1).supply, 500);
        }

        // Mint to issuer
        let input = IProtocolAsset::mintCall {
            assetId: 1,
            to: issuer,
            amount: 400,
        }
        .abi_encode();
        let result = precompile.call(&input, issuer, &mut provider);
        assert!(result.is_ok());

        // Burn from issuer
        let input = IProtocolAsset::burnCall {
            assetId: 1,
            from: issuer,
            amount: 200,
        }
        .abi_encode();

        let result = precompile.call(&input, issuer, &mut provider);
        assert!(result.is_ok(), "burn failed: {:?}", result.err());

        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            assert_eq!(store.read_balance(1, issuer), 200);
            assert_eq!(store.read_meta(1).supply, 700);
        }
    }

    #[test]
    fn test_asset_precompile_approve_and_transfer_from() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0xAB);
        let spender = Address::repeat_byte(0xEF);
        let recipient = Address::repeat_byte(0xCD);

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, owner), u128_to_u256(1000))
            .unwrap();
        // Seed native EVM balance for CALL (asset_id=1) bridging
        provider.balance_add(owner, U256::from(1000)).unwrap();

        let mut precompile = AssetPrecompile;

        // Approve
        let input = IProtocolAsset::approveCall {
            assetId: 1,
            spender,
            amount: 100,
        }
        .abi_encode();

        let result = precompile.call(&input, owner, &mut provider);
        assert!(result.is_ok(), "approve failed: {:?}", result.err());

        // TransferFrom
        let input = IProtocolAsset::transferFromCall {
            assetId: 1,
            from: owner,
            to: recipient,
            amount: 50,
        }
        .abi_encode();

        let result = precompile.call(&input, spender, &mut provider);
        assert!(result.is_ok(), "transfer_from failed: {:?}", result.err());

        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        assert_eq!(store.read_balance(1, owner), 950);
        assert_eq!(store.read_balance(1, recipient), 50);
        assert_eq!(store.read_allowance(1, owner, spender), 50);
    }
}
