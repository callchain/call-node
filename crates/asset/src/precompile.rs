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
use call_precompile::erc20_reader::read_erc20_metadata;
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
        function burn(uint64 assetId, address from, uint128 amount) external;
        function register(string calldata symbol, string calldata name, uint8 decimals, uint128 maxSupply) external returns (uint64 assetId);
        function registerErc20(address evmContract) external returns (uint64 assetId);
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

    fn register_erc20(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate::<IProtocolAsset::registerErc20Call, _, _>(
            calldata,
            50000,
            storage,
            |call, storage| {
                let _caller = require_caller(msg_sender)?;
                let meta = read_erc20_metadata(storage, call.evmContract)
                    .map_err(|e| PrecompileError::Other(format!("ERC-20 read failed: {e}").into()))?;
                let mut store = AssetStorage::new(sr);
                let asset_id = store
                    .register_erc20(
                        call.evmContract,
                        &meta.symbol,
                        &meta.name,
                        meta.decimals,
                        0,               // uncapped
                        Address::ZERO,   // no issuer can mint
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
        let selector: [u8; 4] = calldata[..4].try_into().unwrap_or([0u8; 4]);
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolAsset::getBalanceCall::SELECTOR => self.get_balance(calldata, storage, sr),
            IProtocolAsset::getAssetInfoCall::SELECTOR => {
                self.get_asset_info(calldata, storage, sr)
            }
            IProtocolAsset::transferCall::SELECTOR => {
                self.transfer(calldata, msg_sender, storage, sr)
            }
            IProtocolAsset::batchTransferCall::SELECTOR => {
                self.batch_transfer(calldata, msg_sender, storage, sr)
            }
            IProtocolAsset::approveCall::SELECTOR => {
                self.approve(calldata, msg_sender, storage, sr)
            }
            IProtocolAsset::transferFromCall::SELECTOR => {
                self.transfer_from(calldata, msg_sender, storage, sr)
            }
            IProtocolAsset::mintCall::SELECTOR => self.mint(calldata, msg_sender, storage, sr),
            IProtocolAsset::burnCall::SELECTOR => self.burn(calldata, msg_sender, storage, sr),
            IProtocolAsset::registerCall::SELECTOR => {
                self.register(calldata, msg_sender, storage, sr)
            }
            IProtocolAsset::registerErc20Call::SELECTOR => {
                self.register_erc20(calldata, msg_sender, storage, sr)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::storage_slot;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{
        slot_balance, slot_compliance, u128_to_u256, StatefulPrecompile, StorageRef,
        COMPLIANCE_ADDRESS,
    };
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

    #[test]
    fn test_asset_precompile_batch_transfer_all_allowed() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let r1 = Address::repeat_byte(0x11);
        let r2 = Address::repeat_byte(0x22);
        let r3 = Address::repeat_byte(0x33);

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, from), u128_to_u256(1000))
            .unwrap();
        provider.balance_add(from, U256::from(1000)).unwrap();

        let mut precompile = AssetPrecompile;

        let input = IProtocolAsset::batchTransferCall {
            assetId: 1,
            to: vec![r1, r2, r3],
            amounts: vec![100, 200, 300],
        }
        .abi_encode();

        let result = precompile.call(&input, from, &mut provider);
        assert!(result.is_ok(), "batch transfer failed: {:?}", result.err());

        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        assert_eq!(store.read_balance(1, from), 400);
        assert_eq!(store.read_balance(1, r1), 100);
        assert_eq!(store.read_balance(1, r2), 200);
        assert_eq!(store.read_balance(1, r3), 300);
    }

    #[test]
    fn test_asset_precompile_batch_transfer_blocked_recipient_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let allowed = Address::repeat_byte(0x11);
        let blocked = Address::repeat_byte(0x22);

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, from), u128_to_u256(1000))
            .unwrap();
        provider.balance_add(from, U256::from(1000)).unwrap();

        // Set compliance policy ID = 1 for asset 1
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(1, b"compliance"),
                U256::from(1u64),
            )
            .unwrap();

        // Mark `blocked` as non-compliant (status = 1)
        provider
            .sstore(
                COMPLIANCE_ADDRESS,
                slot_compliance(blocked, 1),
                U256::from(1u64),
            )
            .unwrap();

        let mut precompile = AssetPrecompile;

        // Batch transfer includes the blocked recipient — should fail entirely
        let input = IProtocolAsset::batchTransferCall {
            assetId: 1,
            to: vec![allowed, blocked],
            amounts: vec![100, 200],
        }
        .abi_encode();

        let result = precompile.call(&input, from, &mut provider);
        assert!(
            result.is_err(),
            "batch transfer with blocked recipient should fail"
        );

        // Verify no balances changed (atomic failure)
        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        assert_eq!(store.read_balance(1, from), 1000);
        assert_eq!(store.read_balance(1, allowed), 0);
        assert_eq!(store.read_balance(1, blocked), 0);
    }

    #[test]
    fn test_asset_precompile_batch_transfer_first_allowed_second_blocked() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let allowed = Address::repeat_byte(0x11);
        let blocked = Address::repeat_byte(0x22);

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, from), u128_to_u256(1000))
            .unwrap();
        provider.balance_add(from, U256::from(1000)).unwrap();

        // Set compliance policy ID = 1 for asset 1
        provider
            .sstore(
                ASSET_ADDRESS,
                slot_asset_meta(1, b"compliance"),
                U256::from(1u64),
            )
            .unwrap();

        // Mark `blocked` as non-compliant (status = 1)
        provider
            .sstore(
                COMPLIANCE_ADDRESS,
                slot_compliance(blocked, 1),
                U256::from(1u64),
            )
            .unwrap();

        let mut precompile = AssetPrecompile;

        // Blocked recipient is SECOND in the list — should still fail
        let input = IProtocolAsset::batchTransferCall {
            assetId: 1,
            to: vec![allowed, blocked],
            amounts: vec![100, 200],
        }
        .abi_encode();

        let result = precompile.call(&input, from, &mut provider);
        assert!(
            result.is_err(),
            "batch transfer with blocked recipient at position 1 should fail"
        );
    }

    #[test]
    fn test_asset_precompile_register_erc20_reads_metadata() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x11);
        let contract = Address::repeat_byte(0xAA);

        // Set dummy code so the contract is not rejected.
        provider.set_code(contract, alloy_primitives::bytes!("6000"));

        // Helper: encode a short Solidity string into a U256 storage word.
        // Data is left-aligned (high bytes), length*2 in the low byte.
        let encode_short = |s: &str| {
            let len = s.len();
            assert!(len <= 31, "short string only");
            let mut bytes = [0u8; 32];
            bytes[..len].copy_from_slice(s.as_bytes());
            bytes[31] = (len * 2) as u8;
            U256::from_be_bytes::<32>(bytes)
        };

        // OZ v5 layout: name@0, symbol@1, decimals@2
        provider
            .sstore(contract, U256::from(0), encode_short("Wrapped Ether"))
            .unwrap();
        provider
            .sstore(contract, U256::from(1), encode_short("WETH"))
            .unwrap();
        provider
            .sstore(contract, U256::from(2), U256::from(18))
            .unwrap();

        let input = IProtocolAsset::registerErc20Call {
            evmContract: contract,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_ok(),
            "registerErc20 failed: {:?}",
            result.err()
        );

        // Decode returned asset_id (should be 1 since it's the first registration)
        let output = result.unwrap();
        let asset_id = u64::from_be_bytes([
            output.bytes[24],
            output.bytes[25],
            output.bytes[26],
            output.bytes[27],
            output.bytes[28],
            output.bytes[29],
            output.bytes[30],
            output.bytes[31],
        ]);
        assert_eq!(asset_id, 1);

        // Verify metadata was stored correctly.
        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        let meta = store.read_meta(asset_id);
        assert_eq!(meta.name, "Wrapped Ether");
        assert_eq!(meta.symbol, "WETH");
        assert_eq!(meta.decimals, 18);
        assert_eq!(meta.issuer, Address::ZERO); // issuer = zero address
        assert_eq!(meta.max_supply, 0); // uncapped
    }
}
