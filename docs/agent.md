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
│  │ AgentFeeConfig     │                                     │
│  │ - fee_payer        │  verify_agent_tx (5-step)          │
│  │ - require_owner_   │  execute_agent_tx (0.5x gas)       │
│  │   signature_above  │                                     │
│  └────────────────────┘                                     │
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

**Gap #1 — Domain verification is format-only:** `verify_domain_proof()` and `DefaultDomainVerifier` only check that the domain/URL string is well-formed (non-empty, starts with `https://`). They do **not** make actual DNS queries or HTTP requests. Any domain can be "verified" by providing a syntactically valid proof.

**Gap #2 — No registration fee or stake requirement:** Anyone can register an agent at zero cost. There is no economic barrier to agent spam.

**Gap #3 — No agent revocation/removal:** Once registered, an agent cannot be removed from the registry. `agents` HashMap only grows. A compromised agent remains valid forever.

### 2. Agent Permissions (`permissions.rs`)

`AgentPermissions` controls what an agent can do:

| Field | Default | Behavior |
|-------|---------|----------|
| `allowed_assets` | `[]` (empty = all) | Asset whitelist |
| `daily_limit` | `u128::MAX` | Daily cumulative amount |
| `per_tx_limit` | `u128::MAX` | Per-transaction cap |
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

**Gap #4 — Default permissions are wide open:** `AgentPermissions::default()` sets `allowed_assets = []` (meaning ALL assets allowed), `daily_limit = MAX`, `per_tx_limit = MAX`. An agent with default permissions has unlimited scope.

**Gap #5 — Daily reset is block-based, not time-based:** `BLOCKS_PER_DAY = 86400` assumes 250ms block times. If block times change, the "day" duration changes.

**Gap #6 — BatchTransfer only checks first payment:** `extract_instruction_details()` for `BatchTransfer` returns only the first payment's details. The remaining payments in the batch are not permission-checked.

### 3. Agent Balances (`balances.rs`)

`AgentBalances`: `HashMap<(owner, agent_id, asset_id), u128>`

- `grant_funds()`: Owner credits agent balance (no deduction from owner balance)
- `top_up()`: Same as grant (semantic alias)
- `revoke_funds()`: Removes all balance for an agent, returns amount
- `deduct()`: Subtracts with underflow check

**Gap #7 — Grant does not deduct from owner:** `grant_funds()` simply credits the agent balance. The owner's protocol balance is not reduced. This means agents can be funded with "phantom" money.

**Gap #8 — No overflow protection on credit:** `credit()` uses `balance + amount` without `checked_add`. Overflow would wrap around.

### 4. Agent Transaction Verification (`executor.rs`)

`verify_agent_tx()` performs 5-step validation:
1. Agent signature verification (secp256k1)
2. Nonce check (sequential, no gaps)
3. Per-instruction permission checks
4. Expiry check (uses `max_fee` as expiry proxy)
5. Owner signature threshold for large amounts

`execute_agent_tx()`:
1. Calculates gas with 0.5x discount
2. Deducts fee from agent balance (asset_id = 1, hardcoded)
3. Executes instructions via `execute_protocol_instructions()`

**Gap #9 — Gas fee asset is hardcoded to asset_id=1:** `execute_agent_tx()` always deducts gas fees using asset_id=1, regardless of what the transaction actually uses or what the fee currency is.

**Gap #10 — `max_fee` used as expiry proxy:** The expiry check uses `protocol_tx.max_fee as u64` as the block number proxy. This conflates fee economics with time validity. A high `max_fee` means a long expiry.

**Gap #11 — `execute_agent_call` is broken:** It calls `evm_executor.evm_call_bridge_mint()` with the target address, which is a bridge-specific function repurposed as a general EVM call proxy. This will not execute arbitrary contract calls correctly.

**Gap #12 — Agent transactions are not integrated into block production:** There is no `Instruction::Agent*` execution path in the block execution pipeline. Agent transactions exist as a library but are not wired into consensus.

**Gap #13 — Agent state is not persisted:** `AgentBalances`, `AgentNonces`, and `AgentRegistry` are in-memory only. On node restart, all agent state is lost.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | `AgentError`, `AgentFeeConfig`, `FeePayer`, `SignedAgentTx`, `DomainProof` |
| `registry.rs` | `AgentRegistration`, `AgentRegistry`, domain proof handling |
| `permissions.rs` | `AgentPermissions`, `AgentDailyUsage`, permission verification |
| `balances.rs` | `AgentBalances`, `AgentNonces` |
| `executor.rs` | `verify_agent_tx()`, `execute_agent_tx()`, instruction helpers |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Agent registration | 🟡 Partial | Name uniqueness, metadata storage work, but domain verification is fake |
| Permissions | 🟡 Partial | Checks exist but defaults are wide open |
| Balance management | 🟡 Partial | HashMap wrappers, but grant doesn't deduct from owner, no overflow check |
| Transaction verification | 🟡 Partial | 5-step validation present, but expiry uses max_fee proxy |
| Transaction execution | 🔴 Not ready | AgentCall is broken, not wired to block production, gas asset hardcoded |
| Persistence | 🔴 Not ready | All agent state is in-memory only |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **Domain verification is format-only** | High | No actual DNS or HTTP verification. Any domain can be "verified." |
| 2 | **No registration fee or stake** | Medium | Zero-cost agent creation enables spam. |
| 3 | **No agent revocation/removal** | Medium | Compromised agents cannot be deregistered. Registry grows forever. |
| 4 | **Default permissions are wide open** | High | Empty `allowed_assets` means all assets allowed. `MAX` limits. |
| 5 | **Daily reset is block-based** | Low | 86,400 blocks assumes 250ms block time. Not robust to timing changes. |
| 6 | **BatchTransfer only checks first payment** | High | Subsequent payments in a batch bypass permission checks. |
| 7 | **Grant does not deduct from owner** | Critical | Agent balances can be created without owner funds being reduced. |
| 8 | **No overflow protection on credit** | Medium | `credit()` uses naive addition. Could wrap on overflow. |
| 9 | **Gas fee asset hardcoded to asset_id=1** | High | Fee deduction always uses asset_id=1, ignoring actual fee currency. |
| 10 | **`max_fee` used as expiry proxy** | Medium | Expiry logic conflates fee with time. Unexpected behavior. |
| 11 | **`execute_agent_call` is broken** | High | Calls bridge mint function instead of general EVM call. |
| 12 | **Not integrated into block production** | Critical | Agent instructions exist but are not executed during block validation. |
| 13 | **Agent state not persisted** | High | All balances, nonces, and registrations are lost on restart. |
| 14 | **No agent activity audit trail** | Low | No receipts or events track agent-mediated transactions distinctly. |

---

## Test Status

- `cargo test -p call-agent` — unit tests cover registration, domain proof format, balance operations, nonce tracking, permission checks, instruction extraction, agent pay/batch pay, bridge deposit failure recovery, tx hash determinism
- Missing: actual DNS/HTTP domain verification tests, batch transfer multi-payment permission tests, overflow tests, integration with block production tests, persistence tests
