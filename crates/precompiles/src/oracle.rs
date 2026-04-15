//! Oracle precompile at 0x101 (per spec §25.4)
//!
//! Functions: getPrice(), getTWAP(), isStale(), getOracleStatus()

use call_primitives::AssetId;
use call_protocol::OracleManager;
use alloy_primitives::address;

/// Precompile address
pub const ORACLE_ADDRESS: alloy_primitives::Address =
    address!("0000000000000000000000000000000000000101");

/// Price entry for an asset (legacy, used by precompile)
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

/// Oracle state (wraps OracleManager for precompile access)
#[derive(Debug)]
pub struct OracleState {
    manager: OracleManager,
    stale_threshold_secs: u64,
}

impl Default for OracleState {
    fn default() -> Self {
        Self::new(3600)
    }
}

impl OracleState {
    pub fn new(stale_threshold_secs: u64) -> Self {
        Self {
            manager: OracleManager::default(),
            stale_threshold_secs,
        }
    }

    /// Get current aggregated price from the oracle manager
    pub fn get_price(&self, asset_id: AssetId) -> Option<OraclePrice> {
        self.manager.get_price(asset_id).map(|agg| OraclePrice {
            price: agg.median_price,
            timestamp: agg.timestamp,
            block_number: agg.block_number,
        })
    }

    /// Get TWAP from the oracle manager
    pub fn get_twapped(&self, asset_id: AssetId, current_timestamp: u64) -> Option<u128> {
        self.manager.get_twap(asset_id, current_timestamp)
    }

    /// Check if price is stale
    pub fn is_stale(&self, asset_id: AssetId, current_timestamp: u64) -> bool {
        self.manager.is_stale(asset_id, current_timestamp)
    }

    /// Get oracle status for an asset
    pub fn get_oracle_status(&self, asset_id: AssetId, current_timestamp: u64) -> OracleStatus {
        match self.manager.get_price(asset_id) {
            None => OracleStatus::Disabled,
            Some(_) => {
                if self.is_stale(asset_id, current_timestamp) {
                    OracleStatus::Stale
                } else {
                    OracleStatus::Active
                }
            }
        }
    }

    /// Submit a price (delegates to OracleManager)
    pub fn submit_price(
        &mut self,
        asset_id: AssetId,
        price: u128,
        timestamp: u64,
        block_number: u64,
    ) {
        // Legacy simple submission — full oracle uses OracleSubmission with signatures
        self.manager
            .simple_submit_price(asset_id, price, timestamp, block_number);
    }

    /// Access the underlying manager for advanced operations
    pub fn manager(&self) -> &OracleManager {
        &self.manager
    }

    /// Access the underlying manager mutably
    pub fn manager_mut(&mut self) -> &mut OracleManager {
        &mut self.manager
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
        let twap = state.get_twapped(1, 1000).unwrap();
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
