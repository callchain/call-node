//! Oracle precompile at 0x101 (per spec §25.4)
//!
//! Functions: getPrice(), getTWAP(), isStale(), getOracleStatus()

use call_primitives::AssetId;
use alloy_primitives::address;
use std::collections::HashMap;

/// Precompile address
pub const ORACLE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000101");

/// Price entry for an asset
#[derive(Debug, Clone)]
pub struct OraclePrice {
    pub price: u128,
    pub timestamp: u64,
    pub block_number: u64,
}

/// Oracle status
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OracleStatus {
    Active,
    Stale,
    Disabled,
}

/// Oracle state
#[derive(Debug, Default)]
pub struct OracleState {
    prices: HashMap<AssetId, Vec<OraclePrice>>,
    stale_threshold_secs: u64,
}

impl OracleState {
    pub fn new(stale_threshold_secs: u64) -> Self {
        Self {
            prices: HashMap::new(),
            stale_threshold_secs,
        }
    }

    pub fn get_price(&self, asset_id: AssetId) -> Option<&OraclePrice> {
        self.prices.get(&asset_id).and_then(|v| v.last())
    }

    pub fn get_twapped(&self, asset_id: AssetId, period_secs: u64) -> Option<u128> {
        let prices = self.prices.get(&asset_id)?;
        if prices.is_empty() {
            return None;
        }
        let latest_ts = prices.last().unwrap().timestamp;
        let cutoff = latest_ts.saturating_sub(period_secs);
        let relevant: Vec<_> = prices.iter().filter(|p| p.timestamp >= cutoff).collect();
        if relevant.is_empty() {
            return None;
        }
        let sum: u128 = relevant.iter().map(|p| p.price).sum();
        Some(sum / relevant.len() as u128)
    }

    pub fn is_stale(&self, asset_id: AssetId, current_timestamp: u64) -> bool {
        self.prices
            .get(&asset_id)
            .and_then(|v| v.last())
            .map(|p| current_timestamp - p.timestamp > self.stale_threshold_secs)
            .unwrap_or(true)
    }

    pub fn get_oracle_status(&self, asset_id: AssetId, current_timestamp: u64) -> OracleStatus {
        if !self.prices.contains_key(&asset_id) {
            return OracleStatus::Disabled;
        }
        if self.is_stale(asset_id, current_timestamp) {
            OracleStatus::Stale
        } else {
            OracleStatus::Active
        }
    }

    pub fn submit_price(
        &mut self,
        asset_id: AssetId,
        price: u128,
        timestamp: u64,
        block_number: u64,
    ) {
        self.prices
            .entry(asset_id)
            .or_default()
            .push(OraclePrice {
                price,
                timestamp,
                block_number,
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_oracle_precompile_get_price() {
        let mut state = OracleState::new(3600);
        state.submit_price(1, 2_000_000, 1000, 100);
        assert_eq!(state.get_price(1).map(|p| p.price), Some(2_000_000));
        assert!(state.get_price(999).is_none());
    }

    #[test]
    fn test_oracle_precompile_get_twapped() {
        let mut state = OracleState::new(3600);
        state.submit_price(1, 1_000_000, 900, 90);
        state.submit_price(1, 2_000_000, 1000, 100);
        let twap = state.get_twapped(1, 200).unwrap();
        assert_eq!(twap, 1_500_000);
    }

    #[test]
    fn test_oracle_precompile_is_stale() {
        let mut state = OracleState::new(3600);
        state.submit_price(1, 2_000_000, 1000, 100);
        assert!(!state.is_stale(1, 2000));
        assert!(state.is_stale(1, 5000));
    }
}
