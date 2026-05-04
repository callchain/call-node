pub mod precompile;

pub use precompile::AssetPrecompile;

use call_precompile::{
    slot_allowance, slot_asset_meta, slot_balance, u128_to_u256, u256_to_u128, ASSET_ADDRESS,
};
use call_protocol::storage_backend::StorageBackend;
use call_primitives::{Address, Balance, U256};

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

    pub fn read_balance(&self, asset_id: u64, addr: Address) -> Balance {
        let slot = slot_balance(asset_id, addr);
        self.backend
            .load(ASSET_ADDRESS, slot)
            .try_into()
            .map(|v: u128| v)
            .unwrap_or(0)
    }

    pub fn write_balance(&mut self, asset_id: u64, addr: Address, amount: Balance) {
        let slot = slot_balance(asset_id, addr);
        self.backend.store(ASSET_ADDRESS, slot, u128_to_u256(amount));
    }

    pub fn add_balance(
        &mut self,
        asset_id: u64,
        addr: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let current = self.read_balance(asset_id, addr);
        let new = current.checked_add(amount).ok_or(AssetError::BalanceOverflow)?;
        self.write_balance(asset_id, addr, new);
        Ok(())
    }

    pub fn deduct_balance(
        &mut self,
        asset_id: u64,
        addr: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let current = self.read_balance(asset_id, addr);
        let new = current
            .checked_sub(amount)
            .ok_or(AssetError::InsufficientBalance)?;
        self.write_balance(asset_id, addr, new);
        Ok(())
    }

    // ── Allowance operations ──────────────────────────────────────────

    pub fn read_allowance(&self, asset_id: u64, owner: Address, spender: Address) -> Balance {
        let slot = slot_allowance(asset_id, owner, spender);
        u256_to_u128(self.backend.load(ASSET_ADDRESS, slot))
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

    pub fn load_meta_u256(&self, asset_id: u64, key: &[u8]) -> U256 {
        self.backend.load(ASSET_ADDRESS, slot_asset_meta(asset_id, key))
    }

    pub fn load_meta_u128(&self, asset_id: u64, key: &[u8]) -> u128 {
        u256_to_u128(self.load_meta_u256(asset_id, key))
    }

    pub fn load_meta_u8(&self, asset_id: u64, key: &[u8]) -> u8 {
        self.load_meta_u256(asset_id, key)
            .to_be_bytes::<32>()[31]
    }

    pub fn load_meta_address(&self, asset_id: u64, key: &[u8]) -> Address {
        let v = self.load_meta_u256(asset_id, key);
        Address::from_slice(&v.to_be_bytes::<32>()[12..32])
    }

    pub fn load_meta_string(&self, asset_id: u64, key: &[u8]) -> String {
        let v = self.load_meta_u256(asset_id, key);
        let bytes = v.to_be_bytes::<32>();
        // Trim trailing nulls
        let len = bytes.iter().position(|&b| b == 0).unwrap_or(32);
        String::from_utf8_lossy(&bytes[..len]).into_owned()
    }

    pub fn store_meta_u256(&mut self, asset_id: u64, key: &[u8], value: U256) {
        self.backend
            .store(ASSET_ADDRESS, slot_asset_meta(asset_id, key), value);
    }

    pub fn read_meta(&self, asset_id: u64) -> AssetMeta {
        AssetMeta {
            symbol: self.load_meta_string(asset_id, b"symbol"),
            name: self.load_meta_string(asset_id, b"name"),
            decimals: self.load_meta_u8(asset_id, b"decimals"),
            issuer: self.load_meta_address(asset_id, b"issuer"),
            max_supply: self.load_meta_u128(asset_id, b"max_supply"),
            supply: self.load_meta_u128(asset_id, b"supply"),
            status: self.load_meta_u8(asset_id, b"status"),
        }
    }

    pub fn write_meta(&mut self, asset_id: u64, meta: &AssetMeta) {
        self.store_meta_string(asset_id, b"symbol", &meta.symbol);
        self.store_meta_string(asset_id, b"name", &meta.name);
        self.store_meta_u256(
            asset_id,
            b"decimals",
            U256::from(meta.decimals),
        );
        self.store_meta_u256(
            asset_id,
            b"issuer",
            address_to_u256(meta.issuer),
        );
        self.store_meta_u256(
            asset_id,
            b"max_supply",
            u128_to_u256(meta.max_supply),
        );
        self.store_meta_u256(asset_id, b"supply", u128_to_u256(meta.supply));
        self.store_meta_u256(asset_id, b"status", U256::from(meta.status));
    }

    fn store_meta_string(&mut self, asset_id: u64, key: &[u8], value: &str) {
        let mut bytes = [0u8; 32];
        let src = value.as_bytes();
        let len = src.len().min(32);
        bytes[..len].copy_from_slice(&src[..len]);
        self.backend.store(
            ASSET_ADDRESS,
            slot_asset_meta(asset_id, key),
            U256::from_be_bytes(bytes),
        );
    }

    // ── Business logic ────────────────────────────────────────────────

    pub fn transfer(
        &mut self,
        asset_id: u64,
        from: Address,
        to: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        self.deduct_balance(asset_id, from, amount)?;
        self.add_balance(asset_id, to, amount)?;
        Ok(())
    }

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
    ) {
        self.write_allowance(asset_id, owner, spender, amount);
    }

    pub fn transfer_from(
        &mut self,
        asset_id: u64,
        spender: Address,
        from: Address,
        to: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let allowance = self.read_allowance(asset_id, from, spender);
        if allowance < amount {
            return Err(AssetError::InsufficientAllowance);
        }
        self.write_allowance(asset_id, from, spender, allowance - amount);
        self.transfer(asset_id, from, to, amount)?;
        Ok(())
    }

    pub fn mint(
        &mut self,
        asset_id: u64,
        caller: Address,
        to: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        let meta = self.read_meta(asset_id);
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
        self.store_meta_u256(asset_id, b"supply", u128_to_u256(new_supply));
        self.add_balance(asset_id, to, amount)?;
        Ok(())
    }

    pub fn burn(
        &mut self,
        asset_id: u64,
        caller: Address,
        from: Address,
        amount: Balance,
    ) -> Result<(), AssetError> {
        if caller != from {
            let allowance = self.read_allowance(asset_id, from, caller);
            if allowance < amount {
                return Err(AssetError::InsufficientAllowance);
            }
            self.write_allowance(asset_id, from, caller, allowance - amount);
        }
        let supply = self.load_meta_u128(asset_id, b"supply");
        let new_supply = supply
            .checked_sub(amount)
            .ok_or(AssetError::SupplyUnderflow)?;
        self.store_meta_u256(asset_id, b"supply", u128_to_u256(new_supply));
        self.deduct_balance(asset_id, from, amount)?;
        Ok(())
    }

    pub fn register(
        &mut self,
        symbol: &str,
        name: &str,
        decimals: u8,
        max_supply: Balance,
        issuer: Address,
    ) -> Result<u64, AssetError> {
        let next_id_slot = U256::from(0);
        let asset_id = self
            .backend
            .load(ASSET_ADDRESS, next_id_slot)
            .try_into()
            .map(|v: u128| v as u64)
            .unwrap_or(0);
        let asset_id = if asset_id == 0 { 1 } else { asset_id };
        let next_id = asset_id.checked_add(1).ok_or(AssetError::BalanceOverflow)?;
        self.backend
            .store(ASSET_ADDRESS, next_id_slot, U256::from(next_id));

        self.store_meta_string(asset_id, b"symbol", symbol);
        self.store_meta_string(asset_id, b"name", name);
        self.store_meta_u256(asset_id, b"decimals", U256::from(decimals));
        self.store_meta_u256(asset_id, b"issuer", address_to_u256(issuer));
        self.store_meta_u256(asset_id, b"max_supply", u128_to_u256(max_supply));
        self.store_meta_u256(asset_id, b"supply", U256::from(0));
        self.store_meta_u256(asset_id, b"status", U256::from(0));
        self.store_meta_u256(asset_id, b"compliance", U256::from(0));
        self.store_meta_u256(asset_id, b"registered_at", U256::from(0));

        Ok(asset_id)
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
        fn load(&self, address: Address, slot: U256) -> U256 {
            self.storage.get(&(address, slot)).copied().unwrap_or(U256::ZERO)
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
        let store = AssetStorage::new(&backend);
        assert_eq!(store.read_balance(1, addr), 5000);
        assert_eq!(store.read_balance(1, Address::ZERO), 0);
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
        let store = AssetStorage::new(&backend);
        assert_eq!(store.read_balance(1, from), 500);
        assert_eq!(store.read_balance(1, to), 500);
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
            let asset_id = store.register("GOLD", "Gold Token", 18, 10000, issuer).unwrap();
            assert_eq!(asset_id, 1);

            // Mint
            store.mint(asset_id, issuer, recipient, 5000).unwrap();
            assert_eq!(store.read_balance(asset_id, recipient), 5000);
            assert_eq!(store.read_meta(asset_id).supply, 5000);

            // Burn (recipient burns their own tokens)
            store.burn(asset_id, recipient, recipient, 2000).unwrap();
            assert_eq!(store.read_balance(asset_id, recipient), 3000);
            assert_eq!(store.read_meta(asset_id).supply, 3000);
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
            let asset_id = store.register("GOLD", "Gold Token", 18, 10000, issuer).unwrap();
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
            store.write_balance(1, owner, 1000);
            store.approve(1, owner, spender, 500);
            assert_eq!(store.read_allowance(1, owner, spender), 500);

            store.transfer_from(1, spender, owner, to, 300).unwrap();
            assert_eq!(store.read_balance(1, owner), 700);
            assert_eq!(store.read_balance(1, to), 300);
            assert_eq!(store.read_allowance(1, owner, spender), 200);
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
            store.write_balance(1, owner, 1000);
            store.approve(1, owner, spender, 100);
            let err = store.transfer_from(1, spender, owner, to, 300).unwrap_err();
            assert!(matches!(err, AssetError::InsufficientAllowance));
        }
    }
}
