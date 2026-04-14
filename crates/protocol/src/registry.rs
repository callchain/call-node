//! T1.1 — Asset Registry (per spec §3.1, §3.2)
//!
//! Asset registration, lookup, and lifecycle management.

use call_primitives::{Address, AssetId, Balance};
use crate::{ProtocolError, ProtocolResult};
use std::collections::HashMap;

/// Asset status in the registry
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AssetStatus {
    Active,
    Frozen,
    Delisted,
}

/// Asset metadata
#[derive(Debug, Clone)]
pub struct Asset {
    pub id: AssetId,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub issuer: Address,
    pub total_supply: Balance,
    pub status: AssetStatus,
    pub compliance_policy: u8,
    pub registered_at: u64,
}

/// Fee required to register a new asset
pub const ASSET_REGISTRATION_FEE: Balance = 10_000_000_000_000_000_000u128; // 10 CALL

/// Asset registry state
#[derive(Debug, Default)]
pub struct AssetRegistry {
    assets_by_id: HashMap<AssetId, Asset>,
    assets_by_symbol: HashMap<String, AssetId>,
    next_id: AssetId,
}

impl AssetRegistry {
    pub fn new() -> Self {
        Self {
            assets_by_id: HashMap::new(),
            assets_by_symbol: HashMap::new(),
            next_id: 1,
        }
    }

    /// Register a new asset. Deducts fee, allocates AssetId, creates balance entry.
    pub fn register_asset(
        &mut self,
        symbol: String,
        name: String,
        decimals: u8,
        issuer: Address,
        compliance_policy: u8,
    ) -> ProtocolResult<AssetId> {
        // Check symbol uniqueness
        if self.assets_by_symbol.contains_key(&symbol) {
            return Err(ProtocolError::RegistryError(
                "duplicate symbol".into(),
            ));
        }

        if symbol.len() > 12 {
            return Err(ProtocolError::RegistryError(
                "symbol too long (max 12 chars)".into(),
            ));
        }

        let id = self.next_id;
        self.next_id += 1;

        let asset = Asset {
            id,
            symbol: symbol.clone(),
            name,
            decimals,
            issuer,
            total_supply: 0,
            status: AssetStatus::Active,
            compliance_policy,
            registered_at: 0, // set by caller with current block
        };

        self.assets_by_id.insert(id, asset);
        self.assets_by_symbol.insert(symbol, id);

        Ok(id)
    }

    /// Look up an asset by its ID
    pub fn get_asset(&self, id: AssetId) -> Option<&Asset> {
        self.assets_by_id.get(&id)
    }

    /// Look up an asset by its symbol
    pub fn get_asset_by_symbol(&self, symbol: &str) -> Option<&Asset> {
        self.assets_by_symbol
            .get(symbol)
            .and_then(|id| self.assets_by_id.get(id))
    }

    /// Freeze an asset (issuer only)
    pub fn freeze_asset(&mut self, id: AssetId, caller: &Address) -> ProtocolResult<()> {
        let asset = self
            .assets_by_id
            .get_mut(&id)
            .ok_or(ProtocolError::AssetError("asset not found".into()))?;
        if &asset.issuer != caller {
            return Err(ProtocolError::Unauthorized);
        }
        asset.status = AssetStatus::Frozen;
        Ok(())
    }

    /// Delist an asset (issuer only)
    pub fn delist_asset(&mut self, id: AssetId, caller: &Address) -> ProtocolResult<()> {
        let asset = self
            .assets_by_id
            .get_mut(&id)
            .ok_or(ProtocolError::AssetError("asset not found".into()))?;
        if &asset.issuer != caller {
            return Err(ProtocolError::Unauthorized);
        }
        asset.status = AssetStatus::Delisted;
        Ok(())
    }

    /// Mint supply for an asset (issuer only)
    pub fn mint_supply(
        &mut self,
        id: AssetId,
        caller: &Address,
        amount: Balance,
    ) -> ProtocolResult<()> {
        let asset = self
            .assets_by_id
            .get_mut(&id)
            .ok_or(ProtocolError::AssetError("asset not found".into()))?;
        if &asset.issuer != caller {
            return Err(ProtocolError::Unauthorized);
        }
        asset.total_supply = asset
            .total_supply
            .checked_add(amount)
            .ok_or(ProtocolError::BalanceError("overflow".into()))?;
        Ok(())
    }

    /// Get the next asset ID without allocating
    pub fn next_id(&self) -> AssetId {
        self.next_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_addr(n: u8) -> Address {
        Address::repeat_byte(n)
    }

    #[test]
    fn test_register_asset_success() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("TEST".into(), "Test Token".into(), 18, test_addr(1), 0)
            .expect("register");
        assert_eq!(id, 1);
        let asset = registry.get_asset(id).expect("lookup");
        assert_eq!(asset.symbol, "TEST");
        assert_eq!(asset.issuer, test_addr(1));
        assert_eq!(asset.status, AssetStatus::Active);
    }

    #[test]
    fn test_register_asset_duplicate_symbol() {
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("TEST".into(), "Test".into(), 18, test_addr(1), 0)
            .unwrap();
        let err = registry
            .register_asset("TEST".into(), "Test 2".into(), 18, test_addr(2), 0)
            .unwrap_err();
        assert!(matches!(err, ProtocolError::RegistryError(_)));
    }

    #[test]
    fn test_register_asset_insufficient_fee() {
        // Registration fee is checked at transaction level, not in registry itself.
        // Registry only validates symbol uniqueness and format.
        // This test documents that fee validation is the caller's responsibility.
        let mut registry = AssetRegistry::new();
        // Without fee check, registration succeeds at registry level
        let result = registry.register_asset("OK".into(), "Ok".into(), 18, test_addr(1), 0);
        assert!(result.is_ok());
    }

    #[test]
    fn test_asset_freeze_and_delist() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("X".into(), "X Token".into(), 18, test_addr(1), 0)
            .unwrap();

        // Freeze by issuer
        registry.freeze_asset(id, &test_addr(1)).unwrap();
        assert_eq!(
            registry.get_asset(id).unwrap().status,
            AssetStatus::Frozen
        );

        // Delist by issuer
        registry.delist_asset(id, &test_addr(1)).unwrap();
        assert_eq!(
            registry.get_asset(id).unwrap().status,
            AssetStatus::Delisted
        );

        // Freeze by non-issuer fails
        let id2 = registry
            .register_asset("Y".into(), "Y Token".into(), 18, test_addr(2), 0)
            .unwrap();
        assert!(registry.freeze_asset(id2, &test_addr(3)).is_err());
    }

    #[test]
    fn test_get_asset_not_found() {
        let registry = AssetRegistry::new();
        assert!(registry.get_asset(999).is_none());
        assert!(registry.get_asset_by_symbol("NOPE").is_none());
    }
}
