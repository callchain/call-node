//! Oracle precompile at 0x101 (per spec §25.4)
//!
//! Functions: getPrice(), getTWAP(), isStale(), submitPrice()

use alloy_primitives::{address, Bytes, LogData};

/// Precompile address
pub const ORACLE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000101");

// ── OraclePrecompile (stateful, uses StorageCtx) ──────────────────────

use crate::StatefulPrecompile;
use crate::storage::StorageCtx;
use crate::{
    decode_u128, decode_u64, encode_u128, encode_u8, ok_empty, slot_asset_meta, u128_to_u256,
    u256_to_u128, u256_to_u64, u64_to_u256,
};

/// Stateful oracle precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct OraclePrecompile;

impl OraclePrecompile {
    /// Staleness threshold in seconds (simplified; could be stored in config slot).
    const STALE_THRESHOLD_SECS: u64 = 3600;

    fn get_price(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST)
            .ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid assetId".into()))?;

        let price = StorageCtx::sload(ORACLE_ADDRESS, slot_asset_meta(asset_id, b"price"))
            .map(u256_to_u128)
            .unwrap_or(0);

        let out = revm_precompile::PrecompileOutput::new(0, encode_u128(price).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn get_twap(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST)
            .ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid assetId".into()))?;

        let twap = StorageCtx::sload(ORACLE_ADDRESS, slot_asset_meta(asset_id, b"twap"))
            .map(u256_to_u128)
            .unwrap_or(0);

        let out = revm_precompile::PrecompileOutput::new(0, encode_u128(twap).to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn is_stale(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST)
            .ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid assetId".into()))?;

        let current_ts = StorageCtx::timestamp().to::<u64>();

        let stored_ts = StorageCtx::sload(ORACLE_ADDRESS, slot_asset_meta(asset_id, b"ts"))
            .map(u256_to_u64)
            .unwrap_or(0);

        let stale = stored_ts + Self::STALE_THRESHOLD_SECS < current_ts;
        let out = revm_precompile::PrecompileOutput::new(
            0,
            encode_u8(if stale { 1 } else { 0 }).to_vec().into(),
        );
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn submit_price(
        &self,
        input: &[u8],
        msg_sender: alloy_primitives::Address,
    ) -> crate::PrecompileResult {
        const GAS_COST: u64 = 30_000;
        StorageCtx::deduct_gas(GAS_COST)
            .ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(revm_precompile::PrecompileError::Other(
                "invalid input".into(),
            ));
        }

        let asset_id = decode_u64(input, 4)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid assetId".into()))?;
        let price = decode_u128(input, 36)
            .ok_or_else(|| revm_precompile::PrecompileError::Other("invalid price".into()))?;
        let timestamp = decode_u64(input, 68).ok_or_else(|| {
            revm_precompile::PrecompileError::Other("invalid timestamp".into())
        })?;
        let block_number = decode_u64(input, 100).ok_or_else(|| {
            revm_precompile::PrecompileError::Other("invalid blockNumber".into())
        })?;

        // Verify caller is a registered validator
        let validator_id = StorageCtx::sload(
            crate::VALIDATOR_ADDRESS,
            crate::slot_validator_by_addr(msg_sender),
        )
        .map(u256_to_u64)
        .unwrap_or(0);
        if validator_id == 0 {
            return Err(revm_precompile::PrecompileError::Other(
                "oracle submit: caller is not a validator".into(),
            ));
        }

        // Update price
        StorageCtx::sstore(
            ORACLE_ADDRESS,
            slot_asset_meta(asset_id, b"price"),
            u128_to_u256(price),
        );
        StorageCtx::sstore(
            ORACLE_ADDRESS,
            slot_asset_meta(asset_id, b"ts"),
            u64_to_u256(timestamp),
        );
        StorageCtx::sstore(
            ORACLE_ADDRESS,
            slot_asset_meta(asset_id, b"block"),
            u64_to_u256(block_number),
        );

        // Simple cumulative average TWAP
        let count = StorageCtx::sload(ORACLE_ADDRESS, slot_asset_meta(asset_id, b"count"))
            .map(u256_to_u64)
            .unwrap_or(0);
        let old_twap = StorageCtx::sload(ORACLE_ADDRESS, slot_asset_meta(asset_id, b"twap"))
            .map(u256_to_u128)
            .unwrap_or(0);
        let new_twap = if count == 0 {
            price
        } else {
            (old_twap * count as u128 + price) / (count as u128 + 1)
        };
        StorageCtx::sstore(
            ORACLE_ADDRESS,
            slot_asset_meta(asset_id, b"twap"),
            u128_to_u256(new_twap),
        );
        StorageCtx::sstore(
            ORACLE_ADDRESS,
            slot_asset_meta(asset_id, b"count"),
            u64_to_u256(count + 1),
        );

        // Emit PriceSubmitted(assetId, price, timestamp, blockNumber)
        let topic0 = alloy_primitives::keccak256(b"PriceSubmitted(uint64,uint128,uint64,uint64)");
        let mut event_data = Vec::with_capacity(128);
        event_data.extend_from_slice(&u64_to_u256(asset_id).to_be_bytes::<32>());
        event_data.extend_from_slice(&u128_to_u256(price).to_be_bytes::<32>());
        event_data.extend_from_slice(&u64_to_u256(timestamp).to_be_bytes::<32>());
        event_data.extend_from_slice(&u64_to_u256(block_number).to_be_bytes::<32>());
        if let Some(log) = LogData::new(vec![topic0], Bytes::from(event_data)) {
            let _ = StorageCtx::emit_event(ORACLE_ADDRESS, log);
        }

        ok_empty()
    }
}

impl StatefulPrecompile for OraclePrecompile {
    fn call(
        &mut self,
        calldata: &[u8],
        msg_sender: alloy_primitives::Address,
    ) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(revm_precompile::PrecompileError::Other(
                "invalid input".into(),
            ));
        }
        let selector = &calldata[..4];
        match selector {
            &[0x76, 0x3e, 0x4d, 0x8c] => self.get_price(calldata),
            &[0xab, 0xcd, 0xef, 0x01] => self.get_twap(calldata),
            &[0x12, 0x34, 0x56, 0x78] => self.is_stale(calldata),
            &[0x7a, 0xe9, 0x19, 0xf7] => self.submit_price(calldata, msg_sender),
            _ => Err(revm_precompile::PrecompileError::Other(
                "unknown selector".into(),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── OraclePrecompile tests (stateful, using HashMapStorageProvider) ──

    #[test]
    fn test_oracle_precompile_stateful_submit_and_get() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);

        // submitPrice
        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = OraclePrecompile;
            let caller = alloy_primitives::Address::repeat_byte(0xAB);

            // Seed caller as a validator
            crate::storage::StorageCtx::sstore(
                crate::VALIDATOR_ADDRESS,
                crate::slot_validator_by_addr(caller),
                crate::u64_to_u256(1),
            );

            // submitPrice(assetId=1, price=2_000_000, timestamp=1000, block=100)
            let mut input = vec![0u8; 132];
            input[0..4].copy_from_slice(&[0x7a, 0xe9, 0x19, 0xf7]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[52..68].copy_from_slice(&2_000_000u128.to_be_bytes());
            input[92..100].copy_from_slice(&1000u64.to_be_bytes());
            input[124..132].copy_from_slice(&100u64.to_be_bytes());

            let result = precompile.call(&input, caller);
            assert!(result.is_ok(), "submitPrice failed: {:?}", result.err());
        });

        // Verify event emitted
        let logs = provider.events(ORACLE_ADDRESS);
        assert_eq!(logs.len(), 1);
        assert_eq!(
            logs[0].topics()[0],
            alloy_primitives::keccak256(b"PriceSubmitted(uint64,uint128,uint64,uint64)")
        );

        // getPrice
        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = OraclePrecompile;

            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x76, 0x3e, 0x4d, 0x8c]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());

            let result = precompile.call(&input, alloy_primitives::Address::ZERO);
            assert!(result.is_ok(), "getPrice failed: {:?}", result.err());
            let price = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.unwrap().bytes[16..32]);
                buf
            });
            assert_eq!(price, 2_000_000);
        });
    }

    #[test]
    fn test_oracle_precompile_stateful_twap_and_stale() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);

        // Seed validator and submit two prices
        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = OraclePrecompile;
            let caller = alloy_primitives::Address::repeat_byte(0xAB);

            crate::storage::StorageCtx::sstore(
                crate::VALIDATOR_ADDRESS,
                crate::slot_validator_by_addr(caller),
                crate::u64_to_u256(1),
            );

            for (price, ts) in [(1_000_000u128, 900u64), (2_000_000, 1000)] {
                let mut input = vec![0u8; 132];
                input[0..4].copy_from_slice(&[0x7a, 0xe9, 0x19, 0xf7]);
                input[28..36].copy_from_slice(&1u64.to_be_bytes());
                input[52..68].copy_from_slice(&price.to_be_bytes());
                input[92..100].copy_from_slice(&ts.to_be_bytes());
                input[124..132].copy_from_slice(&100u64.to_be_bytes());
                precompile.call(&input, caller).unwrap();
            }
        });

        // getTWAP
        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = OraclePrecompile;

            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0xab, 0xcd, 0xef, 0x01]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[60..68].copy_from_slice(&1100u64.to_be_bytes());

            let result = precompile.call(&input, alloy_primitives::Address::ZERO).unwrap();
            let twap = u128::from_be_bytes({
                let mut buf = [0u8; 16];
                buf.copy_from_slice(&result.bytes[16..32]);
                buf
            });
            // Cumulative average: (1_000_000 + 2_000_000) / 2 = 1_500_000
            assert_eq!(twap, 1_500_000);
        });

        // isStale with chain timestamp = 2000 (not stale, threshold = 3600)
        provider.set_timestamp(alloy_primitives::U256::from(2000));
        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = OraclePrecompile;

            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());

            let result = precompile.call(&input, alloy_primitives::Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 0);
        });

        // isStale with chain timestamp = 5000 (stale, 5000 - 1000 = 4000 > 3600)
        provider.set_timestamp(alloy_primitives::U256::from(5000));
        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = OraclePrecompile;

            let mut input = vec![0u8; 36];
            input[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());

            let result = precompile.call(&input, alloy_primitives::Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1);
        });
    }

    #[test]
    fn test_oracle_precompile_decode_failure_reverts() {
        let mut provider = crate::storage::HashMapStorageProvider::new(1_000_000);

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = OraclePrecompile;

            // getPrice with truncated input (no assetId word)
            let input = vec![0x76, 0x3e, 0x4d, 0x8c];
            let result = precompile.call(&input, alloy_primitives::Address::ZERO);
            assert!(result.is_err(), "expected revert for truncated getPrice input");

            // isStale with truncated input
            let input = vec![0x12, 0x34, 0x56, 0x78];
            let result = precompile.call(&input, alloy_primitives::Address::ZERO);
            assert!(result.is_err(), "expected revert for truncated isStale input");
        });
    }
}
