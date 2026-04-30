//! Oracle precompile at 0x101 (per spec §25.4)
//!
//! Functions: getPrice(), getTWAP(), isStale(), submitPrice()

use alloy_primitives::address;

/// Precompile address
#[allow(dead_code)]
pub(crate) const ORACLE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000101");

// ── OraclePrecompile (stateful, uses StorageCtx) ──────────────────────

use crate::StatefulPrecompile;
use crate::storage::{storage_slot, StorageCtx};

/// Compute oracle storage slot for an asset field.
fn slot_oracle(asset_id: u64, suffix: &[u8]) -> alloy_primitives::U256 {
    storage_slot(&[&asset_id.to_be_bytes()[..], suffix])
}

/// Read u128 from low 16 bytes of a U256.
fn u256_to_u128(v: alloy_primitives::U256) -> u128 {
    let bytes = v.to_be_bytes::<32>();
    u128::from_be_bytes(bytes[16..32].try_into().unwrap())
}

/// Write u128 into low 16 bytes of a U256.
fn u128_to_u256(v: u128) -> alloy_primitives::U256 {
    let mut bytes = [0u8; 32];
    bytes[16..32].copy_from_slice(&v.to_be_bytes());
    alloy_primitives::U256::from_be_bytes::<32>(bytes)
}

/// Read u64 from low 8 bytes of a U256.
fn u256_to_u64(v: alloy_primitives::U256) -> u64 {
    let bytes = v.to_be_bytes::<32>();
    u64::from_be_bytes(bytes[24..32].try_into().unwrap())
}

/// Write u64 into low 8 bytes of a U256.
fn u64_to_u256(v: u64) -> alloy_primitives::U256 {
    let mut bytes = [0u8; 32];
    bytes[24..32].copy_from_slice(&v.to_be_bytes());
    alloy_primitives::U256::from_be_bytes::<32>(bytes)
}

/// Stateful oracle precompile backed by EVM storage.
#[derive(Debug, Default, Clone, Copy)]
pub struct OraclePrecompile;

impl OraclePrecompile {
    /// Staleness threshold in seconds (simplified; could be stored in config slot).
    const STALE_THRESHOLD_SECS: u64 = 3600;

    fn get_price(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let asset_id = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            if input.len() >= 36 {
                buf.copy_from_slice(&input[28..36]);
            }
            buf
        });

        let price = StorageCtx::sload(ORACLE_ADDRESS, slot_oracle(asset_id, b"price"))
            .map(u256_to_u128)
            .unwrap_or(0);

        let mut output = [0u8; 32];
        output[16..].copy_from_slice(&price.to_be_bytes());

        let out = revm_precompile::PrecompileOutput::new(0, output.to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn get_twap(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let asset_id = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            if input.len() >= 36 {
                buf.copy_from_slice(&input[28..36]);
            }
            buf
        });

        let twap = StorageCtx::sload(ORACLE_ADDRESS, slot_oracle(asset_id, b"twap"))
            .map(u256_to_u128)
            .unwrap_or(0);

        let mut output = [0u8; 32];
        output[16..].copy_from_slice(&twap.to_be_bytes());

        let out = revm_precompile::PrecompileOutput::new(0, output.to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn is_stale(&self, input: &[u8]) -> crate::PrecompileResult {
        const GAS_COST: u64 = 1000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;

        let asset_id = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            if input.len() >= 36 {
                buf.copy_from_slice(&input[28..36]);
            }
            buf
        });
        let current_ts = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            if input.len() >= 68 {
                buf.copy_from_slice(&input[60..68]);
            }
            buf
        });

        let stored_ts = StorageCtx::sload(ORACLE_ADDRESS, slot_oracle(asset_id, b"ts"))
            .map(u256_to_u64)
            .unwrap_or(0);

        let mut output = [0u8; 32];
        output[31] = if stored_ts + Self::STALE_THRESHOLD_SECS < current_ts { 1 } else { 0 };

        let out = revm_precompile::PrecompileOutput::new(0, output.to_vec().into());
        Ok(crate::storage::fill_precompile_output(out))
    }

    fn submit_price(&self, input: &[u8], msg_sender: alloy_primitives::Address) -> crate::PrecompileResult {
        const GAS_COST: u64 = 5000;
        StorageCtx::deduct_gas(GAS_COST).ok_or(revm_precompile::PrecompileError::OutOfGas)?;
        if input.len() < 132 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }

        let asset_id = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&input[28..36]);
            buf
        });
        let price = u128::from_be_bytes({
            let mut buf = [0u8; 16];
            buf.copy_from_slice(&input[48..64]);
            buf
        });
        let timestamp = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&input[92..100]);
            buf
        });
        let block_number = u64::from_be_bytes({
            let mut buf = [0u8; 8];
            buf.copy_from_slice(&input[124..132]);
            buf
        });

        // Verify caller is a registered validator
        let validator_id = StorageCtx::sload(crate::VALIDATOR_ADDRESS, crate::slot_validator_by_addr(msg_sender))
            .map(u256_to_u64)
            .unwrap_or(0);
        if validator_id == 0 {
            return Err(revm_precompile::PrecompileError::Other(
                "oracle submit: caller is not a validator".into(),
            ));
        }

        // Update price
        StorageCtx::sstore(ORACLE_ADDRESS, slot_oracle(asset_id, b"price"), u128_to_u256(price));
        StorageCtx::sstore(ORACLE_ADDRESS, slot_oracle(asset_id, b"ts"), u64_to_u256(timestamp));
        StorageCtx::sstore(ORACLE_ADDRESS, slot_oracle(asset_id, b"block"), u64_to_u256(block_number));

        // Simple cumulative average TWAP
        let count = StorageCtx::sload(ORACLE_ADDRESS, slot_oracle(asset_id, b"count"))
            .map(u256_to_u64)
            .unwrap_or(0);
        let old_twap = StorageCtx::sload(ORACLE_ADDRESS, slot_oracle(asset_id, b"twap"))
            .map(u256_to_u128)
            .unwrap_or(0);
        let new_twap = if count == 0 {
            price
        } else {
            (old_twap * count as u128 + price) / (count as u128 + 1)
        };
        StorageCtx::sstore(ORACLE_ADDRESS, slot_oracle(asset_id, b"twap"), u128_to_u256(new_twap));
        StorageCtx::sstore(ORACLE_ADDRESS, slot_oracle(asset_id, b"count"), u64_to_u256(count + 1));

        let out = revm_precompile::PrecompileOutput::new(0, alloy_primitives::Bytes::new());
        Ok(crate::storage::fill_precompile_output(out))
    }
}

impl StatefulPrecompile for OraclePrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: alloy_primitives::Address) -> crate::PrecompileResult {
        if calldata.len() < 4 {
            return Err(revm_precompile::PrecompileError::Other("invalid input".into()));
        }
        let selector = &calldata[..4];
        match selector {
            &[0x76, 0x3e, 0x4d, 0x8c] => self.get_price(calldata),
            &[0xab, 0xcd, 0xef, 0x01] => self.get_twap(calldata),
            &[0x12, 0x34, 0x56, 0x78] => self.is_stale(calldata),
            &[0x7a, 0xe9, 0x19, 0xf7] => self.submit_price(calldata, msg_sender),
            _ => Err(revm_precompile::PrecompileError::Other("unknown selector".into())),
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
            input[48..64].copy_from_slice(&2_000_000u128.to_be_bytes());
            input[92..100].copy_from_slice(&1000u64.to_be_bytes());
            input[124..132].copy_from_slice(&100u64.to_be_bytes());

            let result = precompile.call(&input, caller);
            assert!(result.is_ok(), "submitPrice failed: {:?}", result.err());

            // getPrice(assetId=1)
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

        crate::storage::StorageCtx::enter(&mut provider, || {
            let mut precompile = OraclePrecompile;
            let caller = alloy_primitives::Address::repeat_byte(0xAB);

            // Seed caller as a validator
            crate::storage::StorageCtx::sstore(
                crate::VALIDATOR_ADDRESS,
                crate::slot_validator_by_addr(caller),
                crate::u64_to_u256(1),
            );

            // Submit two prices
            for (price, ts) in [(1_000_000u128, 900u64), (2_000_000, 1000)] {
                let mut input = vec![0u8; 132];
                input[0..4].copy_from_slice(&[0x7a, 0xe9, 0x19, 0xf7]);
                input[28..36].copy_from_slice(&1u64.to_be_bytes());
                input[48..64].copy_from_slice(&price.to_be_bytes());
                input[92..100].copy_from_slice(&ts.to_be_bytes());
                input[124..132].copy_from_slice(&100u64.to_be_bytes());
                precompile.call(&input, caller).unwrap();
            }

            // getTWAP
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

            // isStale with current_ts = 2000 (not stale, threshold = 3600)
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[60..68].copy_from_slice(&2000u64.to_be_bytes());

            let result = precompile.call(&input, alloy_primitives::Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 0);

            // isStale with current_ts = 5000 (stale, 5000 - 1000 = 4000 > 3600)
            let mut input = vec![0u8; 68];
            input[0..4].copy_from_slice(&[0x12, 0x34, 0x56, 0x78]);
            input[28..36].copy_from_slice(&1u64.to_be_bytes());
            input[60..68].copy_from_slice(&5000u64.to_be_bytes());

            let result = precompile.call(&input, alloy_primitives::Address::ZERO).unwrap();
            assert_eq!(result.bytes[31], 1);
        });
    }
}
