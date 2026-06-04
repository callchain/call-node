pub mod domain_verification;
pub mod precompile;

pub use domain_verification::{verify_domain, verify_domain_dns_txt, verify_domain_http_file};
pub use precompile::AgentPrecompile;

use call_asset::AssetStorage;
use call_precompile::{
    address_to_u256, storage::storage_slot, u128_to_u256, u256_to_address, u256_to_u128,
    u256_to_u64, u64_to_u256, write_string32, AGENT_ADDRESS,
};
use call_primitives::{Address, U256};
use call_protocol::storage_backend::StorageBackend;

/// Error type for agent operations.
#[derive(Debug)]
pub enum AgentError {
    NotFound,
    NotOwner,
    PermissionsExpired,
    AssetNotAllowed,
    AmountExceedsLimit,
    InsufficientBalance,
    BalanceOverflow,
    ArrayLengthMismatch,
    EmptyBatch,
    SessionNotFound,
    InvalidDelegate,
    AgentAlreadyExists,
}

impl std::fmt::Display for AgentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AgentError::NotFound => write!(f, "agent: not found"),
            AgentError::NotOwner => write!(f, "agent: sender is not owner"),
            AgentError::PermissionsExpired => write!(f, "agent: permissions expired"),
            AgentError::AssetNotAllowed => write!(f, "agent: asset not allowed"),
            AgentError::AmountExceedsLimit => write!(f, "agent: amount exceeds per-tx limit"),
            AgentError::InsufficientBalance => write!(f, "agent: insufficient balance"),
            AgentError::BalanceOverflow => write!(f, "agent: balance overflow"),
            AgentError::ArrayLengthMismatch => write!(f, "array length mismatch"),
            AgentError::EmptyBatch => write!(f, "empty batch"),
            AgentError::SessionNotFound => write!(f, "agent: session not found"),
            AgentError::InvalidDelegate => write!(f, "agent: invalid delegate"),
            AgentError::AgentAlreadyExists => write!(f, "agent: address already registered"),
        }
    }
}

impl std::error::Error for AgentError {}

/// Asset ID for the native CALL token.
pub const CALL_ASSET_ID: u64 = 1;

// ── Storage slot helpers ──────────────────────────────────────────────

pub fn slot_agent_count() -> U256 {
    U256::ZERO
}

pub fn slot_agent_owner(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"owner"])
}

pub fn slot_agent_address(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"addr"])
}

pub fn slot_agent_name(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"name"])
}

pub fn slot_agent_url(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"url"])
}

pub fn slot_agent_perms(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"perms"])
}

pub fn slot_agent_registered_at(agent_id: u64) -> U256 {
    storage_slot(&[b"agent", &agent_id.to_be_bytes()[..], b"block"])
}

pub fn slot_agent_balance(agent_id: u64, asset_id: u64) -> U256 {
    storage_slot(&[
        b"abalance",
        &agent_id.to_be_bytes()[..],
        &asset_id.to_be_bytes()[..],
    ])
}

pub fn slot_agent_by_address(addr: Address) -> U256 {
    storage_slot(&[b"agent_by_addr", addr.as_slice()])
}

/// Approximate blocks per day (5s block time).
pub const BLOCKS_PER_DAY: u64 = 17280;

// ── Session storage slot helpers ──────────────────────────────────────

pub fn slot_session_count() -> U256 {
    storage_slot(&[b"scount"])
}

pub fn slot_session_owner(session_id: u64) -> U256 {
    storage_slot(&[b"sowner", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_delegate(session_id: u64) -> U256 {
    storage_slot(&[b"sdelegate", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_limits(session_id: u64) -> U256 {
    storage_slot(&[b"slimits", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_expires(session_id: u64) -> U256 {
    storage_slot(&[b"sexpires", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_spent(session_id: u64) -> U256 {
    storage_slot(&[b"sspent", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_last_day(session_id: u64) -> U256 {
    storage_slot(&[b"slastday", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_allowed_assets_count(session_id: u64) -> U256 {
    storage_slot(&[b"saac", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_allowed_asset(session_id: u64, index: u64) -> U256 {
    storage_slot(&[
        b"saa",
        &session_id.to_be_bytes()[..],
        &index.to_be_bytes()[..],
    ])
}

pub fn slot_session_allowed_recipients_count(session_id: u64) -> U256 {
    storage_slot(&[b"sarc", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_allowed_recipient(session_id: u64, index: u64) -> U256 {
    storage_slot(&[
        b"sar",
        &session_id.to_be_bytes()[..],
        &index.to_be_bytes()[..],
    ])
}

pub fn slot_session_max_total_spend(session_id: u64) -> U256 {
    storage_slot(&[b"smts", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_min_interval_blocks(session_id: u64) -> U256 {
    storage_slot(&[b"smib", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_effective_at(session_id: u64) -> U256 {
    storage_slot(&[b"sea", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_max_executions(session_id: u64) -> U256 {
    storage_slot(&[b"sme", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_execution_count(session_id: u64) -> U256 {
    storage_slot(&[b"sec", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_last_execution_block(session_id: u64) -> U256 {
    storage_slot(&[b"sleb", &session_id.to_be_bytes()[..]])
}

pub fn slot_session_total_spent(session_id: u64) -> U256 {
    storage_slot(&[b"sts", &session_id.to_be_bytes()[..]])
}

// ── Permission packing ────────────────────────────────────────────────

/// Pack agent permissions into a single U256:
/// bytes 0..16  = per_tx_limit (u128)
/// bytes 16..24 = expires_at (u64)
/// byte 31      = flags (bit 0 = allow asset 1, bit 1 = allow all protocols)
pub fn pack_agent_perms(per_tx_limit: u128, expires_at: u64, flags: u8) -> U256 {
    let mut packed = [0u8; 32];
    packed[0..16].copy_from_slice(&per_tx_limit.to_be_bytes());
    packed[16..24].copy_from_slice(&expires_at.to_be_bytes());
    packed[31] = flags;
    U256::from_be_slice(&packed)
}

#[allow(clippy::unwrap_used)]
pub fn unpack_agent_perms(perms: U256) -> (u128, u64, u8) {
    let bytes = perms.to_be_bytes::<32>();
    let per_tx_limit =
        u128::from_be_bytes(bytes[0..16].try_into().expect("invariant: 16-byte slice"));
    let expires_at = u64::from_be_bytes(bytes[16..24].try_into().expect("invariant: 8-byte slice"));
    let flags = bytes[31];
    (per_tx_limit, expires_at, flags)
}

/// Pack session limits into a single U256:
/// bytes 0..16  = per_tx_limit (u128)
/// bytes 16..32 = daily_limit (u128)
pub fn pack_session_limits(per_tx_limit: u128, daily_limit: u128) -> U256 {
    let mut packed = [0u8; 32];
    packed[0..16].copy_from_slice(&per_tx_limit.to_be_bytes());
    packed[16..32].copy_from_slice(&daily_limit.to_be_bytes());
    U256::from_be_slice(&packed)
}

#[allow(clippy::unwrap_used)]
pub fn unpack_session_limits(limits: U256) -> (u128, u128) {
    let bytes = limits.to_be_bytes::<32>();
    let per_tx_limit =
        u128::from_be_bytes(bytes[0..16].try_into().expect("invariant: 16-byte slice"));
    let daily_limit =
        u128::from_be_bytes(bytes[16..32].try_into().expect("invariant: 16-byte slice"));
    (per_tx_limit, daily_limit)
}

// ── SessionPolicy ─────────────────────────────────────────────────────

/// Optional policy constraints for a session key.
/// All fields default to "unrestricted" (0 or empty).
#[derive(Debug, Clone)]
pub struct SessionPolicy {
    pub max_total_spend: u128,
    pub min_interval_blocks: u64,
    pub effective_at: u64,
    pub max_executions: u64,
    pub allowed_assets: Vec<u64>,
    pub allowed_recipients: Vec<Address>,
}

impl Default for SessionPolicy {
    fn default() -> Self {
        Self {
            max_total_spend: 0,
            min_interval_blocks: 0,
            effective_at: 0,
            max_executions: 0,
            allowed_assets: Vec::new(),
            allowed_recipients: Vec::new(),
        }
    }
}

// ── AgentStorage ──────────────────────────────────────────────────────

/// Business logic for agent operations backed by any StorageBackend.
pub struct AgentStorage<B: StorageBackend> {
    backend: B,
}

impl<B: StorageBackend> AgentStorage<B> {
    pub fn new(backend: B) -> Self {
        Self { backend }
    }

    // ── Read operations ───────────────────────────────────────────────

    pub fn read_count(&mut self) -> u64 {
        u256_to_u64(self.backend.load(AGENT_ADDRESS, slot_agent_count()))
    }

    pub fn read_owner(&mut self, agent_id: u64) -> Address {
        u256_to_address(self.backend.load(AGENT_ADDRESS, slot_agent_owner(agent_id)))
    }

    pub fn read_agent_address(&mut self, agent_id: u64) -> Address {
        u256_to_address(
            self.backend.load(AGENT_ADDRESS, slot_agent_address(agent_id)),
        )
    }

    pub fn find_agent_by_address(&mut self, addr: Address) -> u64 {
        let id = u256_to_u64(
            self.backend.load(AGENT_ADDRESS, slot_agent_by_address(addr)),
        );
        id
    }

    pub fn read_name(&mut self, agent_id: u64) -> [u8; 32] {
        self.backend
            .load(AGENT_ADDRESS, slot_agent_name(agent_id))
            .to_be_bytes::<32>()
    }

    pub fn read_url(&mut self, agent_id: u64) -> [u8; 32] {
        self.backend
            .load(AGENT_ADDRESS, slot_agent_url(agent_id))
            .to_be_bytes::<32>()
    }

    pub fn read_perms(&mut self, agent_id: u64) -> U256 {
        self.backend.load(AGENT_ADDRESS, slot_agent_perms(agent_id))
    }

    pub fn read_registered_at(&mut self, agent_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_agent_registered_at(agent_id)),
        )
    }

    pub fn read_agent_balance(&mut self, agent_id: u64, asset_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(AGENT_ADDRESS, slot_agent_balance(agent_id, asset_id)),
        )
    }

    pub fn agent_exists(&mut self, agent_id: u64) -> bool {
        self.read_owner(agent_id) != Address::ZERO
    }

    // ── Permission checks ─────────────────────────────────────────────

    pub fn check_owner(&mut self, agent_id: u64, caller: Address) -> Result<(), AgentError> {
        if self.read_owner(agent_id) != caller {
            return Err(AgentError::NotOwner);
        }
        Ok(())
    }

    pub fn check_agent(
        &mut self,
        agent_id: u64,
        caller: Address,
    ) -> Result<(), AgentError> {
        let agent = self.read_agent_address(agent_id);
        if caller != agent {
            return Err(AgentError::NotOwner);
        }
        Ok(())
    }

    pub fn require_perms(
        &mut self,
        agent_id: u64,
        asset_id: u64,
        current_block: u64,
    ) -> Result<(u128, u64, u8), AgentError> {
        let perms = self.read_perms(agent_id);
        let (per_tx_limit, expires_at, flags) = unpack_agent_perms(perms);
        if expires_at != 0 && current_block > expires_at {
            return Err(AgentError::PermissionsExpired);
        }
        if asset_id != CALL_ASSET_ID && (flags & 1) == 0 {
            return Err(AgentError::AssetNotAllowed);
        }
        Ok((per_tx_limit, expires_at, flags))
    }

    // ── Write operations ──────────────────────────────────────────────

    pub fn register_agent(
        &mut self,
        name: &str,
        url: &str,
        agent_address: Address,
        caller: Address,
        current_block: u64,
    ) -> Result<(), AgentError> {
        if agent_address == Address::ZERO {
            return Err(AgentError::AgentAlreadyExists);
        }
        let existing = self.find_agent_by_address(agent_address);
        if existing != u64::MAX && self.read_agent_address(existing) == agent_address {
            return Err(AgentError::AgentAlreadyExists);
        }

        let count = self.read_count();
        let agent_id = count;
        self.backend
            .store(AGENT_ADDRESS, slot_agent_count(), u64_to_u256(count + 1));
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_owner(agent_id),
            address_to_u256(caller),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_address(agent_id),
            address_to_u256(agent_address),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_by_address(agent_address),
            u64_to_u256(agent_id),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_name(agent_id),
            write_string32(name),
        );
        self.backend
            .store(AGENT_ADDRESS, slot_agent_url(agent_id), write_string32(url));
        // Default perms: per_tx_limit=1_000, expires_at=0, flags=1 (allow asset 1)
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_perms(agent_id),
            pack_agent_perms(1_000, 0, 1),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_registered_at(agent_id),
            u64_to_u256(current_block),
        );
        Ok(())
    }

    pub fn grant_balance(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        agent_id: u64,
        asset_id: u64,
        amount: u128,
        caller: Address,
    ) -> Result<(), AgentError> {
        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        asset_store
            .deduct_balance(asset_id, caller, amount)
            .map_err(|_| AgentError::InsufficientBalance)?;

        let agent_bal = self
            .read_agent_balance(agent_id, asset_id)
            .checked_add(amount)
            .ok_or(AgentError::BalanceOverflow)?;
        self.write_agent_balance(agent_id, asset_id, agent_bal);
        Ok(())
    }

    pub fn revoke_balance(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        agent_id: u64,
        asset_id: u64,
        caller: Address,
    ) -> Result<(), AgentError> {
        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        let current_bal = self.read_agent_balance(agent_id, asset_id);
        self.write_agent_balance(agent_id, asset_id, 0);
        if current_bal > 0 {
            asset_store
                .add_balance(asset_id, caller, current_bal)
                .map_err(|_| AgentError::BalanceOverflow)?;
        }
        Ok(())
    }

    pub fn pay(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        asset_id: u64,
        to: Address,
        amount: u128,
        caller: Address,
        current_block: u64,
    ) -> Result<(), AgentError> {
        let agent_id = self.find_agent_by_address(caller);
        if !self.agent_exists(agent_id) || self.read_agent_address(agent_id) != caller {
            return Err(AgentError::NotFound);
        }

        let (per_tx_limit, _, _) = self.require_perms(agent_id, asset_id, current_block)?;
        if amount > per_tx_limit {
            return Err(AgentError::AmountExceedsLimit);
        }

        let agent_bal = self
            .read_agent_balance(agent_id, asset_id)
            .checked_sub(amount)
            .ok_or(AgentError::InsufficientBalance)?;
        self.write_agent_balance(agent_id, asset_id, agent_bal);

        asset_store
            .add_balance(asset_id, to, amount)
            .map_err(|_| AgentError::BalanceOverflow)?;
        Ok(())
    }

    pub fn batch_pay(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        asset_id: u64,
        recipients: &[Address],
        amounts: &[u128],
        caller: Address,
        current_block: u64,
    ) -> Result<(), AgentError> {
        if recipients.len() != amounts.len() {
            return Err(AgentError::ArrayLengthMismatch);
        }
        if recipients.is_empty() {
            return Err(AgentError::EmptyBatch);
        }

        let agent_id = self.find_agent_by_address(caller);
        if !self.agent_exists(agent_id) || self.read_agent_address(agent_id) != caller {
            return Err(AgentError::NotFound);
        }

        let (per_tx_limit, _, _) = self.require_perms(agent_id, asset_id, current_block)?;

        let total_amount: u128 = amounts.iter().copied().sum();
        for amount in amounts.iter() {
            if *amount > per_tx_limit {
                return Err(AgentError::AmountExceedsLimit);
            }
        }

        let agent_bal = self
            .read_agent_balance(agent_id, asset_id)
            .checked_sub(total_amount)
            .ok_or(AgentError::InsufficientBalance)?;
        self.write_agent_balance(agent_id, asset_id, agent_bal);

        for (recipient, amount) in recipients.iter().zip(amounts.iter()) {
            asset_store
                .add_balance(asset_id, *recipient, *amount)
                .map_err(|_| AgentError::BalanceOverflow)?;
        }
        Ok(())
    }

    pub fn revoke_agent(&mut self, agent_id: u64, caller: Address) -> Result<(), AgentError> {
        if !self.agent_exists(agent_id) {
            return Err(AgentError::NotFound);
        }
        self.check_owner(agent_id, caller)?;

        let addr = self.read_agent_address(agent_id);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_owner(agent_id), U256::ZERO);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_address(agent_id), U256::ZERO);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_name(agent_id), U256::ZERO);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_url(agent_id), U256::ZERO);
        self.backend
            .store(AGENT_ADDRESS, slot_agent_perms(agent_id), U256::ZERO);
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_registered_at(agent_id),
            U256::ZERO,
        );
        if addr != Address::ZERO {
            self.backend
                .store(AGENT_ADDRESS, slot_agent_by_address(addr), U256::ZERO);
        }
        Ok(())
    }

    // ── Session operations ────────────────────────────────────────────

    pub fn read_session_count(&mut self) -> u64 {
        u256_to_u64(self.backend.load(AGENT_ADDRESS, slot_session_count()))
    }

    pub fn read_session_owner(&mut self, session_id: u64) -> Address {
        u256_to_address(
            self.backend
                .load(AGENT_ADDRESS, slot_session_owner(session_id)),
        )
    }

    pub fn read_session_delegate(&mut self, session_id: u64) -> Address {
        u256_to_address(
            self.backend
                .load(AGENT_ADDRESS, slot_session_delegate(session_id)),
        )
    }

    pub fn read_session_limits(&mut self, session_id: u64) -> (u128, u128) {
        let limits = self
            .backend
            .load(AGENT_ADDRESS, slot_session_limits(session_id));
        unpack_session_limits(limits)
    }

    pub fn read_session_expires(&mut self, session_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_expires(session_id)),
        )
    }

    pub fn read_session_spent(&mut self, session_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(AGENT_ADDRESS, slot_session_spent(session_id)),
        )
    }

    pub fn read_session_last_day(&mut self, session_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_last_day(session_id)),
        )
    }

    pub fn read_session_allowed_assets(&mut self, session_id: u64) -> Vec<u64> {
        let count = u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_allowed_assets_count(session_id)),
        );
        (0..count)
            .map(|i| {
                u256_to_u64(
                    self.backend
                        .load(AGENT_ADDRESS, slot_session_allowed_asset(session_id, i)),
                )
            })
            .collect()
    }

    pub fn read_session_allowed_recipients(&mut self, session_id: u64) -> Vec<Address> {
        let count = u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_allowed_recipients_count(session_id)),
        );
        (0..count)
            .map(|i| {
                u256_to_address(
                    self.backend
                        .load(AGENT_ADDRESS, slot_session_allowed_recipient(session_id, i)),
                )
            })
            .collect()
    }

    pub fn read_session_max_total_spend(&mut self, session_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(AGENT_ADDRESS, slot_session_max_total_spend(session_id)),
        )
    }

    pub fn read_session_min_interval_blocks(&mut self, session_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_min_interval_blocks(session_id)),
        )
    }

    pub fn read_session_effective_at(&mut self, session_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_effective_at(session_id)),
        )
    }

    pub fn read_session_max_executions(&mut self, session_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_max_executions(session_id)),
        )
    }

    pub fn read_session_execution_count(&mut self, session_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_execution_count(session_id)),
        )
    }

    pub fn read_session_last_execution_block(&mut self, session_id: u64) -> u64 {
        u256_to_u64(
            self.backend
                .load(AGENT_ADDRESS, slot_session_last_execution_block(session_id)),
        )
    }

    pub fn read_session_total_spent(&mut self, session_id: u64) -> u128 {
        u256_to_u128(
            self.backend
                .load(AGENT_ADDRESS, slot_session_total_spent(session_id)),
        )
    }

    pub fn session_exists(&mut self, session_id: u64) -> bool {
        self.read_session_owner(session_id) != Address::ZERO
    }

    pub fn create_session(
        &mut self,
        delegate: Address,
        per_tx_limit: u128,
        daily_limit: u128,
        expires_at: u64,
        policy: &SessionPolicy,
        caller: Address,
    ) -> Result<u64, AgentError> {
        let count = self.read_session_count();
        let session_id = count;

        self.backend.store(
            AGENT_ADDRESS,
            slot_session_count(),
            u64_to_u256(count + 1),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_owner(session_id),
            address_to_u256(caller),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_delegate(session_id),
            address_to_u256(delegate),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_limits(session_id),
            pack_session_limits(per_tx_limit, daily_limit),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_expires(session_id),
            u64_to_u256(expires_at),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_spent(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_last_day(session_id),
            U256::ZERO,
        );

        // Policy fields
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_allowed_assets_count(session_id),
            u64_to_u256(policy.allowed_assets.len() as u64),
        );
        for (i, &asset) in policy.allowed_assets.iter().enumerate() {
            self.backend.store(
                AGENT_ADDRESS,
                slot_session_allowed_asset(session_id, i as u64),
                u64_to_u256(asset),
            );
        }
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_allowed_recipients_count(session_id),
            u64_to_u256(policy.allowed_recipients.len() as u64),
        );
        for (i, &addr) in policy.allowed_recipients.iter().enumerate() {
            self.backend.store(
                AGENT_ADDRESS,
                slot_session_allowed_recipient(session_id, i as u64),
                address_to_u256(addr),
            );
        }
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_max_total_spend(session_id),
            u128_to_u256(policy.max_total_spend),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_min_interval_blocks(session_id),
            u64_to_u256(policy.min_interval_blocks),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_effective_at(session_id),
            u64_to_u256(policy.effective_at),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_max_executions(session_id),
            u64_to_u256(policy.max_executions),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_execution_count(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_last_execution_block(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_total_spent(session_id),
            U256::ZERO,
        );

        Ok(session_id)
    }

    pub fn revoke_session(
        &mut self,
        session_id: u64,
        caller: Address,
    ) -> Result<(), AgentError> {
        let owner = self.read_session_owner(session_id);
        if owner == Address::ZERO {
            return Err(AgentError::SessionNotFound);
        }
        if owner != caller {
            return Err(AgentError::NotOwner);
        }

        self.backend.store(
            AGENT_ADDRESS,
            slot_session_owner(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_delegate(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_limits(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_expires(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_spent(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_last_day(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_allowed_assets_count(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_allowed_recipients_count(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_max_total_spend(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_min_interval_blocks(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_effective_at(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_max_executions(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_execution_count(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_last_execution_block(session_id),
            U256::ZERO,
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_total_spent(session_id),
            U256::ZERO,
        );

        Ok(())
    }

    pub fn is_session_valid(
        &mut self,
        session_id: u64,
        current_block: u64,
    ) -> bool {
        if !self.session_exists(session_id) {
            return false;
        }
        let effective_at = self.read_session_effective_at(session_id);
        if effective_at != 0 && current_block < effective_at {
            return false;
        }
        let expires_at = self.read_session_expires(session_id);
        if expires_at != 0 && current_block > expires_at {
            return false;
        }
        true
    }

    pub fn execute_session_transfer(
        &mut self,
        asset_store: &mut AssetStorage<B>,
        session_id: u64,
        asset_id: u64,
        to: Address,
        amount: u128,
        caller: Address,
        current_block: u64,
    ) -> Result<(), AgentError> {
        if !self.session_exists(session_id) {
            return Err(AgentError::SessionNotFound);
        }

        let delegate = self.read_session_delegate(session_id);
        if caller != delegate {
            return Err(AgentError::InvalidDelegate);
        }

        let expires_at = self.read_session_expires(session_id);
        if expires_at != 0 && current_block > expires_at {
            return Err(AgentError::SessionNotFound);
        }

        // effectiveAt
        let effective_at = self.read_session_effective_at(session_id);
        if effective_at != 0 && current_block < effective_at {
            return Err(AgentError::SessionNotFound);
        }

        // minIntervalBlocks
        let min_interval = self.read_session_min_interval_blocks(session_id);
        if min_interval != 0 {
            let last_block = self.read_session_last_execution_block(session_id);
            if last_block != 0 && current_block - last_block < min_interval {
                return Err(AgentError::AmountExceedsLimit);
            }
        }

        // maxExecutions
        let max_executions = self.read_session_max_executions(session_id);
        if max_executions != 0 {
            let exec_count = self.read_session_execution_count(session_id);
            if exec_count >= max_executions {
                return Err(AgentError::AmountExceedsLimit);
            }
        }

        let (per_tx_limit, daily_limit) = self.read_session_limits(session_id);
        if amount > per_tx_limit {
            return Err(AgentError::AmountExceedsLimit);
        }

        let last_day = self.read_session_last_day(session_id);
        let spent = self.read_session_spent(session_id);
        let current_day = current_block / BLOCKS_PER_DAY;

        let (current_spent, new_last_day) = if current_day > last_day {
            (0u128, current_day)
        } else {
            (spent, last_day)
        };

        if current_spent + amount > daily_limit {
            return Err(AgentError::AmountExceedsLimit);
        }

        // maxTotalSpend
        let max_total = self.read_session_max_total_spend(session_id);
        if max_total != 0 {
            let total_spent = self.read_session_total_spent(session_id);
            if total_spent + amount > max_total {
                return Err(AgentError::AmountExceedsLimit);
            }
        }

        // allowedAssets
        let allowed_assets = self.read_session_allowed_assets(session_id);
        if !allowed_assets.is_empty() && !allowed_assets.contains(&asset_id) {
            return Err(AgentError::AssetNotAllowed);
        }

        // allowedRecipients
        let allowed_recipients = self.read_session_allowed_recipients(session_id);
        if !allowed_recipients.is_empty() && !allowed_recipients.contains(&to) {
            return Err(AgentError::AmountExceedsLimit);
        }

        // Update counters
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_spent(session_id),
            u128_to_u256(current_spent + amount),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_last_day(session_id),
            u64_to_u256(new_last_day),
        );

        let exec_count = self.read_session_execution_count(session_id);
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_execution_count(session_id),
            u64_to_u256(exec_count + 1),
        );
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_last_execution_block(session_id),
            u64_to_u256(current_block),
        );

        let total_spent = self.read_session_total_spent(session_id);
        self.backend.store(
            AGENT_ADDRESS,
            slot_session_total_spent(session_id),
            u128_to_u256(total_spent + amount),
        );

        let owner = self.read_session_owner(session_id);
        asset_store
            .deduct_balance(asset_id, owner, amount)
            .map_err(|_| AgentError::InsufficientBalance)?;

        asset_store
            .add_balance(asset_id, to, amount)
            .map_err(|_| AgentError::BalanceOverflow)?;

        Ok(())
    }

    // ── Internal helpers ──────────────────────────────────────────────

    fn write_agent_balance(&mut self, agent_id: u64, asset_id: u64, amount: u128) {
        self.backend.store(
            AGENT_ADDRESS,
            slot_agent_balance(agent_id, asset_id),
            u128_to_u256(amount),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_asset::AssetStorage;
    use call_primitives::Address;
    use call_protocol::storage_backend::StorageBackend;
    use std::cell::RefCell;
    use std::collections::HashMap;
    use std::rc::Rc;

    #[derive(Clone)]
    struct TestBackend {
        storage: Rc<RefCell<HashMap<(Address, U256), U256>>>,
    }

    impl TestBackend {
        fn new() -> Self {
            Self {
                storage: Rc::new(RefCell::new(HashMap::new())),
            }
        }
    }

    impl StorageBackend for TestBackend {
        fn load(&mut self, address: Address, slot: U256) -> U256 {
            self.storage
                .borrow()
                .get(&(address, slot))
                .copied()
                .unwrap_or_default()
        }
        fn store(&mut self, address: Address, slot: U256, value: U256) {
            self.storage.borrow_mut().insert((address, slot), value);
        }
    }

    #[test]
    fn test_register_and_read_agent() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("TestAgent", "http://test.com", Address::repeat_byte(0xBB), caller, 100)
            .unwrap();

        assert_eq!(store.read_count(), 1);
        assert_eq!(store.read_owner(0), caller);
        assert_eq!(&store.read_name(0)[0..9], b"TestAgent");
        assert_eq!(&store.read_url(0)[0..15], b"http://test.com");
        assert_eq!(store.read_agent_address(0), Address::repeat_byte(0xBB));
        assert_eq!(store.read_registered_at(0), 100);
        assert!(store.agent_exists(0));
    }

    #[test]
    fn test_check_owner() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("A", "url", Address::repeat_byte(0xBB), caller, 1)
            .unwrap();

        assert!(store.check_owner(0, caller).is_ok());
        assert!(matches!(
            store.check_owner(0, Address::repeat_byte(0x99)),
            Err(AgentError::NotOwner)
        ));
    }

    #[test]
    fn test_require_perms_expired() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("A", "url", Address::repeat_byte(0xBB), caller, 1)
            .unwrap();
        // Override perms: expires_at=50
        store.backend.store(
            AGENT_ADDRESS,
            slot_agent_perms(0),
            pack_agent_perms(1000, 50, 1),
        );

        assert!(matches!(
            store.require_perms(0, CALL_ASSET_ID, 100),
            Err(AgentError::PermissionsExpired)
        ));
    }

    #[test]
    fn test_require_perms_asset_not_allowed() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("A", "url", Address::repeat_byte(0xBB), caller, 1)
            .unwrap();
        // Override perms: flags=0 (bit 0 clear = asset 1 not allowed)
        store.backend.store(
            AGENT_ADDRESS,
            slot_agent_perms(0),
            pack_agent_perms(1000, 0, 0),
        );
        assert!(matches!(
            store.require_perms(0, 2, 1),
            Err(AgentError::AssetNotAllowed)
        ));
    }

    #[test]
    fn test_grant_pay_withdraw_revoke_balance() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);
        let agent = Address::repeat_byte(0xBB);
        let recipient = Address::repeat_byte(0x33);

        agent_store
            .register_agent("A", "url", agent, caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);

        // Grant (owner only)
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 5_000);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, caller).unwrap(), 5_000);

        // Pay (agent only)
        agent_store
            .pay(
                &mut asset_store,
                CALL_ASSET_ID,
                recipient,
                1_000,
                agent,
                1,
            )
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 4_000);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, recipient).unwrap(), 1_000);

        // Revoke balance (owner only)
        agent_store
            .revoke_balance(&mut asset_store, 0, CALL_ASSET_ID, caller)
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 0);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, caller).unwrap(), 9_000);
    }

    #[test]
    fn test_batch_pay() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);
        let agent = Address::repeat_byte(0xBB);
        let r1 = Address::repeat_byte(0x33);
        let r2 = Address::repeat_byte(0x44);

        agent_store
            .register_agent("A", "url", agent, caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();

        agent_store
            .batch_pay(
                &mut asset_store,
                CALL_ASSET_ID,
                &[r1, r2],
                &[500, 800],
                agent,
                1,
            )
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, CALL_ASSET_ID), 3_700);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, r1).unwrap(), 500);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, r2).unwrap(), 800);
    }

    #[test]
    fn test_batch_pay_array_mismatch() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);
        let agent = Address::repeat_byte(0xBB);

        agent_store
            .register_agent("A", "url", agent, caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();

        assert!(matches!(
            agent_store.batch_pay(
                &mut asset_store,
                CALL_ASSET_ID,
                &[Address::repeat_byte(0x33)],
                &[1_000, 2_000],
                agent,
                1,
            ),
            Err(AgentError::ArrayLengthMismatch)
        ));
    }

    #[test]
    fn test_batch_pay_empty() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);
        let agent = Address::repeat_byte(0xBB);

        agent_store
            .register_agent("A", "url", agent, caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();

        assert!(matches!(
            agent_store.batch_pay(&mut asset_store, CALL_ASSET_ID, &[], &[], agent, 1,),
            Err(AgentError::EmptyBatch)
        ));
    }

    #[test]
    fn test_pay_amount_exceeds_limit() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);
        let agent = Address::repeat_byte(0xBB);

        agent_store
            .register_agent("A", "url", agent, caller, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, caller, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, caller)
            .unwrap();

        // Default per_tx_limit is 1_000
        assert!(matches!(
            agent_store.pay(
                &mut asset_store,
                CALL_ASSET_ID,
                Address::repeat_byte(0x33),
                2_000,
                agent,
                1,
            ),
            Err(AgentError::AmountExceedsLimit)
        ));
    }

    #[test]
    fn test_revoke_agent() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        store
            .register_agent("A", "url", Address::repeat_byte(0xBB), caller, 1)
            .unwrap();
        assert!(store.agent_exists(0));

        store.revoke_agent(0, caller).unwrap();
        assert!(!store.agent_exists(0));
        assert_eq!(store.read_owner(0), Address::ZERO);
    }

    #[test]
    fn test_not_found_errors() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        assert!(matches!(
            agent_store.grant_balance(&mut asset_store, 0, 1, 100, caller),
            Err(AgentError::NotFound)
        ));
        assert!(matches!(
            agent_store.revoke_balance(&mut asset_store, 0, 1, caller),
            Err(AgentError::NotFound)
        ));
        assert!(matches!(
            agent_store.pay(&mut asset_store, 1, Address::ZERO, 100, caller, 1,),
            Err(AgentError::NotFound)
        ));
        assert!(matches!(
            agent_store.revoke_agent(0, caller),
            Err(AgentError::NotFound)
        ));
    }

    #[test]
    fn test_pack_unpack_perms() {
        let per_tx_limit = 12345u128;
        let expires_at = 67890u64;
        let flags = 0b101u8;

        let packed = pack_agent_perms(per_tx_limit, expires_at, flags);
        let (limit, expiry, f) = unpack_agent_perms(packed);

        assert_eq!(limit, per_tx_limit);
        assert_eq!(expiry, expires_at);
        assert_eq!(f, flags);
    }

    #[test]
    fn test_grant_balance_u128_overflow_rejected() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let caller = Address::repeat_byte(0x22);

        // Register agent and seed caller balance
        agent_store
            .register_agent("test", "https://test.com", Address::repeat_byte(0xBB), caller, 1)
            .unwrap();
        asset_store.write_balance(1, caller, u128::MAX);

        // Grant u128::MAX to agent (should succeed from zero)
        agent_store
            .grant_balance(&mut asset_store, 0, 1, u128::MAX, caller)
            .unwrap();
        assert_eq!(agent_store.read_agent_balance(0, 1), u128::MAX);

        // Caller now has 0 balance. Try to grant 1 more — should overflow.
        // We need to give caller 1 more token first
        asset_store.write_balance(1, caller, 1);
        let result = agent_store.grant_balance(&mut asset_store, 0, 1, 1, caller);
        assert!(
            matches!(result, Err(AgentError::BalanceOverflow)),
            "grant_balance with u128::MAX + 1 should return BalanceOverflow, got {:?}",
            result
        );

        // Agent balance should remain at u128::MAX
        assert_eq!(agent_store.read_agent_balance(0, 1), u128::MAX);
    }

    // ── Session key tests ─────────────────────────────────────────────

    #[test]
    fn test_create_session() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);

        store
            .register_agent("A", "url", Address::repeat_byte(0xBB), owner, 1)
            .unwrap();

        let session_id = store
            .create_session(delegate, 1_000, 5_000, 100, &SessionPolicy::default(), owner)
            .unwrap();
        assert_eq!(session_id, 0);

        assert_eq!(store.read_session_count(), 1);
        assert_eq!(store.read_session_owner(0), owner);
        assert_eq!(store.read_session_delegate(0), delegate);
        assert_eq!(store.read_session_limits(0), (1_000, 5_000));
        assert_eq!(store.read_session_expires(0), 100);
        assert_eq!(store.read_session_spent(0), 0);
        assert_eq!(store.read_session_last_day(0), 0);
    }

    #[test]
    fn test_create_session_not_owner_fails() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);

        store
            .register_agent("A", "url", Address::repeat_byte(0xBB), owner, 1)
            .unwrap();
        store.create_session(delegate, 1_000, 5_000, 100, &SessionPolicy::default(), owner).unwrap();

        assert!(matches!(
            store.revoke_session(0, Address::repeat_byte(0x99)),
            Err(AgentError::NotOwner)
        ));
    }

    #[test]
    fn test_revoke_session() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);

        store.create_session(delegate, 1_000, 5_000, 100, &SessionPolicy::default(), owner).unwrap();

        store.revoke_session(0, owner).unwrap();
        assert!(!store.session_exists(0));
        assert_eq!(store.read_session_delegate(0), Address::ZERO);
        assert_eq!(store.read_session_limits(0), (0, 0));
    }

    #[test]
    fn test_is_session_valid() {
        let backend = TestBackend::new();
        let mut store = AgentStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);

        store
            .register_agent("A", "url", Address::repeat_byte(0xBB), owner, 1)
            .unwrap();
        store.create_session(delegate, 1_000, 5_000, 100, &SessionPolicy::default(), owner).unwrap();

        assert!(store.is_session_valid(0, 50));
        assert!(!store.is_session_valid(0, 101));
        assert!(!store.is_session_valid(1, 50));
    }

    #[test]
    fn test_execute_session_transfer() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 100, &SessionPolicy::default(), owner)
            .unwrap();

        agent_store
            .execute_session_transfer(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                recipient,
                800,
                delegate,
                50,
            )
            .unwrap();

        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, owner).unwrap(), 9_200);
        assert_eq!(asset_store.read_balance(CALL_ASSET_ID, recipient).unwrap(), 800);
        assert_eq!(agent_store.read_session_spent(0), 800);
    }

    #[test]
    fn test_execute_session_transfer_exceeds_per_tx_limit() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        agent_store
            .register_agent("A", "url", Address::repeat_byte(0xBB), owner, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .grant_balance(&mut asset_store, 0, CALL_ASSET_ID, 5_000, owner)
            .unwrap();
        agent_store
            .create_session(delegate, 1_000, 5_000, 100, &SessionPolicy::default(), owner)
            .unwrap();

        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                recipient,
                2_000,
                delegate,
                50,
            ),
            Err(AgentError::AmountExceedsLimit)
        ));
    }

    #[test]
    fn test_execute_session_transfer_daily_limit() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        agent_store
            .register_agent("A", "url", Address::repeat_byte(0xBB), owner, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 1_500, 0, &SessionPolicy::default(), owner)
            .unwrap();

        // First transfer: 1_000
        agent_store
            .execute_session_transfer(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                recipient,
                1_000,
                delegate,
                50,
            )
            .unwrap();

        // Second transfer same day: 600 exceeds daily limit (1_500)
        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                recipient,
                600,
                delegate,
                50,
            ),
            Err(AgentError::AmountExceedsLimit)
        ));

        // Next day: limit resets
        let next_day = BLOCKS_PER_DAY + 1;
        agent_store
            .execute_session_transfer(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                recipient,
                600,
                delegate,
                next_day,
            )
            .unwrap();
    }

    #[test]
    fn test_execute_session_transfer_invalid_delegate() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let other = Address::repeat_byte(0x55);
        let recipient = Address::repeat_byte(0x44);

        agent_store
            .register_agent("A", "url", Address::repeat_byte(0xBB), owner, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 100, &SessionPolicy::default(), owner)
            .unwrap();

        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                recipient,
                500,
                other,
                50,
            ),
            Err(AgentError::InvalidDelegate)
        ));
    }

    #[test]
    fn test_execute_session_transfer_expired() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        agent_store
            .register_agent("A", "url", Address::repeat_byte(0xBB), owner, 1)
            .unwrap();
        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 100, &SessionPolicy::default(), owner)
            .unwrap();

        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store,
                0,
                CALL_ASSET_ID,
                recipient,
                500,
                delegate,
                101,
            ),
            Err(AgentError::SessionNotFound)
        ));
    }

    #[test]
    fn test_session_allowed_assets() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        asset_store.write_balance(2, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 0, &SessionPolicy { allowed_assets: vec![CALL_ASSET_ID], ..SessionPolicy::default() }, owner)
            .unwrap();

        // Allowed asset
        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 500, delegate, 1,
            )
            .unwrap();

        // Disallowed asset
        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store, 0, 2, recipient, 500, delegate, 1,
            ),
            Err(AgentError::AssetNotAllowed)
        ));
    }

    #[test]
    fn test_session_allowed_recipients() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let allowed = Address::repeat_byte(0x44);
        let blocked = Address::repeat_byte(0x55);

        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 0, &SessionPolicy { allowed_recipients: vec![allowed], ..SessionPolicy::default() }, owner)
            .unwrap();

        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, allowed, 500, delegate, 1,
            )
            .unwrap();

        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, blocked, 500, delegate, 1,
            ),
            Err(AgentError::AmountExceedsLimit)
        ));
    }

    #[test]
    fn test_session_max_total_spend() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 0, &SessionPolicy { max_total_spend: 1_500, ..SessionPolicy::default() }, owner)
            .unwrap();

        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 1_000, delegate, 1,
            )
            .unwrap();

        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 400, delegate, 2,
            )
            .unwrap();

        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 200, delegate, 3,
            ),
            Err(AgentError::AmountExceedsLimit)
        ));
    }

    #[test]
    fn test_session_min_interval_blocks() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 0, &SessionPolicy { min_interval_blocks: 5, ..SessionPolicy::default() }, owner)
            .unwrap();

        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 100, delegate, 10,
            )
            .unwrap();

        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 100, delegate, 12,
            ),
            Err(AgentError::AmountExceedsLimit)
        ));

        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 100, delegate, 16,
            )
            .unwrap();
    }

    #[test]
    fn test_session_effective_at() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 100, &SessionPolicy { effective_at: 50, ..SessionPolicy::default() }, owner)
            .unwrap();

        assert!(!agent_store.is_session_valid(0, 40));
        assert!(agent_store.is_session_valid(0, 50));
        assert!(agent_store.is_session_valid(0, 60));

        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 100, delegate, 40,
            ),
            Err(AgentError::SessionNotFound)
        ));

        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 100, delegate, 50,
            )
            .unwrap();
    }

    #[test]
    fn test_session_max_executions() {
        let backend = TestBackend::new();
        let mut agent_store = AgentStorage::new(backend.clone());
        let mut asset_store = AssetStorage::new(backend.clone());
        let owner = Address::repeat_byte(0x22);
        let delegate = Address::repeat_byte(0x33);
        let recipient = Address::repeat_byte(0x44);

        asset_store.write_balance(CALL_ASSET_ID, owner, 10_000);
        agent_store
            .create_session(delegate, 1_000, 5_000, 0, &SessionPolicy { max_executions: 2, ..SessionPolicy::default() }, owner)
            .unwrap();

        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 100, delegate, 1,
            )
            .unwrap();
        agent_store
            .execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 100, delegate, 2,
            )
            .unwrap();

        assert!(matches!(
            agent_store.execute_session_transfer(
                &mut asset_store, 0, CALL_ASSET_ID, recipient, 100, delegate, 3,
            ),
            Err(AgentError::AmountExceedsLimit)
        ));
    }
}
