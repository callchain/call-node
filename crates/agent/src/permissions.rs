//! Agent permissions and fee configuration (per spec §6.3, §6.5)

use call_primitives::{Address, AssetId};
use crate::{AgentError, AgentFeeConfig};

/// Agent permissions (per spec §6.3)
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentPermissions {
    /// Allowed asset IDs for transactions
    pub allowed_assets: Vec<AssetId>,
    /// Daily transaction limit (total amount across all assets)
    pub daily_limit: u128,
    /// Per-transaction limit
    pub per_tx_limit: u128,
    /// Allowed counterparty addresses (empty = all allowed)
    pub allowed_counterparties: Vec<Address>,
    /// Allowed EVM contract addresses (empty = none, for AgentCall)
    pub allowed_protocols: Vec<Address>,
    /// Permissions expiration (block number, 0 = never)
    pub expires_at: u64,
}

impl Default for AgentPermissions {
    fn default() -> Self {
        Self {
            allowed_assets: vec![1], // Only CALL allowed by default (was: empty = all allowed)
            daily_limit: 10_000,     // Was: u128::MAX (unlimited)
            per_tx_limit: 1_000,     // Was: u128::MAX (unlimited)
            allowed_counterparties: vec![], // Empty = all allowed (unchanged semantics)
            allowed_protocols: vec![],      // Empty = none (unchanged semantics)
            expires_at: 0,
        }
    }
}

impl AgentPermissions {
    /// Check if an asset is allowed for this agent
    pub fn is_asset_allowed(&self, asset_id: AssetId) -> bool {
        self.allowed_assets.is_empty() || self.allowed_assets.contains(&asset_id)
    }

    /// Check if a counterparty is allowed
    pub fn is_counterparty_allowed(&self, counterparty: &Address) -> bool {
        self.allowed_counterparties.is_empty()
            || self.allowed_counterparties.contains(counterparty)
    }

    /// Check if a protocol contract is allowed
    pub fn is_protocol_allowed(&self, protocol: &Address) -> bool {
        self.allowed_protocols.is_empty() || self.allowed_protocols.contains(protocol)
    }

    /// Check if the transaction has expired
    pub fn is_expired(&self, current_block: u64) -> bool {
        self.expires_at != 0 && current_block > self.expires_at
    }

    /// Check if a transaction amount is within per-tx limit
    pub fn check_per_tx_limit(&self, amount: u128) -> Result<(), AgentError> {
        if amount > self.per_tx_limit {
            return Err(AgentError::PermissionDenied(
                format!("amount {} exceeds per-tx limit {}", amount, self.per_tx_limit),
            ));
        }
        Ok(())
    }

    /// Check if cumulative daily amount is within limit
    pub fn check_daily_limit(&self, current_daily: u128, amount: u128) -> Result<(), AgentError> {
        if current_daily + amount > self.daily_limit {
            return Err(AgentError::PermissionDenied(
                format!(
                    "daily limit exceeded: {} + {} > {}",
                    current_daily, amount, self.daily_limit
                ),
            ));
        }
        Ok(())
    }
}

/// Agent daily usage tracking
#[derive(Debug, Default)]
pub struct AgentDailyUsage {
    /// Total fee spent today
    pub total_fee_today: u128,
    /// Total transaction amount today
    pub total_amount_today: u128,
    /// Unix timestamp (ms) when usage was last reset
    pub last_reset_time: u64,
}

impl AgentDailyUsage {
    /// Check if usage should be reset (new day = 86400000 ms = 24 hours)
    pub fn maybe_reset(&mut self, current_time: u64) {
        const MS_PER_DAY: u64 = 86400000; // 24 hours in milliseconds
        if current_time >= self.last_reset_time + MS_PER_DAY {
            self.total_fee_today = 0;
            self.total_amount_today = 0;
            self.last_reset_time = current_time;
        }
    }
}

/// Check if owner signature is required based on transaction amount
pub fn requires_owner_signature(
    tx_amount: u128,
    fee_config: &AgentFeeConfig,
) -> bool {
    tx_amount > fee_config.require_owner_signature_above
}

/// Verify agent permissions for a transaction
///
/// `current_block` is used for permission expiry (block-number based).
/// `current_time` is used for daily usage reset (timestamp based, ms).
pub fn verify_agent_permissions(
    permissions: &AgentPermissions,
    daily_usage: &mut AgentDailyUsage,
    fee_config: &AgentFeeConfig,
    asset_id: AssetId,
    counterparty: &Address,
    amount: u128,
    fee: u128,
    current_block: u64,
    current_time: u64,
) -> Result<(), AgentError> {
    // 1. Check expiration
    if permissions.is_expired(current_block) {
        return Err(AgentError::PermissionDenied("permissions expired".into()));
    }

    // 2. Check asset is allowed
    if !permissions.is_asset_allowed(asset_id) {
        return Err(AgentError::PermissionDenied(
            format!("asset {} not allowed", asset_id),
        ));
    }

    // 3. Check counterparty is allowed
    if !permissions.is_counterparty_allowed(counterparty) {
        return Err(AgentError::PermissionDenied(
            format!("counterparty {:?} not allowed", counterparty),
        ));
    }

    // 4. Check per-tx limit
    permissions.check_per_tx_limit(amount)?;

    // 5. Reset daily usage if needed (time-based, not block-based)
    daily_usage.maybe_reset(current_time);

    // 6. Check daily limit
    permissions.check_daily_limit(daily_usage.total_amount_today, amount)?;

    // 7. Update daily usage
    daily_usage.total_amount_today += amount;
    daily_usage.total_fee_today += fee;

    // 8. Check owner daily fee limit
    if daily_usage.total_fee_today > fee_config.owner_max_daily_fee {
        return Err(AgentError::FeeLimitExceeded {
            daily: daily_usage.total_fee_today,
            total: 0,
        });
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::test_addr;

    fn default_permissions() -> AgentPermissions {
        AgentPermissions {
            allowed_assets: vec![1, 2],
            daily_limit: 10_000,
            per_tx_limit: 1_000,
            allowed_counterparties: vec![test_addr(2), test_addr(3)],
            allowed_protocols: vec![],
            expires_at: 0,
        }
    }

    #[test]
    fn test_asset_allowed() {
        let perms = default_permissions();
        assert!(perms.is_asset_allowed(1));
        assert!(perms.is_asset_allowed(2));
        assert!(!perms.is_asset_allowed(99));

        // Default permissions only allow asset 1 (CALL)
        let default_perms = AgentPermissions::default();
        assert!(default_perms.is_asset_allowed(1));
        assert!(!default_perms.is_asset_allowed(999));
    }

    #[test]
    fn test_counterparty_allowed() {
        let perms = default_permissions();
        assert!(perms.is_counterparty_allowed(&test_addr(2)));
        assert!(!perms.is_counterparty_allowed(&test_addr(99)));
    }

    #[test]
    fn test_permission_expired() {
        let perms = AgentPermissions {
            expires_at: 1000,
            ..default_permissions()
        };
        assert!(!perms.is_expired(500));
        assert!(!perms.is_expired(1000));
        assert!(perms.is_expired(1001));
    }

    #[test]
    fn test_per_tx_limit() {
        let perms = default_permissions();
        assert!(perms.check_per_tx_limit(500).is_ok());
        assert!(perms.check_per_tx_limit(1000).is_ok());
        assert!(perms.check_per_tx_limit(1001).is_err());
    }

    #[test]
    fn test_daily_limit() {
        let perms = default_permissions();
        assert!(perms.check_daily_limit(5000, 4000).is_ok());
        assert!(perms.check_daily_limit(5000, 5001).is_err());
    }

    #[test]
    fn test_requires_owner_signature() {
        let config = AgentFeeConfig {
            require_owner_signature_above: 1000,
            ..Default::default()
        };
        assert!(!requires_owner_signature(500, &config));
        assert!(!requires_owner_signature(1000, &config));
        assert!(requires_owner_signature(1001, &config));
    }

    #[test]
    fn test_verify_agent_permissions() {
        let mut perms = default_permissions();
        let mut usage = AgentDailyUsage::default();
        let fee_config = AgentFeeConfig::default();

        assert!(verify_agent_permissions(
            &mut perms,
            &mut usage,
            &fee_config,
            1,        // asset_id
            &test_addr(2), // counterparty
            500,      // amount
            10,       // fee
            100,      // current_block
            1000,     // current_time (ms)
        ).is_ok());

        assert_eq!(usage.total_amount_today, 500);
        assert_eq!(usage.total_fee_today, 10);
    }

    #[test]
    fn test_verify_agent_permissions_asset_not_allowed() {
        let mut perms = default_permissions();
        let mut usage = AgentDailyUsage::default();
        let fee_config = AgentFeeConfig::default();

        let result = verify_agent_permissions(
            &mut perms,
            &mut usage,
            &fee_config,
            99,     // asset not allowed
            &test_addr(2),
            500,
            10,
            100,
            1000,
        );
        assert!(matches!(result, Err(AgentError::PermissionDenied(_))));
    }

    #[test]
    fn test_verify_agent_permissions_per_tx_limit_exceeded() {
        let mut perms = default_permissions();
        let mut usage = AgentDailyUsage::default();
        let fee_config = AgentFeeConfig::default();

        let result = verify_agent_permissions(
            &mut perms,
            &mut usage,
            &fee_config,
            1,
            &test_addr(2),
            2000,   // exceeds per_tx_limit of 1000
            10,
            100,
            1000,
        );
        assert!(matches!(result, Err(AgentError::PermissionDenied(_))));
    }

    #[test]
    fn test_verify_agent_permissions_expired() {
        let mut perms = AgentPermissions {
            allowed_assets: vec![1],
            expires_at: 50,
            ..Default::default()
        };
        let mut usage = AgentDailyUsage::default();
        let fee_config = AgentFeeConfig::default();

        let result = verify_agent_permissions(
            &mut perms,
            &mut usage,
            &fee_config,
            1,
            &test_addr(0),
            500,
            10,
            100,    // past expiry
            1000,
        );
        assert!(matches!(result, Err(AgentError::PermissionDenied(_))));
    }

    #[test]
    fn test_counterparty_not_allowed() {
        let mut perms = default_permissions();
        let mut usage = AgentDailyUsage::default();
        let fee_config = AgentFeeConfig::default();

        let result = verify_agent_permissions(
            &mut perms,
            &mut usage,
            &fee_config,
            1,
            &test_addr(99), // not in allowed_counterparties
            500,
            10,
            100,
            1000,
        );
        assert!(matches!(result, Err(AgentError::PermissionDenied(_))));
    }
}
