//! Oracle precompile entry point (0x101).
//!
//! Thin wrapper that routes EVM calls to [`OracleStorage`] backed by
//! [`JournalBackend`].  Business logic lives in [`OracleStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_sol_types::{sol, SolCall};
use call_precompile::{
    dispatch,
    journal_backend::JournalBackend,
    require_caller,
    storage::{storage_slot, StorageProvider},
    u128_to_u256, u256_to_u128, u256_to_u64, u64_to_u256,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;
use call_validator::ValidatorStorage;
use revm_precompile::{PrecompileError, PrecompileResult};

pub const ORACLE_ADDRESS: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000101");

pub const STALE_THRESHOLD_SECS: u64 = 3600;

// ── Storage slot helpers ──────────────────────────────────────────────

fn slot_oracle_price(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"price"])
}

fn slot_oracle_twap(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"twap"])
}

fn slot_oracle_timestamp(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"ts"])
}

fn slot_oracle_block(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"block"])
}

fn slot_oracle_count(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"count"])
}

fn slot_oracle_tracked_count() -> U256 {
    storage_slot(&[b"tracked_count"])
}

fn slot_oracle_tracked_asset(index: u64) -> U256 {
    storage_slot(&[b"tracked", &index.to_be_bytes()[..]])
}

// ── OracleStorage ─────────────────────────────────────────────────────

/// Business logic for oracle operations backed by any StorageBackend.
pub struct OracleStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> OracleStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    pub fn read_price(&self, asset_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_price(asset_id)),
        )
    }

    pub fn read_twap(&self, asset_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_twap(asset_id)),
        )
    }

    pub fn read_timestamp(&self, asset_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_timestamp(asset_id)),
        )
    }

    pub fn read_block(&self, asset_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_block(asset_id)),
        )
    }

    pub fn read_count(&self, asset_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_count(asset_id)),
        )
    }

    pub fn is_stale(&self, asset_id: u64, current_ts: u64) -> bool {
        let stored_ts = self.read_timestamp(asset_id);
        stored_ts + STALE_THRESHOLD_SECS < current_ts
    }

    pub fn read_tracked_count(&self) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_tracked_count()),
        )
    }

    pub fn read_tracked_asset(&self, index: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_tracked_asset(index)),
        )
    }

    pub fn set_tracked_assets(&mut self, asset_ids: Vec<u64>) {
        self.backend.store(
            ORACLE_ADDRESS,
            slot_oracle_tracked_count(),
            u64_to_u256(asset_ids.len() as u64),
        );
        for (i, asset_id) in asset_ids.iter().enumerate() {
            self.backend.store(
                ORACLE_ADDRESS,
                slot_oracle_tracked_asset(i as u64),
                u64_to_u256(*asset_id),
            );
        }
        // Zero out any old entries beyond the new list
        let old_count = self.read_tracked_count();
        for i in asset_ids.len() as u64..old_count {
            self.backend
                .store(ORACLE_ADDRESS, slot_oracle_tracked_asset(i), U256::ZERO);
        }
    }

    pub fn submit_price(&mut self, asset_id: u64, price: u128, timestamp: u64, block_number: u64) {
        self.backend.store(
            ORACLE_ADDRESS,
            slot_oracle_price(asset_id),
            u128_to_u256(price),
        );
        self.backend.store(
            ORACLE_ADDRESS,
            slot_oracle_timestamp(asset_id),
            u64_to_u256(timestamp),
        );
        self.backend.store(
            ORACLE_ADDRESS,
            slot_oracle_block(asset_id),
            u64_to_u256(block_number),
        );

        let count = self.read_count(asset_id);
        let old_twap = self.read_twap(asset_id);
        let new_twap = if count == 0 {
            price
        } else {
            (old_twap * count as u128 + price) / (count as u128 + 1)
        };
        self.backend.store(
            ORACLE_ADDRESS,
            slot_oracle_twap(asset_id),
            u128_to_u256(new_twap),
        );
        self.backend.store(
            ORACLE_ADDRESS,
            slot_oracle_count(asset_id),
            u64_to_u256(count + 1),
        );
    }
}

// ── sol! interface ────────────────────────────────────────────────────

sol! {
    interface IProtocolOracle {
        function getPrice(uint64 assetId) external view returns (uint128);
        function getTWAP(uint64 assetId) external view returns (uint128);
        function isStale(uint64 assetId) external view returns (uint8);
        function submitPrice(uint64 assetId, uint128 price, uint64 timestamp, uint64 blockNumber) external;
        function setTrackedAssets(uint64[] assetIds) external;
    }
}

// ── OraclePrecompile ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct OraclePrecompile;

impl OraclePrecompile {
    fn get_price(&self, calldata: &[u8], storage: &mut dyn StorageProvider) -> PrecompileResult {
        let backend = JournalBackend::new(storage);
        dispatch::view::<IProtocolOracle::getPriceCall, _, _>(calldata, 1000, storage, |call, _| {
            let store = OracleStorage::new(backend);
            Ok(store.read_price(call.assetId))
        })
    }

    fn get_twap(&self, calldata: &[u8], storage: &mut dyn StorageProvider) -> PrecompileResult {
        let backend = JournalBackend::new(storage);
        dispatch::view::<IProtocolOracle::getTWAPCall, _, _>(calldata, 1000, storage, |call, _| {
            let store = OracleStorage::new(backend);
            Ok(store.read_twap(call.assetId))
        })
    }

    fn is_stale(&self, calldata: &[u8], storage: &mut dyn StorageProvider) -> PrecompileResult {
        let backend = JournalBackend::new(storage);
        let current_ts = storage.timestamp().to::<u64>();
        dispatch::view::<IProtocolOracle::isStaleCall, _, _>(calldata, 1000, storage, |call, _| {
            let store = OracleStorage::new(backend);
            Ok(U256::from(if store.is_stale(call.assetId, current_ts) {
                1u8
            } else {
                0u8
            }))
        })
    }

    fn submit_price(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolOracle::submitPriceCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;

                // Verify caller is a registered validator
                let validator_store = ValidatorStorage::new(JournalBackend::new(storage));
                let validator_id = validator_store.read_validator_id(caller);
                if validator_id == 0 {
                    return Err(PrecompileError::Other(
                        "oracle submit: caller is not a validator".into(),
                    ));
                }

                let mut store = OracleStorage::new(JournalBackend::new(storage));
                store.submit_price(call.assetId, call.price, call.timestamp, call.blockNumber);

                // Emit PriceSubmitted event
                let topic0 =
                    alloy_primitives::keccak256(b"PriceSubmitted(uint64,uint128,uint64,uint64)");
                let mut event_data = Vec::with_capacity(128);
                event_data.extend_from_slice(&u64_to_u256(call.assetId).to_be_bytes::<32>());
                event_data.extend_from_slice(&u128_to_u256(call.price).to_be_bytes::<32>());
                event_data.extend_from_slice(&u64_to_u256(call.timestamp).to_be_bytes::<32>());
                event_data.extend_from_slice(&u64_to_u256(call.blockNumber).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0],
                    alloy_primitives::Bytes::from(event_data),
                ) {
                    let _ = storage.emit_event(ORACLE_ADDRESS, log);
                }

                Ok(())
            },
        )
    }

    fn set_tracked_assets(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolOracle::setTrackedAssetsCall, _>(
            calldata,
            50_000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;

                // Verify caller is a registered validator
                let validator_store = ValidatorStorage::new(JournalBackend::new(storage));
                let validator_id = validator_store.read_validator_id(caller);
                if validator_id == 0 {
                    return Err(PrecompileError::Other(
                        "oracle setTrackedAssets: caller is not a validator".into(),
                    ));
                }

                let mut store = OracleStorage::new(JournalBackend::new(storage));
                store.set_tracked_assets(call.assetIds.to_vec());

                Ok(())
            },
        )
    }
}

impl call_precompile::StatefulPrecompile for OraclePrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
    ) -> PrecompileResult {
        if calldata.len() < 4 {
            return Err(PrecompileError::Other("invalid input".into()));
        }
        let selector: [u8; 4] = calldata[..4].try_into().unwrap();
        match selector {
            IProtocolOracle::getPriceCall::SELECTOR => self.get_price(calldata, storage),
            IProtocolOracle::getTWAPCall::SELECTOR => self.get_twap(calldata, storage),
            IProtocolOracle::isStaleCall::SELECTOR => self.is_stale(calldata, storage),
            IProtocolOracle::submitPriceCall::SELECTOR => {
                self.submit_price(calldata, msg_sender, storage)
            }
            IProtocolOracle::setTrackedAssetsCall::SELECTOR => {
                self.set_tracked_assets(calldata, msg_sender, storage)
            }
            _ => Err(PrecompileError::Other("unknown selector".into())),
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::VALIDATOR_ADDRESS;
    use call_precompile::{slot_validator_by_addr, u64_to_u256, StatefulPrecompile};

    #[test]
    fn test_oracle_address() {
        assert_eq!(
            ORACLE_ADDRESS,
            alloy_primitives::address!("0000000000000000000000000000000000000101")
        );
    }

    #[test]
    fn test_oracle_precompile_submit_and_get() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = Address::repeat_byte(0xAB);

        // Seed caller as a validator
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                slot_validator_by_addr(caller),
                u64_to_u256(1),
            )
            .unwrap();

        // submitPrice
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::submitPriceCall {
            assetId: 1,
            price: 2_000_000,
            timestamp: 1000,
            blockNumber: 100,
        }
        .abi_encode();

        let result = precompile.call(&input, caller, &mut provider);
        assert!(result.is_ok(), "submitPrice failed: {:?}", result.err());

        // Verify event emitted
        let logs = provider.events(ORACLE_ADDRESS);
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].topics()[0],
            alloy_primitives::keccak256(b"PriceSubmitted(uint64,uint128,uint64,uint64)")
        );

        // getPrice
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::getPriceCall { assetId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let price = u256_to_u128(U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        assert_eq!(price, 2_000_000);
    }

    #[test]
    fn test_oracle_precompile_twap_and_stale() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = Address::repeat_byte(0xAB);

        // Seed caller as a validator
        provider
            .sstore(
                VALIDATOR_ADDRESS,
                slot_validator_by_addr(caller),
                u64_to_u256(1),
            )
            .unwrap();

        // Submit two prices
        let mut precompile = OraclePrecompile;
        for (price, ts) in [(1_000_000u128, 900u64), (2_000_000, 1000)] {
            let input = IProtocolOracle::submitPriceCall {
                assetId: 1,
                price,
                timestamp: ts,
                blockNumber: 100,
            }
            .abi_encode();
            precompile.call(&input, caller, &mut provider).unwrap();
        }

        // getTWAP
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::getTWAPCall { assetId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        let twap = u256_to_u128(U256::from_be_bytes::<32>(
            result.bytes.as_ref().try_into().unwrap(),
        ));
        // Cumulative average: (1_000_000 + 2_000_000) / 2 = 1_500_000
        assert_eq!(twap, 1_500_000);

        // isStale with chain timestamp = 2000 (not stale, threshold = 3600)
        provider.set_timestamp(U256::from(2000));
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::isStaleCall { assetId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 0);

        // isStale with chain timestamp = 5000 (stale, 5000 - 1000 = 4000 > 3600)
        provider.set_timestamp(U256::from(5000));
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::isStaleCall { assetId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 1);
    }

    #[test]
    fn test_oracle_precompile_non_validator_rejected() {
        let mut provider = HashMapStorageProvider::new(1_000_000);
        let caller = Address::repeat_byte(0xCD);

        // No validator registration for caller

        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::submitPriceCall {
            assetId: 1,
            price: 1_000_000,
            timestamp: 1000,
            blockNumber: 100,
        }
        .abi_encode();

        let result = precompile.call(&input, caller, &mut provider);
        assert!(result.is_err(), "expected reject for non-validator");
    }
}
