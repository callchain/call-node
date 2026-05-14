# Callchain Agent Payment Layer

## Overview

The Agent Layer (`crates/agent`) enables delegated balance management and payments on behalf of users. An agent is a registered entity (e.g., a dApp, service, or automated wallet) that operates within owner-defined constraints. Session keys provide a separate mechanism for time-bound, constraint-delegated spending directly from the owner's protocol balance.

**Key features:**
- Agent registration with metadata (name, url, agent_address)
- Agent-specific balance management (owner-funded protocol-layer sub-accounts)
- Agent-mediated single and batch payments
- Per-transaction limits and asset allowlists per agent
- Session keys with 10 optional constraints for delegate-authorized spending

---

## Precompile API

The **Agent precompile at `0x209`** exposes agent and session operations via standard EVM transactions.

### Agent Operations

| Operation | Function | Caller | Base Gas |
|---|---|---|---|
| Register agent | `registerAgent(string name, string url, address agentAddress)` | owner | 6,000 |
| Grant balance | `grantBalance(uint64 agentId, uint64 assetId, uint128 amount)` | owner | 6,000 |
| Revoke balance | `revokeBalance(uint64 agentId, uint64 assetId)` | owner | 6,000 |
| Pay | `pay(uint64 assetId, address to, uint128 amount)` | **agent_address** | 30,000 |
| Batch pay | `batchPay(uint64 assetId, address[] to, uint128[] amounts)` | **agent_address** | 30,000 |
| Revoke agent | `revokeAgent(uint64 agentId)` | owner | 20,000 |
| Get agent owner | `getAgentOwner(uint64 agentId) → address` | anyone | 2,000 |
| Get agent address | `getAgentAddress(uint64 agentId) → address` | anyone | 2,000 |
| Get agent balance | `getAgentBalance(uint64 agentId, uint64 assetId) → uint128` | anyone | 2,000 |
| Get agent name | `getAgentName(uint64 agentId) → bytes32` | anyone | 2,000 |
| Get agent url | `getAgentUrl(uint64 agentId) → bytes32` | anyone | 2,000 |
| Get agent perms | `getAgentPerms(uint64 agentId) → uint256` | anyone | 2,000 |

### Session Key Operations

| Operation | Function | Caller | Base Gas |
|---|---|---|---|
| Create session | `createSession(address delegate, uint128 perTxLimit, uint128 dailyLimit, uint64 expiresAt, uint128 maxTotalSpend, uint64 minIntervalBlocks, uint64 effectiveAt, uint64 maxExecutions, uint64[] allowedAssets, address[] allowedRecipients) → uint64` | owner | 10,000 |
| Revoke session | `revokeSession(uint64 sessionId)` | owner | 6,000 |
| Is session valid | `isSessionValid(uint64 sessionId) → uint64` | anyone | 2,000 |
| Execute session | `executeSession(uint64 sessionId, uint64 assetId, address to, uint128 amount)` | **delegate** | 30,000 |

Gas is dynamically metered: `gas_used = base_gas + sloads*50 + sstores*500`.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Agent Payment Layer                                         │
│                                                             │
│  ┌──────────────────────────────────────────────────────┐  │
│  │ AgentStorage<B: StorageBackend>                      │  │
│  │                                                      │  │
│  │  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  │
│  │  │ AgentRegistry│  │ AgentBalances│  │  Sessions   │  │
│  │  │ - owner     │  │ - (id,asset)│  │ - owner     │  │
│  │  │ - addr      │  │   → amount  │  │ - delegate  │  │
│  │  │ - name      │  │ - grant     │  │ - limits    │  │
│  │  │ - url       │  │ - revoke    │  │ - spent     │  │
│  │  │ - perms     │  │ - deduct    │  │ - counters  │  │
│  │  │ - reverse   │  │ - credit    │  │ - whitelists│  │
│  │  │   (addr→id) │  │             │  │             │  │
│  │  └─────────────┘  └─────────────┘  └─────────────┘  │
│  │                                                      │  │
│  │  Reads / writes EVM storage slots under AGENT_ADDRESS │  │
│  │  (0x209) via StorageRef                               │  │
│  └──────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Agent Storage (`lib.rs`, `precompile.rs`)

`AgentStorage<B: StorageBackend>` is the single struct that manages all agent and session state. It reads and writes EVM storage slots under the agent precompile address (`0x209`) using `StorageRef`.

#### Agent operations

| Method | Caller | Description |
|--------|--------|-------------|
| `register_agent(name, url, agent_address, caller, block)` | owner | Registers a new agent with auto-incremented `agent_id` |
| `grant_balance(asset_store, agent_id, asset_id, amount, caller)` | owner | Deducts from owner's **protocol balance**, credits agent sub-account |
| `revoke_balance(asset_store, agent_id, asset_id, caller)` | owner | Revokes all agent balance for an asset, returns to owner's **protocol balance** |
| `pay(asset_store, asset_id, to, amount, caller, block)` | **agent_address** | Agent pays from its balance to recipient via reverse index lookup |
| `batch_pay(asset_store, asset_id, recipients, amounts, caller, block)` | **agent_address** | Batch payment; array lengths must match |
| `revoke_agent(agent_id, caller)` | owner | Deregisters agent, clears reverse index and metadata |

All operations are caller-authenticated via `msg.sender`. `pay` and `batchPay` look up the agent via the reverse index (`slot_agent_by_address`) from `msg.sender` and can only be called by the registered `agent_address`, not the owner.

#### View operations

| Method | Returns |
|--------|---------|
| `read_count()` | Total registered agents |
| `read_owner(agent_id)` | Owner address |
| `read_agent_address(agent_id)` | Registered agent operating address |
| `find_agent_by_address(addr)` | Agent ID for a given address (or `u64::MAX`) |
| `read_name(agent_id)` | `bytes32` name (left-aligned string) |
| `read_url(agent_id)` | `bytes32` url (left-aligned string) |
| `read_perms(agent_id)` | Packed `U256` permissions |
| `read_registered_at(agent_id)` | Registration block number |
| `read_agent_balance(agent_id, asset_id)` | Agent balance for asset |
| `agent_exists(agent_id)` | `bool` |

### 2. Agent Registration

Stored fields per agent:
- `agent_id`: auto-incremented unique ID (starting at 0)
- `owner`: Address that controls the agent
- `agent_address`: EOA address authorized to perform `pay` and `batchPay`
- `name`, `url`: descriptive metadata (stored as `bytes32`, left-aligned)
- `registered_at`: Block number of registration
- `perms`: Packed `U256` containing per-tx limit, expiry, and flags

Registration enforces:
- `agent_address` must not be `Address::ZERO`
- `agent_address` uniqueness via reverse index (`slot_agent_by_address`)

Default permissions on registration:
- `per_tx_limit = 1_000`
- `expires_at = 0` (never)
- `flags = 1` (bit 0 set = allow CALL asset_id=1)

### 3. Agent Permissions

Agent permissions are packed into a single `U256`:

| Bytes | Field | Type | Default |
|-------|-------|------|---------|
| 0..16 | `per_tx_limit` | `u128` | `1_000` |
| 16..24 | `expires_at` | `u64` | `0` |
| 31 | `flags` | `u8` | `1` |

Flag semantics (bit 0): when set, asset_id=1 (CALL) is allowed. When clear, only asset_id=1 is blocked; other assets are always blocked by the current implementation.

`pay` and `batchPay` enforce:
1. `amount ≤ per_tx_limit`
2. `current_block ≤ expires_at` (if non-zero)
3. Asset allowed by flags

There is no `daily_limit`, `allowed_counterparties`, or per-precompile permission checking at the agent level. Those constraints exist only in **session keys**.

### 4. Agent Balances

Agent balances are stored as EVM storage slots under `0x209` at `slot_agent_balance(agent_id, asset_id)`.

- `grant_balance()`: Deducts from owner's **protocol balance** via `AssetStorage::deduct_balance`, credits agent with `checked_add` overflow protection
- `revoke_balance()`: Reads agent balance, zeros the slot, returns full amount to owner's **protocol balance** via `AssetStorage::add_balance`
- `pay()`: Subtracts from agent balance with `checked_sub` underflow check, credits recipient via `AssetStorage::add_balance`
- `batch_pay()`: Sums amounts, single underflow check, then iterates to credit recipients

**Important**: Agent balances are protocol-layer balances (Asset precompile slots), not native EVM balances.

---

### 5. Session Keys

Session keys allow any address (owner) to delegate limited, time-bound spending authority to an external address (delegate). The delegate can then initiate transfers from the **owner's protocol balance** without requiring the owner to sign every transaction. Sessions are independent of agents — they operate directly on the owner's funds.

#### Session Lifecycle

1. **Create**: Owner calls `createSession(delegate, perTxLimit, dailyLimit, expiresAt, maxTotalSpend, minIntervalBlocks, effectiveAt, maxExecutions, allowedAssets, allowedRecipients)` → returns `sessionId`
2. **Execute**: Delegate calls `executeSession(sessionId, assetId, to, amount)` → transfers from owner balance to recipient
3. **Revoke**: Owner calls `revokeSession(sessionId)` → immediately invalidates the session
4. **Auto-expire**: Sessions automatically become invalid after `expiresAt` block or before `effectiveAt`

#### Session Constraints

| Field | Type | Description | Optional |
|-------|------|-------------|----------|
| `delegate` | `address` | The EOA address authorized to execute on behalf of the owner | No |
| `perTxLimit` | `uint128` | Maximum amount per single `executeSession` | No |
| `dailyLimit` | `uint128` | Maximum cumulative amount per day (~17,280 blocks) | No |
| `expiresAt` | `uint64` | Block number after which the session is invalid (0 = never) | Yes |
| `maxTotalSpend` | `uint128` | Lifetime cumulative spending cap (0 = unlimited) | Yes |
| `minIntervalBlocks` | `uint64` | Minimum blocks between two executions (0 = no limit) | Yes |
| `effectiveAt` | `uint64` | Block number before which the session is inactive (0 = immediate) | Yes |
| `maxExecutions` | `uint64` | Maximum number of `executeSession` calls (0 = unlimited) | Yes |
| `allowedAssets` | `uint64[]` | Whitelist of permitted asset IDs (empty = any asset) | Yes |
| `allowedRecipients` | `address[]` | Whitelist of permitted recipient addresses (empty = any address) | Yes |

All optional constraints are disabled when set to `0` or an empty array.

`executeSession` validation chain (in order):
1. Session exists (owner != ZERO)
2. Caller is the registered delegate
3. `current_block ≤ expiresAt` (if non-zero)
4. `current_block ≥ effectiveAt` (if non-zero)
5. `current_block - last_execution_block ≥ minIntervalBlocks` (if non-zero)
6. `execution_count < maxExecutions` (if non-zero)
7. `amount ≤ perTxLimit`
8. `daily_spent + amount ≤ dailyLimit`
9. `total_spent + amount ≤ maxTotalSpend` (if non-zero)
10. `assetId` in `allowedAssets` (if non-empty)
11. `to` in `allowedRecipients` (if non-empty)

On success, counters are updated: `spent`, `last_day`, `execution_count`, `last_execution_block`, `total_spent`.

#### Session Storage Layout

Session data is stored under `AGENT_ADDRESS (0x209)` with globally unique session IDs:

```
slot_session_count()                              → uint64
slot_session_owner(session_id)                    → address
slot_session_delegate(session_id)                 → address
slot_session_limits(session_id)                   → packed(perTxLimit, dailyLimit)
slot_session_expires(session_id)                  → uint64
slot_session_spent(session_id)                    → uint128
slot_session_last_day(session_id)                 → uint64
slot_session_allowed_assets_count(session_id)     → uint64
slot_session_allowed_asset(session_id, i)         → uint64
slot_session_allowed_recipients_count(session_id) → uint64
slot_session_allowed_recipient(session_id, i)     → address
slot_session_max_total_spend(session_id)          → uint128
slot_session_min_interval_blocks(session_id)      → uint64
slot_session_effective_at(session_id)             → uint64
slot_session_max_executions(session_id)           → uint64
slot_session_execution_count(session_id)          → uint64
slot_session_last_execution_block(session_id)     → uint64
slot_session_total_spent(session_id)              → uint128
```

#### Gas

| Method | Base Gas |
|--------|----------|
| `createSession` | 10,000 |
| `revokeSession` | 6,000 |
| `isSessionValid` | 2,000 |
| `executeSession` | 30,000 |

Note: Gas is paid by the delegate (EVM `msg.sender`) when calling `executeSession`. The transferred amount is deducted from the owner's protocol balance, not the delegate's.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `AgentStorage`, `AgentError`, `SessionPolicy`, storage slot helpers, permission packing/unpacking, session key logic |
| `precompile.rs` | `AgentPrecompile`, ABI dispatch for `0x209`, selector decoding, EVM storage integration, precompile tests |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Agent registration | 🟢 Ready | `agent_address` uniqueness via reverse index, metadata storage, agent revocation |
| Agent permissions | 🟡 Partial | Basic per-tx limit (1,000 default), asset allowlist (bit-flag), expiry. No daily limit or counterparties at agent level |
| Session keys | 🟢 Ready | Create/revoke/validate/execute with 10 optional/permanent constraints |
| Balance management | 🟢 Ready | Grant deducts from owner protocol balance, overflow-protected credit, underflow-protected deduct |
| Payments | 🟢 Ready | Single and batch pay from agent balance, session-key delegated transfers from owner balance |
| Persistence | 🟢 Ready | All state in EVM storage under `0x209`, committed with EVM state root |

---

## Test Status

- `cargo test -p call-agent` — unit tests covering:
  - Registration, owner checks, permission packing/unpacking
  - Balance operations (grant, pay, revoke, batch pay)
  - Overflow/underflow protection, array mismatch, empty batch
  - Agent revocation, not-found errors
  - Session lifecycle (create, revoke, validity)
  - Session constraint enforcement: per-tx limit, daily limit, expiry, max total spend, min interval blocks, effective at, max executions, allowed assets, allowed recipients, invalid delegate
  - Precompile ABI encode/decode and end-to-end calls
