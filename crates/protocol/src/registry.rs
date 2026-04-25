//! T1.1 — Asset Registry (per spec §3.1, §3.2)
//!
//! Asset registration, lookup, and lifecycle management.

use call_primitives::{Address, AssetId, Balance};
use crate::{ProtocolError, ProtocolResult};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Asset status in the registry
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum AssetStatus {
    Active,
    Frozen,
    Delisted,
}

/// Asset metadata
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Asset {
    pub id: AssetId,
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub issuer: Address,
    /// Protocol-layer supply (balances held in protocol accounts)
    pub protocol_supply: Balance,
    /// EVM-layer supply (wrapped ERC-20 tokens in circulation)
    pub evm_supply: Balance,
    /// Maximum supply cap (0 = uncapped)
    pub max_supply: Balance,
    pub status: AssetStatus,
    pub compliance_policy: u8,
    pub registered_at: u64,
    /// EVM contract address for the wrapped ERC-20 token (set after bridge deployment)
    pub evm_contract_address: Option<Address>,
}

impl Asset {
    /// Total supply across both protocol and EVM layers
    pub fn all_supply(&self) -> Balance {
        self.protocol_supply.saturating_add(self.evm_supply)
    }

    /// Check if minting `amount` would exceed the max supply cap
    pub fn would_exceed_cap(&self, amount: Balance) -> bool {
        if self.max_supply == 0 {
            false // uncapped
        } else {
            self.all_supply().saturating_add(amount) > self.max_supply
        }
    }
}

/// Asset registry state
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
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
        registered_at: u64,
        max_supply: Balance,
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
            protocol_supply: 0,
            evm_supply: 0,
            max_supply,
            status: AssetStatus::Active,
            compliance_policy,
            registered_at,
            evm_contract_address: None,
        };

        self.assets_by_id.insert(id, asset);
        self.assets_by_symbol.insert(symbol, id);

        Ok(id)
    }

    /// Look up an asset by its ID
    pub fn get_asset(&self, id: AssetId) -> Option<&Asset> {
        self.assets_by_id.get(&id)
    }

    /// Look up an asset by its ID (mutable)
    pub fn get_asset_mut(&mut self, id: AssetId) -> Option<&mut Asset> {
        self.assets_by_id.get_mut(&id)
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

    /// Unfreeze an asset (issuer only)
    pub fn unfreeze_asset(&mut self, id: AssetId, caller: &Address) -> ProtocolResult<()> {
        let asset = self
            .assets_by_id
            .get_mut(&id)
            .ok_or(ProtocolError::AssetError("asset not found".into()))?;
        if &asset.issuer != caller {
            return Err(ProtocolError::Unauthorized);
        }
        asset.status = AssetStatus::Active;
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

    /// Mint protocol supply for an asset (issuer only)
    /// Enforces max_supply cap against all_supply
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
        if asset.would_exceed_cap(amount) {
            return Err(ProtocolError::AssetError(
                "mint would exceed max supply cap".into(),
            ));
        }
        asset.protocol_supply = asset
            .protocol_supply
            .checked_add(amount)
            .ok_or(ProtocolError::BalanceError("overflow".into()))?;
        Ok(())
    }

    /// Burn protocol supply for an asset (issuer only)
    pub fn burn_supply(
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
        asset.protocol_supply = asset
            .protocol_supply
            .checked_sub(amount)
            .ok_or(ProtocolError::BalanceError("insufficient supply".into()))?;
        Ok(())
    }

    /// Increase EVM supply (called by bridge deposit / EVM issuer mint)
    pub fn add_evm_supply(&mut self, id: AssetId, amount: Balance) -> ProtocolResult<()> {
        let asset = self
            .assets_by_id
            .get_mut(&id)
            .ok_or(ProtocolError::AssetError("asset not found".into()))?;
        if asset.would_exceed_cap(amount) {
            return Err(ProtocolError::AssetError(
                "evm mint would exceed max supply cap".into(),
            ));
        }
        asset.evm_supply = asset
            .evm_supply
            .checked_add(amount)
            .ok_or(ProtocolError::BalanceError("overflow".into()))?;
        Ok(())
    }

    /// Decrease EVM supply (called by bridge withdrawal burn)
    pub fn sub_evm_supply(&mut self, id: AssetId, amount: Balance) -> ProtocolResult<()> {
        let asset = self
            .assets_by_id
            .get_mut(&id)
            .ok_or(ProtocolError::AssetError("asset not found".into()))?;
        asset.evm_supply = asset
            .evm_supply
            .checked_sub(amount)
            .ok_or(ProtocolError::BalanceError("insufficient evm supply".into()))?;
        Ok(())
    }

    /// Get the next asset ID without allocating
    pub fn next_id(&self) -> AssetId {
        self.next_id
    }

    /// Check if the registry has no assets
    pub fn is_empty(&self) -> bool {
        self.assets_by_id.is_empty()
    }

    /// Transfer ownership of an asset to a new issuer
    pub fn transfer_asset_issuer(
        &mut self,
        id: AssetId,
        caller: Address,
        new_issuer: Address,
    ) -> ProtocolResult<()> {
        let asset = self
            .assets_by_id
            .get_mut(&id)
            .ok_or(ProtocolError::AssetError("asset not found".into()))?;
        if asset.issuer != caller {
            return Err(ProtocolError::Unauthorized);
        }
        if caller == new_issuer {
            return Err(ProtocolError::Unauthorized);
        }
        asset.issuer = new_issuer;
        Ok(())
    }

    /// Update the compliance policy for an asset (issuer only)
    pub fn update_asset_compliance_policy(
        &mut self,
        id: AssetId,
        caller: Address,
        new_policy: u8,
    ) -> ProtocolResult<()> {
        let asset = self
            .assets_by_id
            .get_mut(&id)
            .ok_or(ProtocolError::AssetError("asset not found".into()))?;
        if asset.issuer != caller {
            return Err(ProtocolError::Unauthorized);
        }
        asset.compliance_policy = new_policy;
        Ok(())
    }

    /// Set the EVM contract address for a wrapped ERC-20 token.
    /// This is called after the bridge deploys the wrapped token contract.
    pub fn set_evm_contract_address(&mut self, id: AssetId, addr: Address) {
        if let Some(asset) = self.assets_by_id.get_mut(&id) {
            asset.evm_contract_address = Some(addr);
        }
    }

    /// Get the EVM contract address for a wrapped ERC-20 token, if set.
    pub fn get_evm_contract_address(&self, id: AssetId) -> Option<Address> {
        self.assets_by_id.get(&id).and_then(|a| a.evm_contract_address)
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
            .register_asset("TEST".into(), "Test Token".into(), 18, test_addr(1), 0, 100, 0)
            .expect("register");
        assert_eq!(id, 1);
        let asset = registry.get_asset(id).expect("lookup");
        assert_eq!(asset.symbol, "TEST");
        assert_eq!(asset.issuer, test_addr(1));
        assert_eq!(asset.status, AssetStatus::Active);
        assert_eq!(asset.registered_at, 100);
        assert_eq!(asset.max_supply, 0); // uncapped
        assert_eq!(asset.protocol_supply, 0);
        assert_eq!(asset.evm_supply, 0);
        assert_eq!(asset.all_supply(), 0);
    }

    #[test]
    fn test_register_asset_with_cap() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("CAPPED".into(), "Capped Token".into(), 18, test_addr(1), 0, 100, 1_000_000)
            .expect("register");
        let asset = registry.get_asset(id).unwrap();
        assert_eq!(asset.max_supply, 1_000_000);
        assert!(asset.would_exceed_cap(1_000_001));
        assert!(!asset.would_exceed_cap(1_000_000));
    }

    #[test]
    fn test_register_asset_duplicate_symbol() {
        let mut registry = AssetRegistry::new();
        registry
            .register_asset("TEST".into(), "Test".into(), 18, test_addr(1), 0, 100, 0)
            .unwrap();
        let err = registry
            .register_asset("TEST".into(), "Test 2".into(), 18, test_addr(2), 0, 100, 0)
            .unwrap_err();
        assert!(matches!(err, ProtocolError::RegistryError(_)));
    }

    #[test]
    fn test_asset_freeze_unfreeze_and_delist() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("X".into(), "X Token".into(), 18, test_addr(1), 0, 100, 0)
            .unwrap();

        // Freeze by issuer
        registry.freeze_asset(id, &test_addr(1)).unwrap();
        assert_eq!(
            registry.get_asset(id).unwrap().status,
            AssetStatus::Frozen
        );

        // Unfreeze by issuer
        registry.unfreeze_asset(id, &test_addr(1)).unwrap();
        assert_eq!(
            registry.get_asset(id).unwrap().status,
            AssetStatus::Active
        );

        // Delist by issuer
        registry.delist_asset(id, &test_addr(1)).unwrap();
        assert_eq!(
            registry.get_asset(id).unwrap().status,
            AssetStatus::Delisted
        );

        // Freeze by non-issuer fails
        let id2 = registry
            .register_asset("Y".into(), "Y Token".into(), 18, test_addr(2), 0, 100, 0)
            .unwrap();
        assert!(registry.freeze_asset(id2, &test_addr(3)).is_err());
    }

    #[test]
    fn test_mint_and_burn_supply_with_cap() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("CAP".into(), "Capped".into(), 18, test_addr(1), 0, 100, 1000)
            .unwrap();

        // Mint 500 protocol supply
        registry.mint_supply(id, &test_addr(1), 500).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().protocol_supply, 500);
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 500);

        // Add 300 evm supply
        registry.add_evm_supply(id, 300).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().evm_supply, 300);
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 800);

        // Try to mint beyond cap
        assert!(registry.mint_supply(id, &test_addr(1), 201).is_err());
        // Exact cap should work
        registry.mint_supply(id, &test_addr(1), 200).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 1000);

        // Burn protocol supply
        registry.burn_supply(id, &test_addr(1), 100).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().protocol_supply, 600);

        // Sub evm supply
        registry.sub_evm_supply(id, 100).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().evm_supply, 200);
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 800);
    }

    #[test]
    fn test_get_asset_not_found() {
        let registry = AssetRegistry::new();
        assert!(registry.get_asset(999).is_none());
        assert!(registry.get_asset_by_symbol("NOPE").is_none());
    }

    #[test]
    fn test_cap_boundary_exact_limit() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("CAP".into(), "Capped".into(), 18, test_addr(1), 0, 100, 1000)
            .unwrap();

        // Mint exactly the cap
        registry.mint_supply(id, &test_addr(1), 1000).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 1000);

        // One unit over should fail
        assert!(registry.mint_supply(id, &test_addr(1), 1).is_err());

        // Burn some then mint back to exact cap
        registry.burn_supply(id, &test_addr(1), 100).unwrap();
        registry.mint_supply(id, &test_addr(1), 100).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 1000);
    }

    #[test]
    fn test_evm_mint_cap_enforcement() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("CAP".into(), "Capped".into(), 18, test_addr(1), 0, 100, 1000)
            .unwrap();

        // Protocol mint 600
        registry.mint_supply(id, &test_addr(1), 600).unwrap();

        // EVM mint 400 should succeed
        registry.add_evm_supply(id, 400).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 1000);

        // EVM mint 1 more should fail
        assert!(registry.add_evm_supply(id, 1).is_err());

        // Burn protocol supply then EVM mint should succeed
        registry.burn_supply(id, &test_addr(1), 100).unwrap();
        registry.add_evm_supply(id, 100).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 1000);
    }

    #[test]
    fn test_uncapped_asset_unlimited_mint() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("UNCAPPED".into(), "Uncapped".into(), 18, test_addr(1), 0, 100, 0)
            .unwrap();

        // Large mints should succeed when max_supply == 0
        registry.mint_supply(id, &test_addr(1), u128::MAX / 2).unwrap();
        registry.add_evm_supply(id, u128::MAX / 2).unwrap();
        assert_eq!(
            registry.get_asset(id).unwrap().all_supply(),
            u128::MAX - 1
        );
    }

    #[test]
    fn test_all_supply_overflow_safety() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("CAP".into(), "Capped".into(), 18, test_addr(1), 0, 100, 0)
            .unwrap();

        // Set supplies near u128::MAX
        registry.mint_supply(id, &test_addr(1), u128::MAX).unwrap();
        registry.add_evm_supply(id, 1).unwrap();

        // all_supply should saturate, not panic
        let asset = registry.get_asset(id).unwrap();
        assert_eq!(asset.all_supply(), u128::MAX);

        // With a finite cap below u128::MAX, exceeding it is detected
        let capped_id = registry
            .register_asset("CAP2".into(), "Capped2".into(), 18, test_addr(1), 0, 100, u128::MAX - 1)
            .unwrap();
        registry.mint_supply(capped_id, &test_addr(1), u128::MAX - 1).unwrap();
        assert!(registry.get_asset(capped_id).unwrap().would_exceed_cap(1));
    }

    #[test]
    fn test_non_issuer_cannot_freeze_delist() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("X".into(), "X Token".into(), 18, test_addr(1), 0, 100, 0)
            .unwrap();

        assert!(registry.freeze_asset(id, &test_addr(2)).is_err());
        assert!(registry.unfreeze_asset(id, &test_addr(2)).is_err());
        assert!(registry.delist_asset(id, &test_addr(2)).is_err());

        // Issuer can do all of these
        registry.freeze_asset(id, &test_addr(1)).unwrap();
        registry.unfreeze_asset(id, &test_addr(1)).unwrap();
        registry.delist_asset(id, &test_addr(1)).unwrap();
    }

    #[test]
    fn test_bridge_does_not_affect_cap() {
        let mut registry = AssetRegistry::new();
        let id = registry
            .register_asset("BRIDGE".into(), "Bridge".into(), 18, test_addr(1), 0, 100, 1000)
            .unwrap();

        // Protocol mint 500
        registry.mint_supply(id, &test_addr(1), 500).unwrap();

        // Bridge deposit (add_evm_supply) increases evm_supply
        registry.add_evm_supply(id, 200).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 700);

        // Bridge withdrawal (sub_evm_supply) decreases evm_supply
        registry.sub_evm_supply(id, 100).unwrap();
        assert_eq!(registry.get_asset(id).unwrap().all_supply(), 600);

        // Cap is enforced against all_supply, so remaining headroom is 400
        assert!(!registry.get_asset(id).unwrap().would_exceed_cap(400));
        assert!(registry.get_asset(id).unwrap().would_exceed_cap(401));
    }
}
