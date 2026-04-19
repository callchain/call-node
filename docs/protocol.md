# Callchain Protocol Payment Layer

## Overview

The Protocol Payment Layer (`crates/protocol`) is Callchain's native transaction execution engine. It handles all protocol-level operations — asset transfers, staking, shielded transactions, oracle submissions, bridge operations, and compliance enforcement — independently from the EVM smart contract layer.

Every block contains a mix of protocol transactions (`ProtocolTransaction`) and EVM transactions. Protocol transactions are validated, executed, and committed atomically within the block execution pipeline.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│  ProtocolTransaction                                        │
│  ├── sender, nonce, instructions[], gas_config, auth        │
│  └── fee_currency, gas_limit, max_fee                       │
│                                                             │
│  Instruction::Transfer ───────────────┐                    │
│  Instruction::BatchTransfer ──────────┤                    │
│  Instruction::Approve / TransferFrom ─┤  → execute_instruction() │
│  Instruction::Mint / Burn ────────────┤  → atomic rollback       │
│  Instruction::Shielded* ──────────────┤                    │
│  Instruction::BridgeDeposit ──────────┤                    │
│  Instruction::OracleSubmit ───────────┤                    │
│  Instruction::Agent* ─────────────────┤                    │
│  Instruction::UpdateCompliance ───────┘                    │
│                                                             │
│  ┌──────────────┐  ┌──────────┐  ┌──────────────┐        │
│  │ BalanceState │  │AssetRegistry│ │ComplianceEngine│       │
│  │  (balances + │  │ (asset     │  │ (blacklist,   │       │
│  │   allowances)│  │  metadata) │  │  KYC, custom) │       │
│  └──────────────┘  └──────────┘  └──────────────┘        │
└─────────────────────────────────────────────────────────────┘
```

---

## Key Components

### 1. Instruction Set (`instructions.rs`)

| Instruction | Action | Authorization |
|-------------|--------|---------------|
| `Transfer` | Asset transfer with optional memo | Sender balance |
| `BatchTransfer` | Multi-recipient transfer | Sender balance |
| `Approve` | Set spending allowance | Sender |
| `TransferFrom` | Spend allowance + transfer | Allowance holder |
| `Mint` | Create new tokens | Asset issuer only |
| `Burn` | Destroy tokens | Asset issuer only |
| `AgentPay` / `AgentBatchPay` | Agent-mediated payment | Sender |
| `AgentCall` | External contract via agent | No-op (EVM layer) |
| `AgentBridgeDeposit` | Bridge via agent | No-op (bridge layer) |
| `BridgeDeposit` | Cross-chain deposit | Bridge proof validation |
| `ShieldedDeposit` | Transparent → shielded | Balance deduction |
| `ShieldedWithdraw` | Shielded → transparent | ZK proof + nullifier |
| `ShieldedTransfer` | Shielded → shielded | ZK proof |
| `UpdateCompliance` | Set address compliance | Asset issuer only |
| `OracleSubmit` | Price feed submission | Registered validator |

**Atomicity:** `execute_protocol_instructions()` takes a `balances.clone()` snapshot before execution. If any instruction fails, the entire transaction is rolled back by restoring the snapshot. Note: this only rolls back `BalanceState`, not `ComplianceEngine`, `ShieldedState`, or `AssetRegistry` changes made during execution.

**Gap #1 — Incomplete atomic rollback:** The snapshot/rollback mechanism only covers `BalanceState`. Changes to `ComplianceEngine` (e.g., compliance status updates), `ShieldedState` (nullifier spends, Merkle tree updates), and `AssetRegistry` (supply tracking) are not rolled back on failure. A multi-instruction transaction that fails mid-way could leave compliance or shielded state in a partially committed state.

### 2. Transaction Model (`transaction.rs`)

`ProtocolTransaction` fields:
- `sender: Address` — transaction originator
- `nonce: u64` — sequence number for replay protection
- `instructions: Vec<Instruction>` — ordered instruction list
- `gas_config: GasConfig` — fee payment strategy
- `fee_currency: FeeCurrency` — CALL (asset_id=0) or stablecoin
- `gas_limit: u64` — max gas units
- `max_fee: u128` — max fee willing to pay
- `auth: AuthScheme` — signature(s)

**Auth schemes:**
- `SingleSig { signature: [u8; 65] }` — ECDSA secp256k1 recovery signature
- `MultiSig { signatures: Vec<[u8; 65]> }` — M-of-N multisig
- `SessionKey { key: Address, signature: [u8; 65] }` — delegated session key

**Gap #2 — Signature verification is NOT performed in `execute_protocol_instructions`:** The doc comment says "verify_auth (done by caller)", but neither `bft_event_loop::propose` nor `block_production_loop` calls any signature verification function before executing instructions. `AuthScheme` contains raw signature bytes but there is no `verify()` method or `ecrecover` call. Transactions are accepted based solely on nonce uniqueness and balance sufficiency.

**Gap #3 — Nonce management is ad-hoc:** The mempool deduplicates by `(sender, nonce)` pair, but there is no enforcement of sequential nonce ordering. A transaction with nonce=5 can be included before nonce=3. The `accept_to_mempool` function checks only uniqueness, not sequentiality.

### 3. Gas & Fee Model (`transaction.rs`)

**Gas unit table (per instruction):**

| Instruction | Base Gas |
|-------------|----------|
| Transfer (with memo) | 10,000 + memo bytes |
| BatchTransfer | 1,000 per payment + memo bytes |
| Approve / Mint / Burn | 5,000 |
| TransferFrom | 10,000 |
| BridgeDeposit | 10,000 |
| ShieldedDeposit / Withdraw | 20,000 |
| ShieldedTransfer | 50,000 |
| OracleSubmit | 50,000 |

**Multi-instruction discount:**
- 1st instruction: 1.0x
- 2nd–10th: 0.5x
- 11th+: 0.25x

**Fee calculation:** `fee = gas_units * (base_fee + priority_fee)`

**Base fee adjustment (EIP-1559-style):**
```
if gas_used > target:  increase = base_fee * (diff/target) * (1/8)
if gas_used < target:  decrease = base_fee * (|diff|/target) * (1/8)
```

**Fee allocation:**
- CALL fees: 50% burned, 50% validator reward, 100% priority fee to proposer
- Stablecoin fees: 50% treasury, 50% validator reward

**Gap #4 — `GasConfig` sponsors are not implemented:** `AuthorizedSponsor`, `PoolSponsor`, and `PerTxSponsor` variants exist in the enum but `deduct_gas()` returns `Ok(())` for all three. The actual sponsor logic (checking authorization, deducting from sponsor balance) is stubbed. Without sponsor implementation, any transaction using these gas configs executes for free.

**Gap #5 — Stablecoin fee deduction uses wrong asset ID:** In `deduct_stablecoin_from_payer`, the function receives `asset_id` as a parameter but always calls `balances.deduct_balance(0, sender, fee)` — deducting CALL (asset_id=0) instead of the stablecoin. This means stablecoin-denominated fees are charged in CALL.

**Gap #6 — No priority fee enforcement:** `compute_fee()` calculates a priority component, but `max_fee` is compared against `gas_units * base_fee` only (priority is ignored in the mempool acceptance check). Senders can set arbitrarily high priority fees without penalty.

### 4. Balance State (`balances.rs`)

- `ProtocolBalances`: `HashMap<(AssetId, Address), Balance>`
- `Allowances`: `HashMap<(AssetId, Owner, Spender), Balance>`

Operations use `checked_add`/`checked_sub` for overflow/underflow protection. All balance mutations go through `BalanceState` methods.

**Production ready:** Yes. The balance layer is straightforward HashMap wrappers with proper checked arithmetic. No obvious gaps.

### 5. Asset Registry (`registry.rs`)

- `AssetRegistry`: `HashMap<AssetId, Asset>` with symbol uniqueness enforcement
- Registration fee: 10 CALL (checked at transaction level, not in registry)
- Asset status: Active / Frozen / Delisted
- Issuer-only operations: freeze, delist, mint supply, transfer ownership, update compliance policy

**Gap #7 — `registered_at` is hardcoded to 0:** The `Asset::registered_at` field is set to `0` during registration. The comment says "set by caller with current block" but no caller actually sets it.

**Gap #8 — No total supply tracking in registry:** `mint()` validates issuer but `Asset::total_supply` is only updated via `mint_supply()` which is never called from the instruction execution path. The `Mint` instruction calls `balances.mint()` (which updates balances) but not `registry.mint_supply()` (which updates total_supply). Total supply is effectively untracked.

### 6. Compliance Engine (`compliance.rs`)

Policies per asset:
- `None` — no checks
- `OfacBlacklist` — address-level blacklist
- `KycRequired` — KYC verification required
- `Whitelist` — whitelist-only access
- `Custom` — delegate to registered handler trait

Per-address compliance status (Clear / UnderReview / Flagged / Restricted) is stored under `(Address, policy_id)`.

**Gap #9 — Compliance check is sender-only, not recipient-side:** `Transfer` checks `compliance.check_compliance_by_policy_id(&sender, ...)` but does not check the recipient's compliance status. A sanctioned address can still receive funds.

**Gap #10 — `Custom` policy always passes:** The `CompliancePolicy::Custom` branch in `check_compliance()` returns `Ok(())` unconditionally. The `check_custom()` method exists but is never called during instruction execution. Custom compliance handlers are registered but never invoked.

**Gap #11 — Compliance engine is not persisted to DB:** `ComplianceEngine` is held in `RpcState` in memory. There is no DB schema for sanctioned addresses, KYC status, or per-address compliance states. On node restart, all compliance data is lost.

---

## File Map

| File | Role |
|------|------|
| `lib.rs` | Crate root, `ProtocolError` enum |
| `instructions.rs` | `Instruction` enum, execution engine, atomic rollback |
| `transaction.rs` | `ProtocolTransaction`, gas/fee calculation, mempool acceptance |
| `balances.rs` | `BalanceState`, `ProtocolBalances`, `Allowances` |
| `registry.rs` | `AssetRegistry`, `Asset` metadata |
| `compliance.rs` | `ComplianceEngine`, policies, blacklist/KYC/whitelist |
| `smart_accounts.rs` | Smart account registry, session keys |
| `security.rs` | Access control, permission levels |
| `sponsor.rs` | Gas sponsor authorization |
| `receipts.rs` | Transaction receipts |
| `issuer.rs` | Issuer management |
| `economics.rs` | Economic model helpers |
| `fee_currency.rs` | Fee currency registry |

---

## Production Readiness Assessment

| Component | Status | Notes |
|-----------|--------|-------|
| Instruction execution | 🟡 Partial | Atomic rollback incomplete, signature verification missing |
| Balance management | 🟢 Ready | Checked arithmetic, no known gaps |
| Asset registry | 🟡 Partial | `registered_at` bug, total supply untracked |
| Gas/fee model | 🟡 Partial | Sponsor unimplemented, stablecoin fee bug, no priority enforcement |
| Compliance engine | 🔴 Not ready | Recipient checks missing, custom handlers unused, no persistence |
| Transaction validation | 🔴 Not ready | No signature verification, no sequential nonce enforcement |

---

## Production Readiness Gaps

| # | Gap | Severity | Details |
|---|-----|----------|---------|
| 1 | **Incomplete atomic rollback** | High | `execute_protocol_instructions` rolls back `BalanceState` only. `ComplianceEngine`, `ShieldedState`, and `AssetRegistry` mutations persist even on failure. |
| 2 | **No signature verification** | Critical | `AuthScheme` contains raw signature bytes but no verification is performed before instruction execution. Anyone can submit transactions from any address. |
| 3 | **No sequential nonce enforcement** | High | Mempool checks nonce uniqueness only, not ordering. Gaps in nonce sequence are allowed. |
| 4 | **Gas sponsor unimplemented** | High | `AuthorizedSponsor`, `PoolSponsor`, `PerTxSponsor` all return `Ok(())` in `deduct_gas()`. These transactions execute for free. |
| 5 | **Stablecoin fee deducts CALL instead** | High | `deduct_stablecoin_from_payer` ignores the `asset_id` parameter and always deducts asset_id=0 (CALL). |
| 6 | **No priority fee enforcement** | Medium | `max_fee` check ignores priority fee component. No economic disincentive for spam with high priority. |
| 7 | **`registered_at` hardcoded to 0** | Low | Asset registration timestamp is always 0. |
| 8 | **Total supply untracked** | Medium | `Mint` instruction updates balances but not `Asset::total_supply`. Total supply queries will be incorrect. |
| 9 | **Recipient compliance not checked** | High | Transfer only checks sender compliance. Sanctioned addresses can receive funds. |
| 10 | **Custom compliance handlers unused** | Medium | `CompliancePolicy::Custom` always passes. Registered handlers are never invoked. |
| 11 | **Compliance state not persisted** | High | All compliance data (blacklist, KYC, per-address status) is in-memory only. Lost on restart. |
| 12 | **No instruction count limit** | Medium | A single transaction can contain an unbounded number of instructions. Potential DoS vector. |
| 13 | **Memo size limits not enforced at deserialization** | Low | `PaymentMemo::validate()` exists but is only called during execution, not during mempool acceptance. Large memos can bloat mempool. |
| 14 | **No replay protection for multi-sig** | Medium | `MultiSig` accepts `Vec<[u8; 65]>` but there is no M-of-N threshold validation. A single signature is sufficient regardless of config. |

---

## Test Status

- `cargo test -p call-protocol` — unit tests cover gas calculation, fee dynamics, instruction execution, balance operations, asset registry, compliance policies, memo validation, atomic rollback
- Missing: signature verification tests, nonce sequencing tests, sponsor tests, compliance persistence tests, recipient-side compliance tests
