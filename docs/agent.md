# Callchain Agent Payment Layer

## Overview

The Agent Layer (`crates/agent`) enables delegated transaction execution on behalf of users. An agent is a registered entity (e.g., a dApp, service, or automated wallet) that can sign and execute protocol transactions within owner-defined constraints.

**Key features:**
- Agent registration with optional domain verification
- Per-agent permissions (asset whitelist, counterparty restrictions, daily/tx limits)
- Agent-specific balance management (owner-funded sub-accounts)
- Agent transaction verification with dual-signature support (agent + owner)
- 0.5x gas discount for agent-mediated transactions

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  Agent Payment Layer                                         │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ AgentRegistry      │  │ AgentBalances                │  │
│  │ - agents: id→reg   │  │ - (owner, id, asset)→amount │  │
│  │ - by_owner         │  │ - grant / revoke / deduct   │  │
│  │ - by_name          │  │                              │  │
│  └────────────────────┘  └──────────────────────────────┘  │
│                                                             │
│  ┌────────────────────┐  ┌──────────────────────────────┐  │
│  │ AgentPermissions   │  │ AgentNonces                  │  │
│  │ - allowed_assets   │  │ - (owner, id)→nonce          │  │
│  │ - daily_limit      │  │ - check_and_increment        │  │
│  │ - per_tx_limit     │  └──────────────────────────────┘  │
│  │ - allowed_counter- │                                     │
│  │   parties          │  ┌──────────────────────────────┐  │
│  │ - expires_at       │  │ AgentTxContext               │  │
│  └────────────────────┘  │ - agent_id, nonce, sig       │  │
│                          │ - owner_public_key             │  │
│  ┌────────────────────┐  └──────────────────────────────┘  │
│  │ AgentFeeConfig     │  ┌──────────────────────────────┐  │
│  │ - fee_payer        │  │ AgentEvents                  │  │
│  │ - require_owner_   │  │ - event_type, agent_id       │  │
│  │   signature_above  │  │ - asset_id, amount           │  │
│  └────────────────────┘  │ - block_height               │  │
│                          └──────────────────────────────┘  │
│                          verify_agent_tx (5-step)          │
│                          execute_agent_tx (0.5x gas)       │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Agent Registration (`registry.rs`)

`AgentRegistration` fields:
- `agent_id`: auto-incremented unique ID
- `owner`: Address that controls the agent
- `agent_public_key`: secp256k1 public key for agent signatures
- `name`, `url`, `metadata_hash`: descriptive metadata
- `domain_proof`: Optional DNS TXT or HTTP file verification
- `domain_verified`: Whether domain proof passed validation
- `registered_at`: Block number of registration

`AgentRegistry` supports:
- Register by owner with name uniqueness enforcement
- Lookup by ID, name, or owner
- Update config (name, URL, metadata)
- Update domain proof

**Gap #1 — Domain verification is format-only:** ~~`verify_domain_proof()` and `DefaultDomainVerifier` only check that the domain/URL string is well-formed.~~ **FIXED** — `AgentRegistry::new()` now uses `RealDomainVerifier` by default, which performs actual DNS TXT lookups (`hickory_resolver`) and HTTP fetches (`ureq`). `new_with_format_verifier()` is available for testing.

**Gap #2 — No registration fee or stake requirement:** ~~Anyone can register an agent at zero cost. There is no economic barrier to agent spam.~~ **FIXED** — `AgentRegistry` now supports `registration_fee` and `fee_asset_id`. `register_agent()` accepts an optional `balances: &mut BalanceState` and deducts the fee before creating the registration. `with_registration_fee(fee, asset_id)` builder is available.

**Gap #3 — No agent revocation/removal:** ~~Once registered, an agent cannot be removed from the registry.~~ **FIXED** — `AgentRegistry::unregister_agent(agent_id)` removes the agent from all indexes (`agents`, `agents_by_name`, `agents_by_owner`).

### 2. Agent Permissions (`permissions.rs`)

`AgentPermissions` controls what an agent can do:

| Field | Default | Behavior |
|-------|---------|----------|
| `allowed_assets` | `[1]` (only CALL) | Asset whitelist |
| `daily_limit` | `10_000` | Daily cumulative amount |
| `per_tx_limit` | `1_000` | Per-transaction cap |
| `allowed_counterparties` | `[]` (empty = all) | Recipient whitelist |
| `allowed_protocols` | `[]` (empty = none) | EVM contract whitelist |
| `expires_at` | `0` (never) | Permission expiry block |

`verify_agent_permissions()` checks:
1. Expiration
2. Asset allowed
3. Counterparty allowed
4. Per-tx limit
5. Daily limit (with auto-reset every 86,400 blocks)
6. Owner daily fee limit

**Gap #4 — Default permissions are wide open:** ~~`AgentPermissions::default()` sets `allowed_assets = []` (meaning ALL assets allowed), `daily_limit = MAX`, `per_tx_limit = MAX`.~~ **FIXED** — Defaults are now restrictive: `allowed_assets = [1]` (only CALL), `daily_limit = 10_000`, `per_tx_limit = 1_000`.

**Gap #5 — Daily reset is block-based, not time-based:** ~~`BLOCKS_PER_DAY = 86400` assumes 250ms block times. If block times change, the "day" duration changes.~~ **FIXED** — `AgentDailyUsage` now uses `last_reset_time: u64` (ms) and `MS_PER_DAY = 86400000`. `verify_agent_permissions()` takes both `current_block` (for expiry) and `current_time` (for daily reset).

**Gap #6 — BatchTransfer only checks first payment:** ~~`extract_instruction_details()` for `BatchTransfer` returns only the first payment's details.~~ **FIXED** — `extract_instruction_details()` now returns `Vec<(AssetId, Address, u128)>`. `BatchTransfer` and `AgentBatchPay` enumerate **all** payments, and `verify_agent_tx()` iterates over every entry.

### 3. Agent Balances (`balances.rs`)

`AgentBalances`: `HashMap<(owner, agent_id, asset_id), u128>`

- `grant_funds()`: Deducts from owner's protocol balance, then credits agent
- `top_up()`: Same as grant (semantic alias)
- `revoke_funds()`: Removes all balance for an agent, returns amount
- `deduct()`: Subtracts with underflow check
- `credit()`: Adds with `checked_add` overflow protection

**Gap #7 — Grant does not deduct from owner:** ~~`grant_funds()` simply credits the agent balance. The owner's protocol balance is not reduced.~~ **FIXED** — `grant_funds()` now calls `protocol_balances.deduct_balance()` before crediting the agent.

**Gap #8 — No overflow protection on credit:** ~~`credit()` uses `balance + amount` without `checked_add`.~~ **FIXED** — `credit()` uses `checked_add` and returns `AgentError::ExecutionFailed("agent balance overflow")` on overflow.

### 4. Agent Transaction Verification (`executor.rs`)

`verify_agent_tx()` performs 5-step validation:
1. Agent signature verification (secp256k1)
2. Nonce check (sequential, no gaps)
3. Per-instruction permission checks (all payments in a batch)
4. Expiry check (uses `protocol_tx.expires_at` as block deadline)
5. Owner signature threshold for large amounts

`execute_agent_tx()`:
1. Calculates gas with 0.5x discount
2. Deducts fee from agent balance using the transaction's `fee_currency`
3. Executes instructions via `execute_protocol_instructions()`

**Gap #9 — Gas fee asset is hardcoded to asset_id=1:** ~~`execute_agent_tx()` always deducts gas fees using asset_id=1.~~ **FIXED** — Fee asset is resolved from `protocol_tx.fee_currency`: `FeeCurrency::Call` → asset_id=1, `FeeCurrency::Stablecoin(id)` → asset_id=id.

**Gap #10 — `max_fee` used as expiry proxy:** ~~The expiry check uses `protocol_tx.max_fee as u64` as the block number proxy. This conflates fee economics with time validity. A high `max_fee` means a long expiry.~~ **FIXED** — `ProtocolTransaction` now has independent `expires_at: u64` field. `verify_agent_tx()` checks `protocol_tx.expires_at` instead of `max_fee`. `Block::execute` rejects expired txs at block boundary. `compute_tx_hash()` and `compute_agent_tx_hash()` include `expires_at` in preimage.

**Gap #11 — `execute_agent_call` is broken:** ~~It calls `evm_executor.evm_call_bridge_mint()` with the target address.~~ **FIXED** — `execute_agent_call()` now constructs a proper `EvmTransaction` with the target address and data, and executes it via `evm_executor.execute_tx()`.

**Gap #12 — Agent transactions are not integrated into block production:** ~~There is no `Instruction::Agent*` execution path in the block execution pipeline.~~ **FIXED** — `Block::execute` already had `execute_agent_instruction()` for `AgentPay`, `AgentBatchPay`, `AgentCall`, and `AgentBridgeDeposit`. Added `verify_agent_instruction_permissions()` which checks `allowed_assets`, `per_tx_limit`, `expires_at`, and `allowed_protocols` (for `AgentCall`) inline during block execution.

**Gap #13 — Agent state is not persisted:** ~~`AgentBalances`, `AgentNonces`, and `AgentRegistry` are in-memory only.~~ **FIXED** — `AgentRegistry` and `AgentBalances` are persisted to MDBX (`CallAgents` / `CallAgentBalances` tables). `AgentNonces` is now also persisted (`CallAgentNonces` table) via `save_agent_nonces_inner` / `load_agent_nonces_inner` in the node's persistence loop.

### 5. Agent Activity Audit Trail (`lib.rs`, `block.rs`)

`AgentEventType` enum:
- `AgentPay`, `AgentBatchPay`, `AgentCall`, `AgentBridgeDeposit`
- `AgentRegistered`, `AgentRevoked`

`AgentEvent` struct:
- `event_type`, `agent_id`, `tx_hash`, `asset_id`, `amount`, `recipient`, `block_height`

Emitted during `Block::execute` by `execute_agent_instruction()` for every successful agent instruction. Included in `BlockExecutionResult::agent_events` and hashed into `compute_receipt_root()`.

**Gap #14 — No agent activity audit trail:** ~~No receipts or events track agent-mediated transactions distinctly.~~ **FIXED** — `AgentEvent` / `AgentEventType` types added to `call-agent`. `BlockExecutionResult` carries `agent_events: Vec<AgentEvent>`. `execute_agent_instruction()` emits events for `AgentPay`, `AgentBatchPay`, `AgentCall`, and `AgentBridgeDeposit`. `compute_receipt_root()` hashes agent events into the receipt root.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `AgentError`, `AgentFeeConfig`, `FeePayer`, `SignedAgentTx`, `DomainProof`, `AgentEvent`, `AgentEventType` |
| `registry.rs` | `AgentRegistration`, `AgentRegistry`, domain proof handling |
| `permissions.rs` | `AgentPermissions`, `AgentDailyUsage`, permission verification |
| `balances.rs` | `AgentBalances`, `AgentNonces` |
| `executor.rs` | `verify_agent_tx()`, `execute_agent_tx()`, instruction helpers |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Agent registration | 🟢 Ready | Name uniqueness, metadata storage, real DNS/HTTP domain verification, agent revocation |
| Permissions | 🟢 Ready | Restrictive defaults, full per-instruction checking including all batch payments |
| Balance management | 🟢 Ready | Grant deducts from owner, overflow-protected credit, underflow-protected deduct |
| Transaction verification | 🟢 Ready | 5-step validation with independent `expires_at` field, no longer conflates fee with time |
| Transaction execution | 🟢 Ready | Proper EVM call execution, wired into block production with inline permission checks, fee_currency-aware gas deduction |
| Persistence | 🟢 Ready | AgentRegistry, AgentBalances, and AgentNonces all persisted to MDBX |

---

## Production Readiness Gaps

| # | Gap | Severity | Status | Details |
|---|-----|----------|--------|---------|
| 1 | **Domain verification is format-only** | High | ✅ Fixed | `RealDomainVerifier` performs actual DNS TXT and HTTP lookups. `new_with_format_verifier()` for tests. |
| 2 | **No registration fee or stake** | Medium | ✅ Fixed | `AgentRegistry::register_agent()` deducts `registration_fee` from `BalanceState` when configured via `with_registration_fee()`. |
| 3 | **No agent revocation/removal** | Medium | ✅ Fixed | `AgentRegistry::unregister_agent()` removes from all indexes. |
| 4 | **Default permissions are wide open** | High | ✅ Fixed | Defaults now: `allowed_assets = [1]`, `daily_limit = 10_000`, `per_tx_limit = 1_000`. |
| 5 | **Daily reset is block-based** | Low | ✅ Fixed | `AgentDailyUsage` uses `last_reset_time` (ms) and `MS_PER_DAY = 86400000`. `verify_agent_permissions()` takes `current_time` for daily reset. |
| 6 | **BatchTransfer only checks first payment** | High | ✅ Fixed | `extract_instruction_details()` returns `Vec`; all payments are permission-checked. |
| 7 | **Grant does not deduct from owner** | Critical | ✅ Fixed | `grant_funds()` now deducts from `protocol_balances` before crediting agent. |
| 8 | **No overflow protection on credit** | Medium | ✅ Fixed | `credit()` uses `checked_add`. |
| 9 | **Gas fee asset hardcoded to asset_id=1** | High | ✅ Fixed | Resolved from `protocol_tx.fee_currency` (`Call`→1, `Stablecoin(id)`→id). |
| 10 | **`max_fee` used as expiry proxy** | Medium | ✅ Fixed | `ProtocolTransaction` now has independent `expires_at: u64` field. `verify_agent_tx()` checks `protocol_tx.expires_at` instead of `max_fee`. `Block::execute` rejects expired txs at block boundary. `compute_tx_hash()` and `compute_agent_tx_hash()` include `expires_at` in preimage. |
| 11 | **`execute_agent_call` is broken** | High | ✅ Fixed | Now constructs a proper `EvmTransaction` and executes via `evm_executor.execute_tx()`. |
| 12 | **Not integrated into block production** | Critical | ✅ Fixed | `execute_agent_instruction()` in `Block::execute` with inline `verify_agent_instruction_permissions()`. |
| 13 | **Agent state not persisted** | High | ✅ Fixed | `AgentRegistry`, `AgentBalances`, and `AgentNonces` all persisted to MDBX. |
| 14 | **No agent activity audit trail** | Low | ✅ Fixed | `AgentEvent` / `AgentEventType` types added to `call-agent`. `BlockExecutionResult` carries `agent_events: Vec<AgentEvent>`. `execute_agent_instruction()` emits events for `AgentPay`, `AgentBatchPay`, `AgentCall`, and `AgentBridgeDeposit`. `compute_receipt_root()` hashes agent events into the receipt root. |

---

## Test Status

- `cargo test -p call-agent` — 46 unit tests covering registration (including fee deduction, insufficient balance rejection), domain proof format, balance operations (grant deducts from owner, overflow protection), nonce tracking, permission checks, instruction extraction (including batch transfer multi-payment), agent pay/batch pay, bridge deposit failure recovery, tx hash determinism
- `cargo test -p call-consensus` — block execution order test verifies `AgentPay` / `AgentBatchPay` / `AgentCall` / `AgentBridgeDeposit` execute correctly during `Block::execute`; `test_agent_instruction_emits_event` verifies `AgentEvent` emission and receipt root inclusion; `test_expired_transaction_rejected` verifies `expires_at` enforcement
- `cargo test -p call-node --lib` — node startup and state persistence tests verify agent registry, balances, and nonces are saved/loaded to MDBX correctly
- `cargo test -p call-protocol --test test_agent_flow` — integration tests covering registration, domain proof, balance operations, nonce sequential/stale rejection
