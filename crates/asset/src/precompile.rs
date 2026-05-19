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
    check_compliance, dispatch, ok_empty, require_caller, write_string32,
    StorageRef, ASSET_ADDRESS, WRAPPED_TOKEN_FACTORY_ADDRESS,
};
use call_precompile::erc20_reader::read_erc20_metadata;
use call_precompile::evm_caller::{execute_evm_call, apply_state_changes, StorageProviderDb};
use call_primitives::{Address, U256};
use revm_precompile::{PrecompileError, PrecompileResult};

// ── Event helpers (cached, computed once) ─────────────────────────────

use std::sync::LazyLock;

static TRANSFER_TOPIC: LazyLock<alloy_primitives::B256> = LazyLock::new(|| {
    alloy_primitives::keccak256(b"Transfer(uint64,address,address,uint128)")
});
static APPROVAL_TOPIC: LazyLock<alloy_primitives::B256> = LazyLock::new(|| {
    alloy_primitives::keccak256(b"Approval(uint64,address,address,uint128)")
});
static MINT_TOPIC: LazyLock<alloy_primitives::B256> = LazyLock::new(|| {
    alloy_primitives::keccak256(b"Mint(uint64,address,uint128)")
});
static BURN_TOPIC: LazyLock<alloy_primitives::B256> = LazyLock::new(|| {
    alloy_primitives::keccak256(b"Burn(uint64,address,uint128)")
});
static REGISTER_TOPIC: LazyLock<alloy_primitives::B256> = LazyLock::new(|| {
    alloy_primitives::keccak256(b"Register(uint64,address,bytes32)")
});
static WRAPPER_CREATED_TOPIC: LazyLock<alloy_primitives::B256> = LazyLock::new(|| {
    alloy_primitives::keccak256(b"WrapperCreated(uint64,address,address)")
});

/// Check compliance and map failure to typed AssetError.
fn require_compliance(
    addr: Address,
    storage: &mut dyn StorageProvider,
) -> Result<(), PrecompileError> {
    check_compliance(addr, storage).map_err(|_| {
        PrecompileError::Other(crate::AssetError::ComplianceFailed.to_string().into())
    })
}

/// Emit an asset event through the storage provider.
fn emit_asset_event(
    storage: &mut dyn StorageProvider,
    topic0: alloy_primitives::B256,
    topics: Vec<alloy_primitives::B256>,
    data: Vec<u8>,
) -> Result<(), PrecompileError> {
    let log = alloy_primitives::LogData::new(
        std::iter::once(topic0)
            .chain(topics)
            .collect(),
        alloy_primitives::Bytes::from(data),
    )
    .expect("invariant: topics non-empty, LogData::new always succeeds");
    storage.emit_event(ASSET_ADDRESS, log)
}

sol! {
    interface IProtocolAsset {
        function getBalance(uint64 assetId, address account) external view returns (uint128 balance);
        function getTotalSupply(uint64 assetId) external view returns (uint128 supply);
        function getAssetInfo(uint64 assetId) external view returns (bytes32 symbol, bytes32 name, uint8 decimals, address issuer, uint128 maxSupply, uint8 status, uint256 registeredAt);
        function transfer(uint64 assetId, address to, uint128 amount) external;
        function batchTransfer(uint64 assetId, address[] calldata to, uint128[] calldata amounts) external;
        function approve(uint64 assetId, address spender, uint128 amount) external;
        function transferFrom(uint64 assetId, address from, address to, uint128 amount) external;
        function mint(uint64 assetId, address to, uint128 amount) external;
        function burn(uint64 assetId, address from, uint128 amount) external;
        function register(string calldata symbol, string calldata name, uint8 decimals, uint128 maxSupply) external returns (uint64 assetId);
        function registerErc20(address evmContract) external returns (uint64 assetId);
        function createWrapper(uint64 assetId) external returns (address wrapperContract);
    }
}

sol! {
    interface IWrappedTokenFactory {
        function createWrapper(string calldata name, string calldata symbol, uint8 decimals, uint256 assetId) external returns (address wrapperContract);
    }
}

/// Stateful asset precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct AssetPrecompile;

impl AssetPrecompile {
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
                // Verify asset exists before reading balance
                store
                    .read_meta(call.assetId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                let balance = store
                    .read_balance(call.assetId, call.account)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(balance)
            },
        )
    }

    fn get_total_supply(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAsset::getTotalSupplyCall, _, _>(
            calldata,
            800,
            storage,
            |call, _storage| {
                let mut store = AssetStorage::new(sr);
                let meta = store
                    .read_meta(call.assetId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                Ok(meta.supply)
            },
        )
    }

    fn get_asset_info(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolAsset::getAssetInfoCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = AssetStorage::new(sr);
                let meta = store
                    .read_meta(call.assetId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let symbol =
                    alloy_primitives::B256::from(write_string32(&meta.symbol).to_be_bytes::<32>());
                let name =
                    alloy_primitives::B256::from(write_string32(&meta.name).to_be_bytes::<32>());
                Ok((
                    symbol,
                    name,
                    U256::from(meta.decimals),
                    meta.issuer,
                    meta.max_supply,
                    U256::from(meta.status),
                    meta.registered_at,
                ))
            },
        )
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
                require_compliance(from, storage)?;
                require_compliance(call.to, storage)?;
                let mut store = AssetStorage::new(sr);
                // Verify asset exists before attempting transfer
                store
                    .read_meta(call.assetId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                store
                    .transfer(call.assetId, from, call.to, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let mut data = Vec::with_capacity(16);
                data.extend_from_slice(&call_precompile::encode_u128(call.amount));
                emit_asset_event(
                    storage,
                    *TRANSFER_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(call.assetId).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(from).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(call.to).to_be_bytes::<32>()),
                    ],
                    data,
                )?;
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
        const MAX_BATCH_SIZE: usize = 256;

        let call = dispatch::decode_call::<IProtocolAsset::batchTransferCall>(calldata)?;
        if call.to.len() != call.amounts.len() {
            return Err(PrecompileError::Other(
                "recipients and amounts length mismatch".into(),
            ));
        }
        if call.to.is_empty() {
            return Err(PrecompileError::Other("empty batch".into()));
        }
        if call.to.len() > MAX_BATCH_SIZE {
            return Err(PrecompileError::Other(
                format!("batch size exceeds limit of {}", MAX_BATCH_SIZE).into(),
            ));
        }
        const BASE_PER_ITEM: u64 = 5000;
        // Event emission overhead per item: 375 base + 3 topics * 375 + 16 bytes * 8
        const EVENT_OVERHEAD_PER_ITEM: u64 = 1700;
        let total_gas = BASE_PER_ITEM
            .checked_add(EVENT_OVERHEAD_PER_ITEM)
            .and_then(|g| g.checked_mul(call.to.len() as u64))
            .ok_or(PrecompileError::Other("batch gas overflow".into()))?;
        storage.deduct_gas(total_gas)?;

        let from = require_caller(msg_sender)?;
        require_compliance(from, storage)?;
        for to in &call.to {
            require_compliance(*to, storage)?;
        }

        let pairs: Vec<(Address, u128)> = call.to.into_iter().zip(call.amounts.clone()).collect();
        let cp = storage.checkpoint();
        let result = (|| -> Result<(), PrecompileError> {
            let mut store = AssetStorage::new(sr);
            // Verify asset exists before batch transfer
            store
                .read_meta(call.assetId)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
            store
                .batch_transfer(call.assetId, from, &pairs)
                .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

            // Emit Transfer event for each recipient BEFORE committing checkpoint
            for (to, amount) in &pairs {
                let mut data = Vec::with_capacity(16);
                data.extend_from_slice(&call_precompile::encode_u128(*amount));
                emit_asset_event(
                    storage,
                    *TRANSFER_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(call.assetId).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(from).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(*to).to_be_bytes::<32>()),
                    ],
                    data,
                )?;
            }
            Ok(())
        })();
        match result {
            Ok(()) => storage.checkpoint_commit(cp),
            Err(e) => {
                storage.checkpoint_revert(cp);
                return Err(e);
            }
        }

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
            |call, storage| {
                let owner = require_caller(msg_sender)?;
                let mut store = AssetStorage::new(sr);
                store
                    .approve(call.assetId, owner, call.spender, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let mut data = Vec::with_capacity(16);
                data.extend_from_slice(&call_precompile::encode_u128(call.amount));
                emit_asset_event(
                    storage,
                    *APPROVAL_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(call.assetId).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(owner).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(call.spender).to_be_bytes::<32>()),
                    ],
                    data,
                )?;
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
                require_compliance(spender, storage)?;
                require_compliance(call.from, storage)?;
                require_compliance(call.to, storage)?;
                let mut store = AssetStorage::new(sr);
                // Verify asset exists
                store
                    .read_meta(call.assetId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                store
                    .transfer_from(call.assetId, spender, call.from, call.to, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let mut data = Vec::with_capacity(16);
                data.extend_from_slice(&call_precompile::encode_u128(call.amount));
                emit_asset_event(
                    storage,
                    *TRANSFER_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(call.assetId).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(call.from).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(call.to).to_be_bytes::<32>()),
                    ],
                    data,
                )?;
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
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                if call.to == Address::ZERO {
                    return Err(PrecompileError::Other("mint to zero address".into()));
                }
                require_compliance(caller, storage)?;
                require_compliance(call.to, storage)?;
                let mut store = AssetStorage::new(sr);
                store
                    .mint(call.assetId, caller, call.to, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let mut data = Vec::with_capacity(16);
                data.extend_from_slice(&call_precompile::encode_u128(call.amount));
                emit_asset_event(
                    storage,
                    *MINT_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(call.assetId).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(call.to).to_be_bytes::<32>()),
                    ],
                    data,
                )?;
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
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                if call.from == Address::ZERO {
                    return Err(PrecompileError::Other("burn from zero address".into()));
                }
                require_compliance(caller, storage)?;
                require_compliance(call.from, storage)?;
                let mut store = AssetStorage::new(sr);
                // Verify asset exists before burn
                store
                    .read_meta(call.assetId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                store
                    .burn(call.assetId, caller, call.from, call.amount)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let mut data = Vec::with_capacity(16);
                data.extend_from_slice(&call_precompile::encode_u128(call.amount));
                emit_asset_event(
                    storage,
                    *BURN_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(call.assetId).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(call.from).to_be_bytes::<32>()),
                    ],
                    data,
                )?;
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
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                // Reject symbol/name that exceed 32-byte word limit
                if call.symbol.len() > 32 {
                    return Err(PrecompileError::Other(
                        "symbol exceeds 32 bytes".into(),
                    ));
                }
                if call.name.len() > 32 {
                    return Err(PrecompileError::Other(
                        "name exceeds 32 bytes".into(),
                    ));
                }
                let mut store = AssetStorage::new(sr);
                let asset_id = store
                    .register(
                        &call.symbol,
                        &call.name,
                        call.decimals,
                        call.maxSupply,
                        caller,
                        storage.timestamp(),
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let symbol = alloy_primitives::B256::from(write_string32(&call.symbol).to_be_bytes::<32>());
                let mut data = Vec::with_capacity(32);
                data.extend_from_slice(symbol.as_slice());
                emit_asset_event(
                    storage,
                    *REGISTER_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(asset_id).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(caller).to_be_bytes::<32>()),
                    ],
                    data,
                )?;
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
                let caller = require_caller(msg_sender)?;
                let meta = read_erc20_metadata(storage, call.evmContract)
                    .map_err(|e| PrecompileError::Other(format!("ERC-20 read failed: {e}").into()))?;
                // Truncate or reject long ERC-20 metadata
                if meta.symbol.len() > 32 {
                    return Err(PrecompileError::Other(
                        "ERC-20 symbol exceeds 32 bytes".into(),
                    ));
                }
                if meta.name.len() > 32 {
                    return Err(PrecompileError::Other(
                        "ERC-20 name exceeds 32 bytes".into(),
                    ));
                }
                let mut store = AssetStorage::new(sr);
                // Reject duplicate ERC-20 contract binding (O(1) via indexed slot)
                if store.evm_contract_asset_id(call.evmContract).is_some() {
                    return Err(PrecompileError::Other(
                        "ERC-20 contract already bound to an asset".into(),
                    ));
                }
                let asset_id = store
                    .register_erc20(
                        call.evmContract,
                        &meta.symbol,
                        &meta.name,
                        meta.decimals,
                        0,               // uncapped
                        Address::ZERO,   // no issuer can mint
                        storage.timestamp(),
                    )
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;

                let symbol = alloy_primitives::B256::from(write_string32(&meta.symbol).to_be_bytes::<32>());
                let mut data = Vec::with_capacity(32);
                data.extend_from_slice(symbol.as_slice());
                emit_asset_event(
                    storage,
                    *REGISTER_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(asset_id).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(caller).to_be_bytes::<32>()),
                    ],
                    data,
                )?;
                Ok(asset_id)
            },
        )
    }

    fn create_wrapper(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate::<IProtocolAsset::createWrapperCall, _, _>(
            calldata,
            100_000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;
                require_compliance(caller, storage)?;
                let mut store = AssetStorage::new(sr);

                // 1. Verify caller is issuer
                let meta = store
                    .read_meta(call.assetId)
                    .map_err(|e| PrecompileError::Other(e.to_string().into()))?;
                if meta.issuer != caller {
                    return Err(PrecompileError::Other("not asset issuer".into()));
                }

                // 2. Verify has_erc20 == 0 (not yet bound)
                if store.has_erc20(call.assetId) {
                    return Err(PrecompileError::Other("asset already has ERC-20 bridge".into()));
                }

                // 2.5 Set has_erc20=1 BEFORE factory call as reentrancy lock.
                // If the factory call fails, the outer checkpoint reverts this back to 0.
                store.set_has_erc20(call.assetId, 1);

                // 3. Build factory call
                let factory_call = IWrappedTokenFactory::createWrapperCall {
                    name: meta.name,
                    symbol: meta.symbol,
                    decimals: meta.decimals,
                    assetId: U256::from(call.assetId),
                };
                let data = factory_call.abi_encode();

                // 4. Execute nested EVM call from ASSET_ADDRESS to factory
                let mut db = StorageProviderDb { provider: storage };
                let (result, state) = execute_evm_call(
                    &mut db,
                    ASSET_ADDRESS,
                    WRAPPED_TOKEN_FACTORY_ADDRESS,
                    data.into(),
                )?;

                let wrapper_addr = match result {
                    revm::context_interface::result::ExecutionResult::Success { output, .. } => {
                        let bytes = match output {
                            revm::context_interface::result::Output::Call(b) => b,
                            revm::context_interface::result::Output::Create(b, _) => b,
                        };
                        let decoded = IWrappedTokenFactory::createWrapperCall::abi_decode_returns(&bytes)
                            .map_err(|e| PrecompileError::Other(format!("factory decode failed: {e}").into()))?;
                        decoded
                    }
                    _ => {
                        return Err(PrecompileError::Other("factory call failed".into()));
                    }
                };

                if wrapper_addr == Address::ZERO {
                    return Err(PrecompileError::Other("factory returned zero address".into()));
                }

                // 5. Apply nested state changes
                apply_state_changes(storage, state)?;

                // 6. Set remaining metadata: evm_contract, dominance=1 (PROTOCOL)
                // has_erc20 was already set to 1 before the factory call as reentrancy lock.
                store.set_evm_contract(call.assetId, wrapper_addr);
                store.set_dominance(call.assetId, 1);

                // Emit WrapperCreated event
                emit_asset_event(
                    storage,
                    *WRAPPER_CREATED_TOPIC,
                    vec![
                        alloy_primitives::B256::from(call_precompile::u64_to_u256(call.assetId).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(wrapper_addr).to_be_bytes::<32>()),
                        alloy_primitives::B256::from(call_precompile::address_to_u256(caller).to_be_bytes::<32>()),
                    ],
                    Vec::new(),
                )?;

                Ok(wrapper_addr)
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
            IProtocolAsset::getTotalSupplyCall::SELECTOR => {
                self.get_total_supply(calldata, storage, sr)
            }
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
            IProtocolAsset::createWrapperCall::SELECTOR => {
                self.create_wrapper(calldata, msg_sender, storage, sr)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::{
        slot_balance, slot_compliance, u128_to_u256, StatefulPrecompile, StorageRef,
        COMPLIANCE_ADDRESS,
    };
    use call_primitives::Address;

    /// Encode a short Solidity string (≤31 bytes) into a U256 storage word.
    /// Format: `bytes[..len] = data`, `bytes[31] = len * 2` (packed string flag).
    fn encode_short(s: &str) -> U256 {
        let len = s.len();
        assert!(len <= 31, "short string only");
        let mut bytes = [0u8; 32];
        bytes[..len].copy_from_slice(s.as_bytes());
        bytes[31] = (len * 2) as u8;
        U256::from_be_bytes::<32>(bytes)
    }

    /// Decode a uint64 asset_id from the low 8 bytes of ABI-encoded precompile output.
    fn decode_asset_id(output: &revm_precompile::PrecompileOutput) -> u64 {
        u64::from_be_bytes([
            output.bytes[24], output.bytes[25], output.bytes[26], output.bytes[27],
            output.bytes[28], output.bytes[29], output.bytes[30], output.bytes[31],
        ])
    }

    #[test]
    fn test_asset_precompile_get_balance() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let addr = Address::repeat_byte(0xAB);

        // Register asset 1
        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test", 18, 0, addr, U256::ZERO).unwrap();
        }

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

        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test", 18, 0, from, U256::ZERO).unwrap();
            store.write_balance(1, from, 1000);
        }

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
        assert_eq!(store.read_balance(1, from).unwrap(), 500);
        assert_eq!(store.read_balance(1, to).unwrap(), 500);

        // Native EVM balance must NOT be affected by protocol transfer
        assert_eq!(provider.get_balance(from), U256::ZERO);
        assert_eq!(provider.get_balance(to), U256::ZERO);
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
        assert_eq!(decode_asset_id(&result), 1);

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
            assert_eq!(store.read_balance(1, recipient).unwrap(), 500);
            assert_eq!(store.read_meta(1).unwrap().supply, 500);
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
            assert_eq!(store.read_balance(1, issuer).unwrap(), 200);
            assert_eq!(store.read_meta(1).unwrap().supply, 700);
        }
    }

    #[test]
    fn test_asset_precompile_approve_and_transfer_from() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0xAB);
        let spender = Address::repeat_byte(0xEF);
        let recipient = Address::repeat_byte(0xCD);

        // Register asset 1 so approve/transfer_from can verify existence
        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test Token", 18, 0, owner, U256::ZERO).unwrap();
        }

        provider
            .sstore(ASSET_ADDRESS, slot_balance(1, owner), u128_to_u256(1000))
            .unwrap();

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
        assert_eq!(store.read_balance(1, owner).unwrap(), 950);
        assert_eq!(store.read_balance(1, recipient).unwrap(), 50);
        assert_eq!(store.read_allowance(1, owner, spender).unwrap(), 50);
    }

    #[test]
    fn test_asset_precompile_batch_transfer_all_allowed() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let r1 = Address::repeat_byte(0x11);
        let r2 = Address::repeat_byte(0x22);
        let r3 = Address::repeat_byte(0x33);

        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test", 18, 0, from, U256::ZERO).unwrap();
            store.write_balance(1, from, 1000);
        }

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
        assert_eq!(store.read_balance(1, from).unwrap(), 400);
        assert_eq!(store.read_balance(1, r1).unwrap(), 100);
        assert_eq!(store.read_balance(1, r2).unwrap(), 200);
        assert_eq!(store.read_balance(1, r3).unwrap(), 300);
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

        // Set compliance policy ID = 1 for asset 1
        // Mark `blocked` as non-compliant (status = 1)
        provider
            .sstore(
                COMPLIANCE_ADDRESS,
                slot_compliance(blocked),
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
        assert_eq!(store.read_balance(1, from).unwrap(), 1000);
        assert_eq!(store.read_balance(1, allowed).unwrap(), 0);
        assert_eq!(store.read_balance(1, blocked).unwrap(), 0);
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

        // Mark `blocked` as non-compliant (status = 1)
        provider
            .sstore(
                COMPLIANCE_ADDRESS,
                slot_compliance(blocked),
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
        let asset_id = decode_asset_id(&output);
        assert_eq!(asset_id, 1);

        // Verify metadata was stored correctly.
        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        let meta = store.read_meta(asset_id).unwrap();
        assert_eq!(meta.name, "Wrapped Ether");
        assert_eq!(meta.symbol, "WETH");
        assert_eq!(meta.decimals, 18);
        assert_eq!(meta.issuer, Address::ZERO); // issuer = zero address
        assert_eq!(meta.max_supply, 0); // uncapped
    }

    #[test]
    fn test_create_wrapper_not_issuer_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);
        let attacker = Address::repeat_byte(0x99);

        // Register asset
        let input = IProtocolAsset::registerCall {
            symbol: "GOLD".into(),
            name: "Gold".into(),
            decimals: 18,
            maxSupply: 10000,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, issuer, &mut provider).unwrap();
        assert_eq!(decode_asset_id(&result), 1);

        // Attacker tries createWrapper
        let input = IProtocolAsset::createWrapperCall { assetId: 1 }.abi_encode();
        let result = precompile.call(&input, attacker, &mut provider);
        assert!(
            result.is_err(),
            "createWrapper by non-issuer should fail: {:?}",
            result
        );
    }

    #[test]
    fn test_create_wrapper_already_has_erc20_fails() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);
        let contract = Address::repeat_byte(0xAA);

        // Register ERC-20-backed asset
        provider.set_code(contract, alloy_primitives::bytes!("6000"));
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
        let result = precompile.call(&input, issuer, &mut provider);
        assert!(result.is_ok(), "registerErc20 failed: {:?}", result.err());

        // Issuer tries createWrapper on already-backed asset
        let input = IProtocolAsset::createWrapperCall { assetId: 1 }.abi_encode();
        let result = precompile.call(&input, issuer, &mut provider);
        assert!(
            result.is_err(),
            "createWrapper on asset with existing ERC-20 should fail: {:?}",
            result
        );
    }

    #[test]
    fn test_transfer_event_emitted() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);

        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test", 18, 0, from, U256::ZERO).unwrap();
            store.write_balance(1, from, 1000);
        }

        let input = IProtocolAsset::transferCall {
            assetId: 1,
            to,
            amount: 500,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        precompile.call(&input, from, &mut provider).unwrap();

        let events = provider.events(ASSET_ADDRESS);
        assert!(!events.is_empty(), "transfer must emit an event");
        assert_eq!(events[0].topics()[0], *TRANSFER_TOPIC);
    }

    #[test]
    fn test_batch_transfer_size_limit() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);

        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test", 18, 0, from, U256::ZERO).unwrap();
            store.write_balance(1, from, 1_000_000);
        }

        let recipients: Vec<Address> = (0..257).map(|i| Address::repeat_byte(i as u8)).collect();
        let amounts: Vec<u128> = vec![1; 257];

        let input = IProtocolAsset::batchTransferCall {
            assetId: 1,
            to: recipients,
            amounts,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, from, &mut provider);
        assert!(
            result.is_err(),
            "batch transfer >256 should fail: {:?}",
            result
        );
        let err = result.unwrap_err().to_string();
        assert!(err.contains("exceeds limit"), "error should mention limit: {}", err);
    }

    #[test]
    fn test_get_total_supply_asset_not_found() {
        let mut provider = HashMapStorageProvider::new(1_000_000);

        let input = IProtocolAsset::getTotalSupplyCall { assetId: 999 }.abi_encode();
        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, Address::ZERO, &mut provider);
        assert!(
            result.is_err(),
            "getTotalSupply for unregistered asset should fail: {:?}",
            result
        );
    }

    #[test]
    fn test_sender_compliance_blocked() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);

        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test", 18, 0, from, U256::ZERO).unwrap();
            store.write_balance(1, from, 1000);
        }

        // Mark sender as non-compliant (status = 1)
        provider
            .sstore(
                COMPLIANCE_ADDRESS,
                slot_compliance(from),
                U256::from(1u64),
            )
            .unwrap();

        let input = IProtocolAsset::transferCall {
            assetId: 1,
            to,
            amount: 100,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, from, &mut provider);
        assert!(
            result.is_err(),
            "transfer by blocked sender should fail: {:?}",
            result
        );
    }

    #[test]
    fn test_registered_at_timestamp() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        provider.set_timestamp(U256::from(1_700_000_000u64));
        let issuer = Address::repeat_byte(0x11);

        let input = IProtocolAsset::registerCall {
            symbol: "GOLD".into(),
            name: "Gold".into(),
            decimals: 18,
            maxSupply: 10000,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        precompile.call(&input, issuer, &mut provider).unwrap();

        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        let registered_at = store.load_meta_u256(1, b"registered_at");
        assert_eq!(registered_at, U256::from(1_700_000_000u64));
    }

    #[test]
    fn test_precompile_transfer_from_allowance_rollback() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0xAB);
        let spender = Address::repeat_byte(0xEF);
        let recipient = Address::repeat_byte(0xCD);

        // Register asset and seed owner balance
        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test Token", 18, 0, owner, U256::ZERO).unwrap();
            store.write_balance(1, owner, 100);
        }

        let mut precompile = AssetPrecompile;

        // Approve 500
        let input = IProtocolAsset::approveCall {
            assetId: 1,
            spender,
            amount: 500,
        }
        .abi_encode();
        precompile.call(&input, owner, &mut provider).unwrap();

        // transferFrom 200 — fails because owner balance (100) < 200
        let input = IProtocolAsset::transferFromCall {
            assetId: 1,
            from: owner,
            to: recipient,
            amount: 200,
        }
        .abi_encode();
        let result = precompile.call(&input, spender, &mut provider);
        assert!(result.is_err(), "transferFrom should fail due to insufficient balance");

        // Allowance must remain 500
        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        assert_eq!(store.read_allowance(1, owner, spender).unwrap(), 500);
    }

    #[test]
    fn test_precompile_burn_allowance_rollback() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let owner = Address::repeat_byte(0xAB);
        let burner = Address::repeat_byte(0xEF);

        // Register asset, mint to owner, approve burner
        {
            let mut store = AssetStorage::new(StorageRef::new(&mut provider));
            store.register("TEST", "Test Token", 18, 0, owner, U256::ZERO).unwrap();
            store.write_balance(1, owner, 100);
        }

        let mut precompile = AssetPrecompile;

        // Approve burner 500
        let input = IProtocolAsset::approveCall {
            assetId: 1,
            spender: burner,
            amount: 500,
        }
        .abi_encode();
        precompile.call(&input, owner, &mut provider).unwrap();

        // Burn 200 from owner by burner — fails because owner balance (100) < 200
        let input = IProtocolAsset::burnCall {
            assetId: 1,
            from: owner,
            amount: 200,
        }
        .abi_encode();
        let result = precompile.call(&input, burner, &mut provider);
        assert!(result.is_err(), "burn should fail due to insufficient balance");

        // Allowance must remain 500
        let mut store = AssetStorage::new(StorageRef::new(&mut provider));
        assert_eq!(store.read_allowance(1, owner, burner).unwrap(), 500);
    }

    #[test]
    fn test_transfer_unregistered_asset() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);

        let input = IProtocolAsset::transferCall {
            assetId: 999,
            to,
            amount: 100,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, from, &mut provider);
        assert!(result.is_err(), "transfer for unregistered asset should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "error should mention asset not found: {}", err);
    }

    #[test]
    fn test_batch_transfer_unregistered_asset() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);

        let input = IProtocolAsset::batchTransferCall {
            assetId: 999,
            to: vec![to],
            amounts: vec![100],
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, from, &mut provider);
        assert!(result.is_err(), "batch transfer for unregistered asset should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "error should mention asset not found: {}", err);
    }

    #[test]
    fn test_burn_unregistered_asset() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = Address::repeat_byte(0xAB);
        let from = Address::repeat_byte(0xCD);

        let input = IProtocolAsset::burnCall {
            assetId: 999,
            from,
            amount: 100,
        }
        .abi_encode();

        let mut precompile = AssetPrecompile;
        let result = precompile.call(&input, caller, &mut provider);
        assert!(result.is_err(), "burn for unregistered asset should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("not found"), "error should mention asset not found: {}", err);
    }

    #[test]
    fn test_mint_to_zero_address_rejected() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);

        // Register asset
        let input = IProtocolAsset::registerCall {
            symbol: "GOLD".into(),
            name: "Gold".into(),
            decimals: 18,
            maxSupply: 10000,
        }
        .abi_encode();
        let mut precompile = AssetPrecompile;
        precompile.call(&input, issuer, &mut provider).unwrap();

        // Mint to zero address
        let input = IProtocolAsset::mintCall {
            assetId: 1,
            to: Address::ZERO,
            amount: 500,
        }
        .abi_encode();
        let result = precompile.call(&input, issuer, &mut provider);
        assert!(result.is_err(), "mint to zero address should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("zero address"), "error should mention zero address: {}", err);
    }

    #[test]
    fn test_burn_from_zero_address_rejected() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let issuer = Address::repeat_byte(0x11);

        // Register asset and mint to issuer
        let input = IProtocolAsset::registerCall {
            symbol: "GOLD".into(),
            name: "Gold".into(),
            decimals: 18,
            maxSupply: 10000,
        }
        .abi_encode();
        let mut precompile = AssetPrecompile;
        precompile.call(&input, issuer, &mut provider).unwrap();

        let input = IProtocolAsset::mintCall {
            assetId: 1,
            to: issuer,
            amount: 500,
        }
        .abi_encode();
        precompile.call(&input, issuer, &mut provider).unwrap();

        // Burn from zero address
        let input = IProtocolAsset::burnCall {
            assetId: 1,
            from: Address::ZERO,
            amount: 100,
        }
        .abi_encode();
        let result = precompile.call(&input, issuer, &mut provider);
        assert!(result.is_err(), "burn from zero address should fail");
        let err = result.unwrap_err().to_string();
        assert!(err.contains("zero address"), "error should mention zero address: {}", err);
    }

    #[test]
    fn test_register_erc20_duplicate_binding_rejected() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let sender = Address::repeat_byte(0x11);
        let contract = Address::repeat_byte(0xAA);

        provider.set_code(contract, alloy_primitives::bytes!("6000"));
        provider
            .sstore(contract, U256::from(0), encode_short("Wrapped Ether"))
            .unwrap();
        provider
            .sstore(contract, U256::from(1), encode_short("WETH"))
            .unwrap();
        provider
            .sstore(contract, U256::from(2), U256::from(18))
            .unwrap();

        let mut precompile = AssetPrecompile;

        // First registration succeeds
        let input = IProtocolAsset::registerErc20Call {
            evmContract: contract,
        }
        .abi_encode();
        let result = precompile.call(&input, sender, &mut provider);
        assert!(result.is_ok(), "first registerErc20 should succeed: {:?}", result.err());

        // Second registration with same contract fails
        let result = precompile.call(&input, sender, &mut provider);
        assert!(
            result.is_err(),
            "duplicate registerErc20 should fail: {:?}",
            result
        );
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("already bound"),
            "error should mention already bound: {}",
            err
        );
    }
}
