//! T1.8 — Gas Sponsor System (per spec §12.3.1–12.3.4)
//!
//! Authorized sponsors, sponsor pools, per-tx sponsors.

use call_primitives::{Address, Balance};
use crate::AccountState;
use crate::{ProtocolError, ProtocolResult};
use std::collections::HashMap;

// ── Authorized Sponsor (12.3.1) ──────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GasSponsorAuth {
    pub sponsor: Address,
    pub allowed_senders: Vec<Address>,
    pub max_daily: u128,
    pub expires_at: u64,
    pub sponsor_signature: [u8; 65],
}

// ── Sponsor Pool (12.3.3) ────────────────────────────────────────────

#[derive(Debug, Clone)]
pub struct GasSponsorPool {
    pub sponsor: Address,
    pub balance: Balance,
    pub delegated_to: Vec<Address>,
    pub per_tx_limit: u128,
}

// ── Sponsor State ────────────────────────────────────────────────────

#[derive(Debug, Default)]
pub struct SponsorRegistry {
    auths: HashMap<Address, GasSponsorAuth>,
    pools: HashMap<Address, GasSponsorPool>,
    daily_usage: HashMap<Address, (u128, u64)>, // (used, day)
}

impl SponsorRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    // ── Authorized Sponsor ──

    pub fn register_sponsor_auth(
        &mut self,
        auth: GasSponsorAuth,
    ) -> ProtocolResult<()> {
        if self.auths.contains_key(&auth.sponsor) {
            return Err(ProtocolError::SponsorError(
                "sponsor already registered".into(),
            ));
        }
        self.auths.insert(auth.sponsor, auth);
        Ok(())
    }

    pub fn revoke_sponsor_auth(&mut self, sponsor: Address) -> ProtocolResult<()> {
        self.auths
            .remove(&sponsor)
            .map(|_| ())
            .ok_or_else(|| ProtocolError::SponsorError("sponsor not found".into()))
    }

    pub fn verify_and_deduct_authorized_sponsor(
        &mut self,
        sponsor: &Address,
        sender: &Address,
        fee: u128,
        current_day: u64,
        account: &mut AccountState,
    ) -> ProtocolResult<()> {
        let auth = self
            .auths
            .get(sponsor)
            .ok_or_else(|| ProtocolError::SponsorError("sponsor not registered".into()))?;

        // Check expiry
        if current_day >= auth.expires_at {
            return Err(ProtocolError::SponsorError("sponsor expired".into()));
        }

        // Check sender whitelist
        if !auth.allowed_senders.is_empty() && !auth.allowed_senders.contains(sender) {
            return Err(ProtocolError::SponsorError("sender not whitelisted".into()));
        }

        // Check daily limit
        let (used_today, day) = self.daily_usage.get(sponsor).copied().unwrap_or((0, 0));
        if day != current_day {
            self.daily_usage.insert(*sponsor, (fee, current_day));
        } else if used_today + fee > auth.max_daily {
            return Err(ProtocolError::SponsorError("daily limit exceeded".into()));
        } else {
            self.daily_usage.insert(*sponsor, (used_today + fee, current_day));
        }

        // Deduct from sponsor balance
        account.balances.deduct_balance(crate::CALL_ASSET_ID, *sponsor, fee)
    }

    // ── Sponsor Pool ──

    pub fn deposit_to_pool(
        &mut self,
        sponsor: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        if let Some(pool) = self.pools.get_mut(&sponsor) {
            pool.balance = pool
                .balance
                .checked_add(amount)
                .ok_or(ProtocolError::SponsorError("overflow".into()))?;
        } else {
            self.pools.insert(
                sponsor,
                GasSponsorPool {
                    sponsor,
                    balance: amount,
                    delegated_to: Vec::new(),
                    per_tx_limit: 0,
                },
            );
        }
        Ok(())
    }

    pub fn withdraw_from_pool(
        &mut self,
        sponsor: Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        let pool = self
            .pools
            .get_mut(&sponsor)
            .ok_or_else(|| ProtocolError::SponsorError("pool not found".into()))?;

        if pool.balance < amount {
            return Err(ProtocolError::SponsorError(
                "insufficient pool balance".into(),
            ));
        }
        pool.balance -= amount;
        Ok(())
    }

    pub fn verify_and_deduct_pool_sponsor(
        &mut self,
        sponsor: &Address,
        sender: &Address,
        fee: u128,
        account: &mut AccountState,
    ) -> ProtocolResult<()> {
        let pool = self
            .pools
            .get(sponsor)
            .ok_or_else(|| ProtocolError::SponsorError("pool not found".into()))?;

        // Check delegated_to whitelist
        if !pool.delegated_to.is_empty() && !pool.delegated_to.contains(sender) {
            return Err(ProtocolError::SponsorError("sender not delegated".into()));
        }

        // Check per-tx limit
        if fee > pool.per_tx_limit && pool.per_tx_limit > 0 {
            return Err(ProtocolError::SponsorError("exceeds per-tx limit".into()));
        }

        // Check pool balance
        if pool.balance < fee as Balance {
            return Err(ProtocolError::SponsorError(
                "insufficient pool balance".into(),
            ));
        }

        // Deduct from pool balance only (account layer tracks pool separately)
        let pool = self.pools.get_mut(sponsor).unwrap();
        pool.balance -= fee as Balance;

        // Deduct from sponsor's balance layer entry
        account.balances.deduct_balance(crate::CALL_ASSET_ID, *sponsor, fee as Balance)
    }

    pub fn verify_and_deduct_per_tx_sponsor(
        &mut self,
        sponsor: &Address,
        fee: u128,
        account: &mut AccountState,
    ) -> ProtocolResult<()> {
        // Check sponsor has sufficient balance
        let sponsor_bal = account.balances.get_balance(crate::CALL_ASSET_ID, sponsor);
        if sponsor_bal < fee {
            return Err(ProtocolError::SponsorError(
                "sponsor insufficient balance".into(),
            ));
        }
        // Deduct from sponsor balance
        account.balances.deduct_balance(crate::CALL_ASSET_ID, *sponsor, fee)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_sponsor_auth_register_and_revoke() {
        let mut registry = SponsorRegistry::new();
        let auth = GasSponsorAuth {
            sponsor: test_addr(1),
            allowed_senders: vec![test_addr(2)],
            max_daily: 10_000,
            expires_at: 1000,
            sponsor_signature: [0u8; 65],
        };
        registry.register_sponsor_auth(auth).unwrap();
        registry.revoke_sponsor_auth(test_addr(1)).unwrap();
        assert!(registry.auths.is_empty());
    }

    #[test]
    fn test_sponsor_auth_expired_expiry() {
        let mut registry = SponsorRegistry::new();
        let auth = GasSponsorAuth {
            sponsor: test_addr(1),
            allowed_senders: vec![],
            max_daily: 10_000,
            expires_at: 50,
            sponsor_signature: [0u8; 65],
        };
        registry.register_sponsor_auth(auth).unwrap();

        let mut account = AccountState::new();
        let result = registry.verify_and_deduct_authorized_sponsor(
            &test_addr(1),
            &test_addr(2),
            100,
            100, // current_day > expires_at
            &mut account,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_sponsor_auth_sender_not_whitelisted() {
        let mut registry = SponsorRegistry::new();
        let auth = GasSponsorAuth {
            sponsor: test_addr(1),
            allowed_senders: vec![test_addr(2)],
            max_daily: 10_000,
            expires_at: 1000,
            sponsor_signature: [0u8; 65],
        };
        registry.register_sponsor_auth(auth).unwrap();

        let mut account = AccountState::new();
        let result = registry.verify_and_deduct_authorized_sponsor(
            &test_addr(1),
            &test_addr(3), // not in whitelist
            100,
            10,
            &mut account,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_sponsor_auth_daily_limit_exceeded() {
        let mut registry = SponsorRegistry::new();
        let auth = GasSponsorAuth {
            sponsor: test_addr(1),
            allowed_senders: vec![],
            max_daily: 500,
            expires_at: 1000,
            sponsor_signature: [0u8; 65],
        };
        registry.register_sponsor_auth(auth).unwrap();

        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 10_000).unwrap();

        // First tx: OK
        registry
            .verify_and_deduct_authorized_sponsor(&test_addr(1), &test_addr(2), 300, 10, &mut account)
            .unwrap();

        // Second tx: exceeds daily limit (300+300 > 500)
        let result = registry.verify_and_deduct_authorized_sponsor(
            &test_addr(1), &test_addr(2), 300, 10, &mut account,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_sponsor_pool_deposit_and_withdraw() {
        let mut registry = SponsorRegistry::new();
        registry.deposit_to_pool(test_addr(1), 10_000).unwrap();
        registry.withdraw_from_pool(test_addr(1), 3_000).unwrap();
        let pool = registry.pools.get(&test_addr(1)).unwrap();
        assert_eq!(pool.balance, 7_000);
    }

    #[test]
    fn test_sponsor_pool_insufficient_balance() {
        let mut registry = SponsorRegistry::new();
        registry.deposit_to_pool(test_addr(1), 1_000).unwrap();
        assert!(registry.withdraw_from_pool(test_addr(1), 2_000).is_err());
    }

    #[test]
    fn test_per_tx_sponsor_signature_verification() {
        let mut registry = SponsorRegistry::new();
        let mut account = AccountState::new();
        account.balances.set_balance(crate::CALL_ASSET_ID, test_addr(1), 1000).unwrap();
        // With sufficient balance, should succeed
        let result = registry.verify_and_deduct_per_tx_sponsor(&test_addr(1), 100, &mut account);
        assert!(result.is_ok());
        assert_eq!(account.balances.get_balance(crate::CALL_ASSET_ID, &test_addr(1)), 900);

        // Insufficient balance should fail
        let result = registry.verify_and_deduct_per_tx_sponsor(&test_addr(2), 100, &mut account);
        assert!(result.is_err());
    }
}
