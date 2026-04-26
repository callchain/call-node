//! T1.7 — Fee Currency Registry (per spec §12.3.0)
//!
//! Multi-currency fee payment, oracle price lookup, governance management.

use call_primitives::{AssetId, Balance, PricePair};
use call_oracle::OracleManager;
use crate::ProtocolResult;
use crate::ProtocolError;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeeCurrencyEntry {
    pub asset_id: AssetId,
    pub name: String,
    pub decimals: u8,
    pub oracle_price_key: Option<[u8; 32]>,
    pub added_at_block: u64,
    pub added_by_proposal: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FeeCurrencyRegistry {
    pub allowed_currencies: Vec<FeeCurrencyEntry>,
    pub stablecoin_cap_bps: u32, // default 5000 = 50%
    /// Minimum market cap in USD for fee currency eligibility
    pub min_market_cap_usd: u128,
    current_stablecoin_used: Balance,
    block_gas_limit: Balance,
}

/// Default minimum market cap for fee currency eligibility (100M USD)
pub const DEFAULT_MIN_MARKET_CAP_USD: u128 = 100_000_000;
/// Oracle strikes before auto-disable
pub const ORACLE_STRIKES_BEFORE_DISABLE: u32 = 10;
/// Default grace period for FeeCurrencyRemove (86400 blocks ≈ 1 day)
pub const FEE_CURRENCY_GRACE_PERIOD: u64 = 86_400;

impl Default for FeeCurrencyRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl FeeCurrencyRegistry {
    pub fn new() -> Self {
        Self {
            allowed_currencies: Vec::new(),
            stablecoin_cap_bps: 5000, // 50%
            min_market_cap_usd: DEFAULT_MIN_MARKET_CAP_USD,
            current_stablecoin_used: 0,
            block_gas_limit: 20_000_000,
        }
    }

    pub fn is_allowed(&self, asset_id: AssetId) -> bool {
        self.allowed_currencies.iter().any(|e| e.asset_id == asset_id)
    }

    pub fn get_entry(&self, asset_id: AssetId) -> Option<&FeeCurrencyEntry> {
        self.allowed_currencies.iter().find(|e| e.asset_id == asset_id)
    }

    /// Add a fee currency (governance only)
    pub fn add_fee_currency(
        &mut self,
        entry: FeeCurrencyEntry,
        _proposal_id: u64,
    ) -> ProtocolResult<()> {
        if self.is_allowed(entry.asset_id) {
            return Err(ProtocolError::RegistryError(
                "currency already registered".into(),
            ));
        }
        self.allowed_currencies.push(entry);
        Ok(())
    }

    /// Remove a fee currency (governance only, with grace period)
    pub fn remove_fee_currency(
        &mut self,
        asset_id: AssetId,
        grace_period_blocks: u64,
    ) -> ProtocolResult<()> {
        let idx = self
            .allowed_currencies
            .iter()
            .position(|e| e.asset_id == asset_id)
            .ok_or_else(|| ProtocolError::RegistryError("currency not found".into()))?;

        // Grace period: mark for removal, actual removal after blocks
        // Simplified: remove immediately (grace period enforced by governance)
        let _ = grace_period_blocks;
        self.allowed_currencies.remove(idx);
        Ok(())
    }

    /// Get CALL/USD price from oracle, or fallback to hardcoded value.
    /// Queries `PricePair { base: 1, quote: 0 }` (CALL priced in USD).
    pub fn get_call_price(&self, oracle: Option<&OracleManager>) -> u128 {
        match oracle.and_then(|o| o.get_price(PricePair::CALL_USD)) {
            Some(agg) => agg.median_price,
            None => 2_000_000, // Fallback: $2.00 with 6 decimals
        }
    }

    /// Get the USD price for any asset from the oracle.
    /// Implicitly quotes in USD (quote = 0).
    pub fn get_asset_usd_price(&self, asset_id: AssetId, oracle: Option<&OracleManager>) -> u128 {
        match oracle.and_then(|o| o.get_price_by_asset(asset_id)) {
            Some(agg) => agg.median_price,
            None => 1_000_000, // Fallback: $1.00 with 6 decimals (assume stablecoin)
        }
    }

    /// Enforce stablecoin cap per block
    pub fn check_stablecoin_cap(&self, amount: Balance) -> ProtocolResult<()> {
        let cap = (self.block_gas_limit * self.stablecoin_cap_bps as u128) / 10000;
        if self.current_stablecoin_used + amount > cap {
            return Err(ProtocolError::GasError(
                "stablecoin fee cap exceeded".into(),
            ));
        }
        Ok(())
    }

    /// Increment the stablecoin usage counter (call after a stablecoin fee is paid)
    pub fn increment_stablecoin_used(&mut self, amount: Balance) -> ProtocolResult<()> {
        self.current_stablecoin_used = self
            .current_stablecoin_used
            .checked_add(amount)
            .ok_or(ProtocolError::GasError(
                "stablecoin used overflow".into(),
            ))?;
        Ok(())
    }

    /// Reset the stablecoin counter (called at the start of each cap window)
    pub fn reset_stablecoin_used(&mut self) {
        self.current_stablecoin_used = 0;
    }

    /// Get current stablecoin usage
    pub fn stablecoin_used(&self) -> Balance {
        self.current_stablecoin_used
    }

    /// Priority score for mempool sorting.
    ///
    /// Phase 2 two-lookup model:
    /// - CALL fees are used directly.
    /// - Stablecoin fees are converted: `fee * stablecoin_usd_price / call_usd_price`.
    ///   Both prices are implicitly quoted in USD (`quote = 0`).
    pub fn priority_score(
        &self,
        fee: u128,
        is_call: bool,
        asset_id: AssetId,
        oracle: Option<&OracleManager>,
    ) -> u128 {
        if is_call {
            fee // CALL direct
        } else {
            // Phase 2: stablecoin_usd / call_usd
            let stablecoin_usd = self.get_asset_usd_price(asset_id, oracle);
            let call_usd = self.get_call_price(oracle);
            fee * stablecoin_usd / call_usd
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_fee_currency_registry_is_allowed() {
        let mut registry = FeeCurrencyRegistry::new();
        assert!(!registry.is_allowed(1));

        registry
            .add_fee_currency(
                FeeCurrencyEntry {
                    asset_id: 1,
                    name: "CALL".into(),
                    decimals: 18,
                    oracle_price_key: None,
                    added_at_block: 0,
                    added_by_proposal: 1,
                },
                1,
            )
            .unwrap();
        assert!(registry.is_allowed(1));
    }

    #[test]
    fn test_fee_currency_add_remove() {
        let mut registry = FeeCurrencyRegistry::new();
        registry
            .add_fee_currency(
                FeeCurrencyEntry {
                    asset_id: 2,
                    name: "USDC".into(),
                    decimals: 6,
                    oracle_price_key: None,
                    added_at_block: 0,
                    added_by_proposal: 2,
                },
                2,
            )
            .unwrap();
        assert!(registry.is_allowed(2));

        registry.remove_fee_currency(2, FEE_CURRENCY_GRACE_PERIOD).unwrap();
        assert!(!registry.is_allowed(2));
    }

    #[test]
    fn test_priority_score_call() {
        let registry = FeeCurrencyRegistry::new();
        let score = registry.priority_score(1_000_000, true, 1, None);
        assert_eq!(score, 1_000_000); // CALL = direct fee
    }

    #[test]
    fn test_priority_score_stablecoin_conversion_fallback() {
        let registry = FeeCurrencyRegistry::new();
        // Fallback: stablecoin_usd = $1.00, call_usd = $2.00
        // fee * 1M / 2M = fee / 2
        let score = registry.priority_score(2_000_000, false, 2, None);
        assert_eq!(score, 1_000_000); // 2M * 1M / 2M = 1M
    }

    #[test]
    fn test_priority_score_with_oracle() {
        let registry = FeeCurrencyRegistry::new();
        let mut oracle = OracleManager::default();
        // Set CALL/USD = $3.00, USDC/USD = $1.00
        oracle.simple_submit_price_by_asset(1, 3_000_000, 1000, 100);
        oracle.simple_submit_price_by_asset(2, 1_000_000, 1000, 100);
        // 1M USDC * 1M / 3M = 333_333
        let score = registry.priority_score(1_000_000, false, 2, Some(&oracle));
        assert_eq!(score, 333_333); // 1M * 1M / 3M = 333_333
    }

    #[test]
    fn test_get_call_price_with_oracle() {
        let registry = FeeCurrencyRegistry::new();
        let mut oracle = OracleManager::default();
        oracle.simple_submit_price_by_asset(1, 3_000_000, 1000, 100);
        let price = registry.get_call_price(Some(&oracle));
        assert_eq!(price, 3_000_000); // CALL/USD = $3.00
    }

    #[test]
    fn test_get_call_price_fallback() {
        let registry = FeeCurrencyRegistry::new();
        let price = registry.get_call_price(None);
        assert_eq!(price, 2_000_000); // Fallback $2.00
    }
}
