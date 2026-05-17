//! Oracle precompile entry point (0x101).
//!
//! Thin wrapper that routes EVM calls to [`OracleStorage`] backed by
//! EVM storage.  Business logic lives in [`OracleStorage`]; this
//! file only handles ABI decode/encode, gas accounting and selector dispatch.

use alloy_sol_types::{sol, SolCall};
use call_precompile::{
    dispatch, require_caller,
    storage::{storage_slot, StorageProvider},
    u128_to_u256, u256_to_u128, u256_to_u64, u64_to_u256, StorageRef,
    GOVERNANCE_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;
use call_validator::ValidatorStorage;
use revm_precompile::{PrecompileError, PrecompileResult};

pub const ORACLE_ADDRESS: Address =
    alloy_primitives::address!("0000000000000000000000000000000000000101");

pub const STALE_THRESHOLD_SECS: u64 = crate::constants::ORACLE_STALENESS_SECS;

/// Maximum number of samples in the TWAP cumulative average before reset.
pub const MAX_TWAP_COUNT: u64 = 10_000;

/// Maximum number of assets that can be tracked simultaneously.
pub const MAX_TRACKED_ASSETS: usize = 1_000;

/// Minimum valid price (0 is rejected to avoid ambiguity).
pub const MIN_PRICE: u128 = 1;
/// Maximum valid price. 10^18 corresponds to ~10^12 USD with 6-decimal precision.
pub const MAX_PRICE: u128 = 1_000_000_000_000_000_000;

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

fn slot_oracle_tracked_flag(asset_id: u64) -> U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], b"flag"])
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

    pub fn read_price(&mut self, asset_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_price(asset_id)),
        )
    }

    pub fn read_twap(&mut self, asset_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_twap(asset_id)),
        )
    }

    pub fn read_timestamp(&mut self, asset_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_timestamp(asset_id)),
        )
    }

    pub fn read_block(&mut self, asset_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_block(asset_id)),
        )
    }

    pub fn read_count(&mut self, asset_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_count(asset_id)),
        )
    }

    pub fn is_stale(&mut self, asset_id: u64, current_ts: u64) -> bool {
        let stored_ts = self.read_timestamp(asset_id);
        stored_ts.saturating_add(STALE_THRESHOLD_SECS) < current_ts
    }

    pub fn read_tracked_count(&mut self) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_tracked_count()),
        )
    }

    pub fn read_tracked_asset(&mut self, index: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(ORACLE_ADDRESS, slot_oracle_tracked_asset(index)),
        )
    }

    /// Check whether an asset is currently in the tracked list (O(1)).
    pub fn is_asset_tracked(&mut self, asset_id: u64) -> bool {
        let flag = self.backend.load(ORACLE_ADDRESS, slot_oracle_tracked_flag(asset_id));
        flag != U256::ZERO
    }

    pub fn set_tracked_assets(&mut self, asset_ids: Vec<u64>) {
        // Read old count BEFORE overwriting it
        let old_count = self.read_tracked_count();
        // Collect old asset IDs so we can clear their O(1) flags
        let old_assets: Vec<u64> = (0..old_count)
            .map(|i| self.read_tracked_asset(i))
            .collect();
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
            // Set O(1) tracked flag
            self.backend.store(
                ORACLE_ADDRESS,
                slot_oracle_tracked_flag(*asset_id),
                U256::from(1),
            );
        }
        // Zero out any old entries beyond the new list and clear flags for
        // assets that are no longer tracked.
        for i in asset_ids.len() as u64..old_count {
            self.backend
                .store(ORACLE_ADDRESS, slot_oracle_tracked_asset(i), U256::ZERO);
        }
        for old_asset in old_assets {
            if !asset_ids.contains(&old_asset) {
                self.backend.store(
                    ORACLE_ADDRESS,
                    slot_oracle_tracked_flag(old_asset),
                    U256::ZERO,
                );
            }
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
            // Use U256 for intermediate calculation to prevent u128 overflow.
            let old = U256::from(old_twap);
            let cnt = U256::from(count);
            let prc = U256::from(price);
            let numerator = old * cnt + prc;
            let denominator = cnt + U256::from(1);
            let result = numerator / denominator;
            // Saturate at u128::MAX if the result is out of range.
            if result > U256::from(u128::MAX) {
                u128::MAX
            } else {
                result.to::<u128>()
            }
        };
        self.backend.store(
            ORACLE_ADDRESS,
            slot_oracle_twap(asset_id),
            u128_to_u256(new_twap),
        );

        // Cap the TWAP sample count to prevent unbounded growth.
        // Reset to half the cap so historical weight is preserved and
        // responsiveness does not suddenly jump.
        let new_count = if count >= MAX_TWAP_COUNT {
            MAX_TWAP_COUNT / 2
        } else {
            count + 1
        };
        self.backend.store(
            ORACLE_ADDRESS,
            slot_oracle_count(asset_id),
            u64_to_u256(new_count),
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
        function getPrices(uint64[] assetIds) external view returns (uint128[]);
        function getTWAPs(uint64[] assetIds) external view returns (uint128[]);
    }
}

// ── OraclePrecompile ──────────────────────────────────────────────────

#[derive(Debug, Default, Clone, Copy)]
pub struct OraclePrecompile;

impl OraclePrecompile {
    fn get_price(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolOracle::getPriceCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = OracleStorage::new(sr);
                if !store.is_asset_tracked(call.assetId) {
                    return Err(PrecompileError::Other(
                        "oracle: asset not tracked".into(),
                    ));
                }
                let price = store.read_price(call.assetId);
                if price == 0 {
                    return Err(PrecompileError::Other(
                        "oracle: price not initialized".into(),
                    ));
                }
                Ok(price)
            },
        )
    }

    fn get_twap(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::view::<IProtocolOracle::getTWAPCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = OracleStorage::new(sr);
                if !store.is_asset_tracked(call.assetId) {
                    return Err(PrecompileError::Other(
                        "oracle: asset not tracked".into(),
                    ));
                }
                let twap = store.read_twap(call.assetId);
                if twap == 0 {
                    return Err(PrecompileError::Other(
                        "oracle: twap not initialized".into(),
                    ));
                }
                Ok(twap)
            },
        )
    }

    fn get_prices(
        &self,
        calldata: &[u8],
        _storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let call = IProtocolOracle::getPricesCall::abi_decode(calldata)
            .map_err(|_| PrecompileError::Other("decode failed".into()))?;

        let mut store = OracleStorage::new(sr);
        let mut prices = Vec::with_capacity(call.assetIds.len());
        for asset_id in &call.assetIds {
            if !store.is_asset_tracked(*asset_id) {
                return Err(PrecompileError::Other(
                    "oracle: asset not tracked".into(),
                ));
            }
            prices.push(U256::from(store.read_price(*asset_id)));
        }

        // Manual ABI encoding for uint128[]: offset || length || items
        let mut encoded = Vec::with_capacity(64 + prices.len() * 32);
        encoded.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
        encoded.extend_from_slice(&U256::from(prices.len()).to_be_bytes::<32>());
        for price in prices {
            encoded.extend_from_slice(&price.to_be_bytes::<32>());
        }

        let gas = 1000u64 + 500u64 * call.assetIds.len().max(1) as u64;
        Ok(revm_precompile::PrecompileOutput::new(
            gas,
            alloy_primitives::Bytes::from(encoded),
        ))
    }

    fn get_twap_batch(
        &self,
        calldata: &[u8],
        _storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let call = IProtocolOracle::getTWAPsCall::abi_decode(calldata)
            .map_err(|_| PrecompileError::Other("decode failed".into()))?;

        let mut store = OracleStorage::new(sr);
        let mut twaps = Vec::with_capacity(call.assetIds.len());
        for asset_id in &call.assetIds {
            if !store.is_asset_tracked(*asset_id) {
                return Err(PrecompileError::Other(
                    "oracle: asset not tracked".into(),
                ));
            }
            twaps.push(U256::from(store.read_twap(*asset_id)));
        }

        let mut encoded = Vec::with_capacity(64 + twaps.len() * 32);
        encoded.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
        encoded.extend_from_slice(&U256::from(twaps.len()).to_be_bytes::<32>());
        for twap in twaps {
            encoded.extend_from_slice(&twap.to_be_bytes::<32>());
        }

        let gas = 1000u64 + 500u64 * call.assetIds.len().max(1) as u64;
        Ok(revm_precompile::PrecompileOutput::new(
            gas,
            alloy_primitives::Bytes::from(encoded),
        ))
    }

    fn is_stale(
        &self,
        calldata: &[u8],
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        let current_ts = storage.timestamp().to::<u64>();
        dispatch::view::<IProtocolOracle::isStaleCall, _, _>(
            calldata,
            1000,
            storage,
            |call, _storage| {
                let mut store = OracleStorage::new(sr);
                if !store.is_asset_tracked(call.assetId) {
                    return Err(PrecompileError::Other(
                        "oracle: asset not tracked".into(),
                    ));
                }
                Ok(U256::from(if store.is_stale(call.assetId, current_ts) {
                    1u8
                } else {
                    0u8
                }))
            },
        )
    }

    fn submit_price(
        &self,
        calldata: &[u8],
        msg_sender: Address,
        storage: &mut dyn StorageProvider,
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolOracle::submitPriceCall, _>(
            calldata,
            30000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;

                // Verify caller is a registered validator
                let validator_id = {
                    let mut validator_store = ValidatorStorage::new(sr);
                    validator_store.read_validator_id(caller)
                };
                if validator_id == 0 {
                    return Err(PrecompileError::Other(
                        "oracle submit: caller is not a validator".into(),
                    ));
                }

                // Price validation
                if call.price < MIN_PRICE || call.price > MAX_PRICE {
                    return Err(PrecompileError::Other(
                        "oracle submit: price out of range".into(),
                    ));
                }

                let current_ts = storage.timestamp().to::<u64>();
                let current_block = storage.block_number();

                // Timestamp validation: not in the future (allow 60s clock drift),
                // and not older than 5 minutes.
                const TS_FUTURE_GRACE: u64 = 60;
                const TS_MAX_AGE: u64 = 300;
                if call.timestamp > current_ts + TS_FUTURE_GRACE {
                    return Err(PrecompileError::Other(
                        "oracle submit: timestamp in the future".into(),
                    ));
                }
                if call.timestamp + TS_MAX_AGE < current_ts {
                    return Err(PrecompileError::Other(
                        "oracle submit: timestamp too old".into(),
                    ));
                }

                // Block number validation: not in the future, not older than 10 blocks.
                const BLOCK_MAX_DELTA: u64 = 10;
                if call.blockNumber > current_block {
                    return Err(PrecompileError::Other(
                        "oracle submit: blockNumber in the future".into(),
                    ));
                }
                if call.blockNumber + BLOCK_MAX_DELTA < current_block {
                    return Err(PrecompileError::Other(
                        "oracle submit: blockNumber too old".into(),
                    ));
                }

                let mut store = OracleStorage::new(sr);
                if !store.is_asset_tracked(call.assetId) {
                    return Err(PrecompileError::Other(
                        "oracle submit: asset not tracked".into(),
                    ));
                }

                store.submit_price(call.assetId, call.price, call.timestamp, call.blockNumber);

                // Emit PriceSubmitted event
                // Signature: PriceSubmitted(address indexed validator,
                //   uint64 indexed assetId, uint128 price, uint64 timestamp,
                //   uint64 blockNumber)
                let topic0 = alloy_primitives::keccak256(
                    b"PriceSubmitted(address,uint64,uint128,uint64,uint64)",
                );
                let mut addr_bytes = [0u8; 32];
                addr_bytes[12..].copy_from_slice(caller.as_slice());
                let topic1 = alloy_primitives::B256::from(addr_bytes);
                let topic2 = alloy_primitives::B256::from(
                    u64_to_u256(call.assetId).to_be_bytes::<32>(),
                );
                let mut event_data = Vec::with_capacity(96);
                event_data.extend_from_slice(&u128_to_u256(call.price).to_be_bytes::<32>());
                event_data.extend_from_slice(&u64_to_u256(call.timestamp).to_be_bytes::<32>());
                event_data.extend_from_slice(&u64_to_u256(call.blockNumber).to_be_bytes::<32>());
                if let Some(log) = alloy_primitives::LogData::new(
                    vec![topic0, topic1, topic2],
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
        sr: StorageRef,
    ) -> PrecompileResult {
        dispatch::mutate_void::<IProtocolOracle::setTrackedAssetsCall, _>(
            calldata,
            50_000,
            storage,
            |call, storage| {
                let caller = require_caller(msg_sender)?;

                // Only governance can modify the tracked asset list.
                if caller != GOVERNANCE_ADDRESS {
                    return Err(PrecompileError::Other(
                        "oracle setTrackedAssets: caller is not governance".into(),
                    ));
                }

                // Limit the number of tracked assets to prevent gas exhaustion.
                if call.assetIds.len() > MAX_TRACKED_ASSETS {
                    return Err(PrecompileError::Other(
                        format!(
                            "oracle setTrackedAssets: too many assets, max {}",
                            MAX_TRACKED_ASSETS
                        )
                        .into(),
                    ));
                }

                let mut store = OracleStorage::new(sr);
                store.set_tracked_assets(call.assetIds.to_vec());

                // Emit TrackedAssetsUpdated(uint64[]) event.
                let topic0 =
                    alloy_primitives::keccak256(b"TrackedAssetsUpdated(uint64[])");
                let mut event_data = Vec::with_capacity(64 + call.assetIds.len() * 32);
                // ABI offset to dynamic data
                event_data.extend_from_slice(&U256::from(32).to_be_bytes::<32>());
                // Array length
                event_data.extend_from_slice(&U256::from(call.assetIds.len()).to_be_bytes::<32>());
                // Array items (each padded to 32 bytes)
                for id in &call.assetIds {
                    event_data.extend_from_slice(&u64_to_u256(*id).to_be_bytes::<32>());
                }
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
        let selector: [u8; 4] = calldata[..4]
            .try_into()
            .expect("invariant: 4-byte selector");
        let sr = StorageRef::new(storage);
        match selector {
            IProtocolOracle::getPriceCall::SELECTOR => self.get_price(calldata, storage, sr),
            IProtocolOracle::getTWAPCall::SELECTOR => self.get_twap(calldata, storage, sr),
            IProtocolOracle::getPricesCall::SELECTOR => self.get_prices(calldata, storage, sr),
            IProtocolOracle::getTWAPsCall::SELECTOR => {
                self.get_twap_batch(calldata, storage, sr)
            }
            IProtocolOracle::isStaleCall::SELECTOR => self.is_stale(calldata, storage, sr),
            IProtocolOracle::submitPriceCall::SELECTOR => {
                self.submit_price(calldata, msg_sender, storage, sr)
            }
            IProtocolOracle::setTrackedAssetsCall::SELECTOR => {
                self.set_tracked_assets(calldata, msg_sender, storage, sr)
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

        // Set chain context so submitted timestamp/blockNumber are valid
        provider.set_timestamp(U256::from(1000));
        provider.set_block_number(100);

        // Track asset 1 first (required before submit/get) — must be called by governance
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::setTrackedAssetsCall {
            assetIds: vec![1],
        }
        .abi_encode();
        precompile.call(&input, GOVERNANCE_ADDRESS, &mut provider).unwrap();

        // Verify TrackedAssetsUpdated event emitted
        let logs = provider.events(ORACLE_ADDRESS);
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].topics()[0],
            alloy_primitives::keccak256(b"TrackedAssetsUpdated(uint64[])")
        );

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

        // Verify PriceSubmitted event emitted
        let logs = provider.events(ORACLE_ADDRESS);
        let price_logs: Vec<_> = logs
            .iter()
            .filter(|l| {
                l.topics()[0]
                    == alloy_primitives::keccak256(b"PriceSubmitted(address,uint64,uint128,uint64,uint64)")
            })
            .collect();
        assert_eq!(price_logs.len(), 1);

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

        // Set chain context so submitted timestamp/blockNumber are valid
        provider.set_timestamp(U256::from(1000));
        provider.set_block_number(100);

        // Track asset 1 first — must be called by governance
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::setTrackedAssetsCall {
            assetIds: vec![1],
        }
        .abi_encode();
        precompile.call(&input, GOVERNANCE_ADDRESS, &mut provider).unwrap();

        // Submit two prices to build a cumulative TWAP.
        let mut precompile = OraclePrecompile;
        for (price, ts, blk) in [
            (1_000_000u128, 900u64, 100u64),
            (2_000_000, 1200, 101),
        ] {
            provider.set_timestamp(U256::from(ts));
            provider.set_block_number(blk);
            let input = IProtocolOracle::submitPriceCall {
                assetId: 1,
                price,
                timestamp: ts,
                blockNumber: blk,
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

        // isStale with chain timestamp = 1800 (not stale, stored=1200, threshold=900, 1200+900=2100 > 1800)
        provider.set_timestamp(U256::from(1800));
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::isStaleCall { assetId: 1 }.abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();
        assert_eq!(result.bytes[31], 0);

        // isStale with chain timestamp = 2200 (stale, 1200+900=2100 < 2200)
        provider.set_timestamp(U256::from(2200));
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

    #[test]
    fn test_oracle_precompile_batch_queries() {
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

        provider.set_timestamp(U256::from(1000));
        provider.set_block_number(100);

        // Track assets 1 and 2
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::setTrackedAssetsCall {
            assetIds: vec![1, 2],
        }
        .abi_encode();
        precompile.call(&input, GOVERNANCE_ADDRESS, &mut provider).unwrap();

        // Submit prices for both assets
        for (asset_id, price) in [(1, 1_000_000u128), (2, 2_000_000u128)] {
            let input = IProtocolOracle::submitPriceCall {
                assetId: asset_id,
                price,
                timestamp: 1000,
                blockNumber: 100,
            }
            .abi_encode();
            precompile.call(&input, caller, &mut provider).unwrap();
        }

        // getPrices batch
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::getPricesCall {
            assetIds: vec![1, 2],
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();

        // Decode uint128[]: offset (32) || length (2) || price1 || price2
        let bytes = result.bytes.as_ref();
        let offset = u32::from_be_bytes(bytes[28..32].try_into().unwrap()) as usize;
        let len = u32::from_be_bytes(bytes[offset + 28..offset + 32].try_into().unwrap()) as usize;
        assert_eq!(len, 2);
        let p1 = u256_to_u128(U256::from_be_bytes::<32>(bytes[offset + 32..offset + 64].try_into().unwrap()));
        let p2 = u256_to_u128(U256::from_be_bytes::<32>(bytes[offset + 64..offset + 96].try_into().unwrap()));
        assert_eq!(p1, 1_000_000);
        assert_eq!(p2, 2_000_000);

        // getTWAPs batch
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::getTWAPsCall {
            assetIds: vec![1, 2],
        }
        .abi_encode();
        let result = precompile
            .call(&input, Address::ZERO, &mut provider)
            .unwrap();

        let bytes = result.bytes.as_ref();
        let offset = u32::from_be_bytes(bytes[28..32].try_into().unwrap()) as usize;
        let len = u32::from_be_bytes(bytes[offset + 28..offset + 32].try_into().unwrap()) as usize;
        assert_eq!(len, 2);
        let t1 = u256_to_u128(U256::from_be_bytes::<32>(bytes[offset + 32..offset + 64].try_into().unwrap()));
        let t2 = u256_to_u128(U256::from_be_bytes::<32>(bytes[offset + 64..offset + 96].try_into().unwrap()));
        assert_eq!(t1, 1_000_000);
        assert_eq!(t2, 2_000_000);
    }

    #[test]
    fn test_oracle_precompile_batch_rejects_untracked() {
        let mut provider = HashMapStorageProvider::new(1_000_000);

        // No assets tracked
        let mut precompile = OraclePrecompile;
        let input = IProtocolOracle::getPricesCall {
            assetIds: vec![99],
        }
        .abi_encode();
        let result = precompile.call(&input, Address::ZERO, &mut provider);
        assert!(result.is_err(), "expected reject for untracked asset in batch");
    }
}
