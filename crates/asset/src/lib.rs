pub mod precompile;

pub use precompile::AssetPrecompile;

use call_precompile::{
    slot_allowance, slot_asset_meta, slot_balance, u128_to_u256, u256_to_u128, ASSET_ADDRESS,
};
use call_primitives::{Address, Balance, U256};
use call_protocol::storage_backend::StorageBackend;

#[derive(Debug)]
pub enum AssetError {
    InsufficientBalance,
    BalanceOverflow,
    SupplyOverflow,
    SupplyUnderflow,
    MaxSupplyExceeded,
    NotIssuer,
    AssetNotFound,
    InsufficientAllowance,
    ComplianceFailed,
    InvalidMetadata(String),
    AssetIdOverflow,
    InvalidDecimals(u8),
}

impl std::fmt::Display for AssetError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AssetError::InsufficientBalance => write!(f, "insufficient balance"),
            AssetError::BalanceOverflow => write!(f, "balance overflow"),
            AssetError::SupplyOverflow => write!(f, "supply overflow"),
            AssetError::SupplyUnderflow => write!(f, "supply underflow"),
            AssetError::MaxSupplyExceeded => write!(f, "max supply exceeded"),
            AssetError::NotIssuer => write!(f, "not asset issuer"),
            AssetError::AssetNotFound => write!(f, "asset not found"),
            AssetError::InsufficientAllowance => write!(f, "insufficient allowance"),
            AssetError::ComplianceFailed => write!(f, "compliance check failed"),
            AssetError::InvalidMetadata(key) => write!(f, "invalid metadata: {key}"),
            AssetError::AssetIdOverflow => write!(f, "asset id overflow"),
            AssetError::InvalidDecimals(d) => write!(f, "invalid decimals: {d} (max 18)"),
        }
    }
}

impl std::error::Error for AssetError {}

/// Asset metadata loaded from storage.
#[derive(Debug, Clone, PartialEq)]
pub struct AssetMeta {
    pub symbol: String,
    pub name: String,
    pub decimals: u8,
    pub issuer: Address,
    pub max_supply: Balance,
    pub supply: Balance,
    pub status: u8,
    pub registered_at: U256,
}

/// Business logic for asset operations, backed by any StorageBackend.
pub struct AssetStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> AssetStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    // ── Balance operations ────────────────────────────────────────────

    pub fn read_balance(&mut self, asset_id: u64, addr: Address) -> Result<Balance, AssetError> {
        let slot = slot_balance(asset_id, addr);
        let raw = self.backend.load(ASSET_ADDRESS, slot);
        if raw > U256::from(u128::MAX) {
            return Err(AssetError::BalanceOverflow);
        }
        Ok(raw.try_into().unwrap_or(0))
    }

    pub fn write_balance(&mut self, asset_id: u64, addr: Address, amount: Balance) {
        let slot = slot_balance(asset_id, addr);
        self.backend
            .store(ASSET_ADDRESS, slot, u128_to_u256(amount));
    }

    pub fn add_balance(
        &mut self,
        asset_id: u64,
        addr: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let current = self.read_balance(asset_id, addr)?;
        let new = current
            .checked_add(amount)
            .ok_or(AssetError::BalanceOverflow)?;
        self.write_balance(asset_id, addr, new);
        Ok(())
    }

    pub fn deduct_balance(
        &mut self,
        asset_id: u64,
        addr: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let current = self.read_balance(asset_id, addr)?;
        let new = current
            .checked_sub(amount)
            .ok_or(AssetError::InsufficientBalance)?;
        self.write_balance(asset_id, addr, new);
        Ok(())
    }

    // ── Allowance operations ──────────────────────────────────────────

    pub fn read_allowance(
        &mut self,
        asset_id: u64,
        owner: Address,
        spender: Address,
    ) -> Result<Balance, AssetError> {
        let slot = slot_allowance(asset_id, owner, spender);
        let raw = self.backend.load(ASSET_ADDRESS, slot);
        if raw > U256::from(u128::MAX) {
            return Err(AssetError::BalanceOverflow);
        }
        Ok(raw.try_into().unwrap_or(0))
    }

    pub fn write_allowance(
        &mut self,
        asset_id: u64,
        owner: Address,
        spender: Address,
        amount: Balance,
    ) {
        let slot = slot_allowance(asset_id, owner, spender);
        self.backend
            .store(ASSET_ADDRESS, slot, u128_to_u256(amount));
    }

    // ── Metadata operations ───────────────────────────────────────────

    pub fn load_meta_u256(&mut self, asset_id: u64, key: &[u8]) -> U256 {
        self.backend
            .load(ASSET_ADDRESS, slot_asset_meta(asset_id, key))
    }

    pub fn load_meta_u128(&mut self, asset_id: u64, key: &[u8]) -> u128 {
        u256_to_u128(self.load_meta_u256(asset_id, key))
    }

    pub fn load_meta_u8(&mut self, asset_id: u64, key: &[u8]) -> u8 {
        self.load_meta_u256(asset_id, key).to_be_bytes::<32>()[31]
    }

    pub fn load_meta_address(&mut self, asset_id: u64, key: &[u8]) -> Address {
        let v = self.load_meta_u256(asset_id, key);
        Address::from_slice(&v.to_be_bytes::<32>()[12..32])
    }

    pub fn load_meta_string(&mut self, asset_id: u64, key: &[u8]) -> Result<String, AssetError> {
        let v = self.load_meta_u256(asset_id, key);
        let bytes = v.to_be_bytes::<32>();
        // Trim trailing nulls
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(32);
        String::from_utf8(bytes[..len].to_vec())
            .map_err(|_| AssetError::InvalidMetadata(
                String::from_utf8_lossy(key).into_owned(),
            ))
    }

    pub fn store_meta_u256(&mut self, asset_id: u64, key: &[u8], value: U256) {
        self.backend
            .store(ASSET_ADDRESS, slot_asset_meta(asset_id, key), value);
    }

    pub fn read_meta(&mut self, asset_id: u64) -> Result<AssetMeta, AssetError> {
        let symbol = self.load_meta_string(asset_id, b"symbol")?;
        let issuer = self.load_meta_address(asset_id, b"issuer");

        // Existence check: unregistered slots default to zeros.
        if issuer == Address::ZERO && symbol.is_empty() {
            return Err(AssetError::AssetNotFound);
        }

        Ok(AssetMeta {
            symbol,
            name: self.load_meta_string(asset_id, b"name")?,
            decimals: self.load_meta_u8(asset_id, b"decimals"),
            issuer,
            max_supply: self.load_meta_u128(asset_id, b"max_supply"),
            supply: self.load_meta_u128(asset_id, b"supply"),
            status: self.load_meta_u8(asset_id, b"status"),
            registered_at: self.load_meta_u256(asset_id, b"registered_at"),
        })
    }

    pub(crate) fn write_meta(
        &mut self,
        asset_id: u64,
        meta: &AssetMeta,
    ) -> Result<(), AssetError> {
        self.store_meta_string(asset_id, b"symbol", &meta.symbol)?;
        self.store_meta_string(asset_id, b"name", &meta.name)?;
        self.store_meta_u256(asset_id, b"decimals", U256::from(meta.decimals));
        self.store_meta_u256(asset_id, b"issuer", address_to_u256(meta.issuer));
        self.store_meta_u256(asset_id, b"max_supply", u128_to_u256(meta.max_supply));
        self.store_meta_u256(asset_id, b"supply", u128_to_u256(meta.supply));
        self.store_meta_u256(asset_id, b"status", U256::from(meta.status));
        self.store_meta_u256(asset_id, b"registered_at", meta.registered_at);
        Ok(())
    }

    fn store_meta_string(
        &mut self,
        asset_id: u64,
        key: &[u8],
        value: &str,
    ) -> Result<(), AssetError> {
        if value.len() > 32 {
            return Err(AssetError::InvalidMetadata(format!(
                "'{}' exceeds 32 bytes ({} bytes)",
                String::from_utf8_lossy(key),
                value.len()
            )));
        }
        let mut bytes = [0u8; 32];
        let src = value.as_bytes();
        let len = src.len().min(32);
        bytes[..len].copy_from_slice(&src[..len]);
        self.backend.store(
            ASSET_ADDRESS,
            slot_asset_meta(asset_id, key),
            U256::from_be_bytes(bytes),
        );
        Ok(())
    }

    // ── Business logic ────────────────────────────────────────────────

    pub fn transfer(
        &mut self,
        asset_id: u64,
        from: Address,
        to: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        // Pre-flight: ensure receiver's balance won't overflow before
        // deducting sender. This prevents partial state when called
        // outside a checkpoint (e.g. direct Rust API usage).
        let to_current = self.read_balance(asset_id, to)?;
        to_current
            .checked_add(amount)
            .ok_or(AssetError::BalanceOverflow)?;

        self.deduct_balance(asset_id, from, amount)?;
        self.add_balance(asset_id, to, amount)?;
        Ok(())
    }

    /// Transfer to multiple recipients sequentially.
    ///
    /// ⚠️ **Not atomic**: each inner transfer commits independently.
    /// Callers must wrap this in a storage checkpoint if atomicity is required.
    pub fn batch_transfer(
        &mut self,
        asset_id: u64,
        from: Address,
        recipients: &[(Address, Balance)],
    ) -> Result<(), AssetError> {
        for (to, amount) in recipients {
            self.transfer(asset_id, from, *to, *amount)?;
        }
        Ok(())
    }

    pub fn approve(
        &mut self,
        asset_id: u64,
        owner: Address,
        spender: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        self.read_meta(asset_id)?; // verify asset exists
        self.write_allowance(asset_id, owner, spender, amount);
        Ok(())
    }

    pub fn transfer_from(
        &mut self,
        asset_id: u64,
        spender: Address,
        from: Address,
        to: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let allowance = self.read_allowance(asset_id, from, spender)?;
        if allowance < amount {
            return Err(AssetError::InsufficientAllowance);
        }
        // Perform transfer first, then deduct allowance.
        // This ensures allowance is only reduced when the state change succeeds.
        self.transfer(asset_id, from, to, amount)?;
        self.write_allowance(asset_id, from, spender, allowance - amount);
        Ok(())
    }

    pub fn mint(
        &mut self,
        asset_id: u64,
        caller: Address,
        to: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let meta = self.read_meta(asset_id)?;
        if meta.issuer != caller {
            return Err(AssetError::NotIssuer);
        }
        let new_supply = meta
            .supply
            .checked_add(amount)
            .ok_or(AssetError::SupplyOverflow)?;
        if meta.max_supply > 0 && new_supply > meta.max_supply {
            return Err(AssetError::MaxSupplyExceeded);
        }
        self.add_balance(asset_id, to, amount)?;
        self.store_meta_u256(asset_id, b"supply", u128_to_u256(new_supply));
        Ok(())
    }

    pub fn burn(
        &mut self,
        asset_id: u64,
        caller: Address,
        from: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let new_allowance = if caller != from {
            let allowance = self.read_allowance(asset_id, from, caller)?;
            if allowance < amount {
                return Err(AssetError::InsufficientAllowance);
            }
            Some(allowance - amount)
        } else {
            None
        };
        // Deduct balance first; only then reduce allowance.
        // This ensures allowance is only reduced when the state change succeeds.
        self.deduct_balance(asset_id, from, amount)?;
        if let Some(na) = new_allowance {
            self.write_allowance(asset_id, from, caller, na);
        }
        let supply = self.load_meta_u128(asset_id, b"supply");
        let new_supply = supply
            .checked_sub(amount)
            .ok_or(AssetError::SupplyUnderflow)?;
        self.store_meta_u256(asset_id, b"supply", u128_to_u256(new_supply));
        Ok(())
    }

    pub fn register(
        &mut self,
        symbol: &str,
        name: &str,
        decimals: u8,
        max_supply: Balance,
        issuer: Address,
        registered_at: U256,
    ) -> Result<u64, AssetError> {
        if decimals > 18 {
            return Err(AssetError::InvalidDecimals(decimals));
        }
        let next_id_slot = U256::from(0);
        let raw = self.backend.load(ASSET_ADDRESS, next_id_slot);
        if raw > U256::from(u64::MAX) {
            return Err(AssetError::AssetIdOverflow);
        }
        let asset_id = raw.to::<u64>();
        let asset_id = if asset_id == 0 { 1 } else { asset_id };
        let next_id = asset_id.checked_add(1).ok_or(AssetError::AssetIdOverflow)?;
        self.backend
            .store(ASSET_ADDRESS, next_id_slot, U256::from(next_id));

        self.store_meta_string(asset_id, b"symbol", symbol)?;
        self.store_meta_string(asset_id, b"name", name)?;
        self.store_meta_u256(asset_id, b"decimals", U256::from(decimals));
        self.store_meta_u256(asset_id, b"issuer", address_to_u256(issuer));
        self.store_meta_u256(asset_id, b"max_supply", u128_to_u256(max_supply));
        self.store_meta_u256(asset_id, b"supply", U256::from(0));
        self.store_meta_u256(asset_id, b"status", U256::from(0));
        self.store_meta_u256(asset_id, b"compliance", U256::from(0));
        self.store_meta_u256(asset_id, b"registered_at", registered_at);
        self.store_meta_u256(asset_id, b"has_erc20", U256::from(0));

        Ok(asset_id)
    }

    /// Register an ERC-20-backed asset: binds an existing EVM ERC-20 contract
    /// to a protocol asset_id. This asset can be switched between EVM and protocol.
    pub fn register_erc20(
        &mut self,
        evm_contract: Address,
        symbol: &str,
        name: &str,
        decimals: u8,
        max_supply: Balance,
        issuer: Address,
        registered_at: U256,
    ) -> Result<u64, AssetError> {
        if decimals > 18 {
            return Err(AssetError::InvalidDecimals(decimals));
        }
        let next_id_slot = U256::from(0);
        let raw = self.backend.load(ASSET_ADDRESS, next_id_slot);
        if raw > U256::from(u64::MAX) {
            return Err(AssetError::AssetIdOverflow);
        }
        let asset_id = raw.to::<u64>();
        let asset_id = if asset_id == 0 { 1 } else { asset_id };
        let next_id = asset_id.checked_add(1).ok_or(AssetError::AssetIdOverflow)?;
        self.backend
            .store(ASSET_ADDRESS, next_id_slot, U256::from(next_id));

        self.store_meta_string(asset_id, b"symbol", symbol)?;
        self.store_meta_string(asset_id, b"name", name)?;
        self.store_meta_u256(asset_id, b"decimals", U256::from(decimals));
        self.store_meta_u256(asset_id, b"issuer", address_to_u256(issuer));
        self.store_meta_u256(asset_id, b"max_supply", u128_to_u256(max_supply));
        self.store_meta_u256(asset_id, b"supply", U256::from(0));
        self.store_meta_u256(asset_id, b"status", U256::from(0));
        self.store_meta_u256(asset_id, b"compliance", U256::from(0));
        self.store_meta_u256(asset_id, b"registered_at", registered_at);
        self.store_meta_u256(asset_id, b"has_erc20", U256::from(1));
        self.store_meta_u256(asset_id, b"evm_contract", address_to_u256(evm_contract));

        Ok(asset_id)
    }

    /// Check whether an asset has an ERC-20 bridge (has_erc20 == 1).
    /// CALL (asset_id == 1) always returns false; it uses native balance.
    pub fn has_erc20(&mut self, asset_id: u64) -> bool {
        if asset_id == call_protocol::CALL_ASSET_ID {
            return false;
        }
        self.load_meta_u8(asset_id, b"has_erc20") == 1
    }

    pub fn set_has_erc20(&mut self, asset_id: u64, value: u8) {
        self.store_meta_u256(asset_id, b"has_erc20", U256::from(value));
    }

    pub fn set_evm_contract(&mut self, asset_id: u64, addr: Address) {
        self.store_meta_u256(asset_id, b"evm_contract", address_to_u256(addr));
    }

    pub fn dominance(&mut self, asset_id: u64) -> u8 {
        self.load_meta_u8(asset_id, b"dominance")
    }

    pub fn set_dominance(&mut self, asset_id: u64, value: u8) {
        self.store_meta_u256(asset_id, b"dominance", U256::from(value));
    }

    /// Read the next asset_id counter stored at slot 0 of ASSET_ADDRESS.
    pub fn next_asset_id(&mut self) -> u64 {
        let raw = self.backend.load(ASSET_ADDRESS, U256::from(0));
        if raw > U256::from(u64::MAX) {
            return u64::MAX;
        }
        let id = raw.to::<u64>();
        if id == 0 { 1 } else { id }
    }
}

fn address_to_u256(addr: Address) -> U256 {
    let mut bytes = [0u8; 32];
    bytes[12..32].copy_from_slice(addr.as_slice());
    U256::from_be_bytes(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;

    /// In-memory StorageBackend for testing.
    struct TestBackend {
        storage: std::collections::HashMap<(Address, U256), U256>,
    }

    impl TestBackend {
        fn new() -> Self {
            Self {
                storage: std::collections::HashMap::new(),
            }
        }
    }

    impl StorageBackend for TestBackend {
        fn load(&mut self, address: Address, slot: U256) -> U256 {
            self.storage
                .get(&(address, slot))
                .copied()
                .unwrap_or(U256::ZERO)
        }
        fn store(&mut self, address: Address, slot: U256, value: U256) {
            self.storage.insert((address, slot), value);
        }
    }

    #[test]
    fn test_read_write_balance() {
        let mut backend = TestBackend::new();
        let addr = Address::repeat_byte(0xAB);
        {
            let mut store = AssetStorage::new(&mut backend);
            store.write_balance(1, addr, 5000);
        }
        let mut store = AssetStorage::new(&mut backend);
        assert_eq!(store.read_balance(1, addr).unwrap(), 5000);
        assert_eq!(store.read_balance(1, Address::ZERO).unwrap(), 0);
    }

    #[test]
    fn test_transfer() {
        let mut backend = TestBackend::new();
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);
        {
            let mut store = AssetStorage::new(&mut backend);
            store.write_balance(1, from, 1000);
            store.transfer(1, from, to, 500).unwrap();
        }
        let mut store = AssetStorage::new(&mut backend);
        assert_eq!(store.read_balance(1, from).unwrap(), 500);
        assert_eq!(store.read_balance(1, to).unwrap(), 500);
    }

    #[test]
    fn test_transfer_insufficient() {
        let mut backend = TestBackend::new();
        let from = Address::repeat_byte(0xAB);
        let to = Address::repeat_byte(0xCD);
        {
            let mut store = AssetStorage::new(&mut backend);
            store.write_balance(1, from, 100);
            let err = store.transfer(1, from, to, 500).unwrap_err();
            assert!(matches!(err, AssetError::InsufficientBalance));
        }
    }

    #[test]
    fn test_mint_and_burn() {
        let mut backend = TestBackend::new();
        let issuer = Address::repeat_byte(0x11);
        let recipient = Address::repeat_byte(0x22);
        {
            let mut store = AssetStorage::new(&mut backend);
            // Register asset
            let asset_id = store
                .register("GOLD", "Gold Token", 18, 10000, issuer, U256::ZERO)
                .unwrap();
            assert_eq!(asset_id, 1);

            // Mint
            store.mint(asset_id, issuer, recipient, 5000).unwrap();
            assert_eq!(store.read_balance(asset_id, recipient).unwrap(), 5000);
            assert_eq!(store.read_meta(asset_id).unwrap().supply, 5000);

            // Burn (recipient burns their own tokens)
            store.burn(asset_id, recipient, recipient, 2000).unwrap();
            assert_eq!(store.read_balance(asset_id, recipient).unwrap(), 3000);
            assert_eq!(store.read_meta(asset_id).unwrap().supply, 3000);
        }
    }

    #[test]
    fn test_mint_not_issuer() {
        let mut backend = TestBackend::new();
        let issuer = Address::repeat_byte(0x11);
        let attacker = Address::repeat_byte(0x99);
        let recipient = Address::repeat_byte(0x22);
        {
            let mut store = AssetStorage::new(&mut backend);
            let asset_id = store
                .register("GOLD", "Gold Token", 18, 10000, issuer, U256::ZERO)
                .unwrap();
            let err = store.mint(asset_id, attacker, recipient, 100).unwrap_err();
            assert!(matches!(err, AssetError::NotIssuer));
        }
    }

    #[test]
    fn test_approve_and_transfer_from() {
        let mut backend = TestBackend::new();
        let owner = Address::repeat_byte(0x11);
        let spender = Address::repeat_byte(0x22);
        let to = Address::repeat_byte(0x33);
        {
            let mut store = AssetStorage::new(&mut backend);
            store.register("TEST", "Test Token", 18, 0, owner, U256::ZERO).unwrap();
            store.write_balance(1, owner, 1000);
            store.approve(1, owner, spender, 500).unwrap();
            assert_eq!(store.read_allowance(1, owner, spender).unwrap(), 500);

            store.transfer_from(1, spender, owner, to, 300).unwrap();
            assert_eq!(store.read_balance(1, owner).unwrap(), 700);
            assert_eq!(store.read_balance(1, to).unwrap(), 300);
            assert_eq!(store.read_allowance(1, owner, spender).unwrap(), 200);
        }
    }

    #[test]
    fn test_allowance_insufficient() {
        let mut backend = TestBackend::new();
        let owner = Address::repeat_byte(0x11);
        let spender = Address::repeat_byte(0x22);
        let to = Address::repeat_byte(0x33);
        {
            let mut store = AssetStorage::new(&mut backend);
            store.register("TEST", "Test Token", 18, 0, owner, U256::ZERO).unwrap();
            store.write_balance(1, owner, 1000);
            store.approve(1, owner, spender, 100).unwrap();
            let err = store.transfer_from(1, spender, owner, to, 300).unwrap_err();
            assert!(matches!(err, AssetError::InsufficientAllowance));
        }
    }

    // ── Property-based tests (proptest) ───────────────────────────────

    use proptest::prelude::*;

    proptest! {
        #[test]
        fn prop_transfer_preserves_total_balance(
            sender_bal in 0u128..10_000u128,
            recipient_bal in 0u128..10_000u128,
            amount in 0u128..10_000u128,
        ) {
            let mut backend = TestBackend::new();
            let sender = Address::repeat_byte(0xAB);
            let recipient = Address::repeat_byte(0xCD);
            {
                let mut store = AssetStorage::new(&mut backend);
                store.write_balance(1, sender, sender_bal);
                store.write_balance(1, recipient, recipient_bal);

                let old_total = sender_bal + recipient_bal;

                if store.transfer(1, sender, recipient, amount).is_ok() {
                    let new_sender = store.read_balance(1, sender).unwrap();
                    let new_recipient = store.read_balance(1, recipient).unwrap();
                    let new_total = new_sender + new_recipient;
                    prop_assert_eq!(old_total, new_total,
                        "transfer must preserve total balance: old={}, new={}",
                        old_total, new_total);
                }
            }
        }

        #[test]
        fn prop_mint_increases_supply(
            initial_supply in 0u128..5_000u128,
            mint_amount in 0u128..5_000u128,
            max_supply in 5_000u128..10_000u128,
        ) {
            let mut backend = TestBackend::new();
            let issuer = Address::repeat_byte(0x11);
            let recipient = Address::repeat_byte(0x22);
            {
                let mut store = AssetStorage::new(&mut backend);
                let asset_id = store
                    .register("GOLD", "Gold Token", 18, max_supply, issuer, U256::ZERO)
                    .unwrap();

                // Pre-seed supply
                if initial_supply > 0 {
                    store.mint(asset_id, issuer, recipient, initial_supply).unwrap();
                }

                let supply_before = store.read_meta(asset_id).unwrap().supply;

                match store.mint(asset_id, issuer, recipient, mint_amount) {
                    Ok(()) => {
                        let supply_after = store.read_meta(asset_id).unwrap().supply;
                        prop_assert_eq!(supply_after, supply_before + mint_amount,
                            "mint must increase supply by exact amount");
                    }
                    Err(AssetError::MaxSupplyExceeded) => {
                        prop_assert!(supply_before + mint_amount > max_supply,
                            "MaxSupplyExceeded only when exceeding max_supply");
                    }
                    Err(_) => {}
                }
            }
        }

        #[test]
        fn prop_burn_decreases_supply(
            initial_supply in 100u128..5_000u128,
            burn_amount in 0u128..100u128,
        ) {
            let mut backend = TestBackend::new();
            let issuer = Address::repeat_byte(0x11);
            let holder = Address::repeat_byte(0x22);
            {
                let mut store = AssetStorage::new(&mut backend);
                let asset_id = store
                    .register("GOLD", "Gold Token", 18, 10_000, issuer, U256::ZERO)
                    .unwrap();

                store.mint(asset_id, issuer, holder, initial_supply).unwrap();
                let supply_before = store.read_meta(asset_id).unwrap().supply;

                match store.burn(asset_id, holder, holder, burn_amount) {
                    Ok(()) => {
                        let supply_after = store.read_meta(asset_id).unwrap().supply;
                        prop_assert_eq!(supply_after, supply_before - burn_amount,
                            "burn must decrease supply by exact amount");
                    }
                    Err(AssetError::SupplyUnderflow) => {
                        prop_assert!(burn_amount > supply_before,
                            "SupplyUnderflow only when burning more than supply");
                    }
                    Err(AssetError::InsufficientBalance) => {
                        prop_assert!(burn_amount > initial_supply,
                            "InsufficientBalance only when burning more than balance");
                    }
                    Err(_) => {}
                }
            }
        }

        #[test]
        fn prop_balance_never_negative(
            initial in 0u128..10_000u128,
            deduct in 0u128..10_000u128,
        ) {
            let mut backend = TestBackend::new();
            let addr = Address::repeat_byte(0xAB);
            {
                let mut store = AssetStorage::new(&mut backend);
                store.write_balance(1, addr, initial);

                let _ = store.deduct_balance(1, addr, deduct);
                let bal = store.read_balance(1, addr).unwrap();
                prop_assert!(bal <= initial, "balance must not exceed initial after deduct");
            }
        }
    }

    #[test]
    fn test_transfer_from_allowance_rollback_on_failure() {
        let mut backend = TestBackend::new();
        let owner = Address::repeat_byte(0x11);
        let spender = Address::repeat_byte(0x22);
        let to = Address::repeat_byte(0x33);
        {
            let mut store = AssetStorage::new(&mut backend);
            store.register("TEST", "Test Token", 18, 0, owner, U256::ZERO).unwrap();
            store.write_balance(1, owner, 100);
            store.approve(1, owner, spender, 500).unwrap();

            // transfer_from fails because owner balance (100) < amount (200)
            let err = store.transfer_from(1, spender, owner, to, 200).unwrap_err();
            assert!(matches!(err, AssetError::InsufficientBalance));

            // Allowance must NOT have been reduced
            assert_eq!(store.read_allowance(1, owner, spender).unwrap(), 500);
        }
    }

    #[test]
    fn test_burn_allowance_rollback_on_failure() {
        let mut backend = TestBackend::new();
        let owner = Address::repeat_byte(0x11);
        let burner = Address::repeat_byte(0x22);
        {
            let mut store = AssetStorage::new(&mut backend);
            store.register("TEST", "Test Token", 18, 0, owner, U256::ZERO).unwrap();
            store.write_balance(1, owner, 100);
            store.approve(1, owner, burner, 500).unwrap();

            // burn fails because owner balance (100) < amount (200)
            let err = store.burn(1, burner, owner, 200).unwrap_err();
            assert!(matches!(err, AssetError::InsufficientBalance));

            // Allowance must NOT have been reduced
            assert_eq!(store.read_allowance(1, owner, burner).unwrap(), 500);
        }
    }

    #[test]
    fn test_batch_transfer_partial_commit() {
        let mut backend = TestBackend::new();
        let from = Address::repeat_byte(0xAB);
        let r1 = Address::repeat_byte(0x11);
        let r2 = Address::repeat_byte(0x22);
        {
            let mut store = AssetStorage::new(&mut backend);
            store.register("TEST", "Test", 18, 0, from, U256::ZERO).unwrap();
            store.write_balance(1, from, 150);

            // First recipient gets 100 (succeeds), second gets 100 (fails: 50 left)
            let err = store
                .batch_transfer(1, from, &[(r1, 100), (r2, 100)])
                .unwrap_err();
            assert!(matches!(err, AssetError::InsufficientBalance));

            // First transfer succeeded before failure (non-atomic)
            assert_eq!(store.read_balance(1, from).unwrap(), 50);
            assert_eq!(store.read_balance(1, r1).unwrap(), 100);
            assert_eq!(store.read_balance(1, r2).unwrap(), 0);
        }
    }

    #[test]
    fn test_store_meta_string_too_long() {
        let mut backend = TestBackend::new();
        let mut store = AssetStorage::new(&mut backend);
        // 33-byte string should fail
        let long = "a".repeat(33);
        let err = store.register(&long, "Test", 18, 0, Address::ZERO, U256::ZERO)
            .unwrap_err();
        assert!(matches!(err, AssetError::InvalidMetadata(_)));
    }

    #[test]
    fn test_read_meta_unregistered() {
        let mut backend = TestBackend::new();
        let mut store = AssetStorage::new(&mut backend);
        let err = store.read_meta(999).unwrap_err();
        assert!(matches!(err, AssetError::AssetNotFound));
    }

    #[test]
    fn test_register_invalid_decimals() {
        let mut backend = TestBackend::new();
        let mut store = AssetStorage::new(&mut backend);
        let err = store.register("TEST", "Test", 19, 0, Address::ZERO, U256::ZERO)
            .unwrap_err();
        assert!(matches!(err, AssetError::InvalidDecimals(19)));
    }
}
