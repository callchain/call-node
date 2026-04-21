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

**Atomicity:** `execute_protocol_instructions()` snapshots `BalanceState`, `ComplianceEngine`, and `ShieldedState` before execution. If any instruction fails, all three are restored. `AssetRegistry` is passed as immutable reference and cannot be mutated during execution.

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

**Signature verification:** `ProtocolTransaction::verify_signature()` and `verify_signature_with_registry()` are fully implemented. Called in block execution (`block.rs`) and RPC handlers before instruction execution.

**Nonce sequencing:** `accept_to_mempool` enforces sequential nonces via `expected_nonces` HashMap. Transactions with nonce < expected are rejected.

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

**Instruction limit:** `MAX_INSTRUCTIONS_PER_TX = 100` enforced at mempool boundary.

### 4. Balance State (`balances.rs`)

- `ProtocolBalances`: `HashMap<(AssetId, Address), Balance>`
- `Allowances`: `HashMap<(AssetId, Owner, Spender), Balance>`

Operations use `checked_add`/`checked_sub` for overflow/underflow protection. All balance mutations go through `BalanceState` methods.

### 5. Asset Registry (`registry.rs`)

- `AssetRegistry`: `HashMap<AssetId, Asset>` with symbol uniqueness enforcement
- Registration fee: 10 CALL (checked at transaction level, not in registry)
- Asset status: Active / Frozen / Delisted
- Issuer-only operations: freeze, delist, mint supply, transfer ownership, update compliance policy

### 6. Compliance Engine (`compliance.rs`)

Policies per asset:
- `None` — no checks
- `OfacBlacklist` — address-level blacklist
- `KycRequired` — KYC verification required
- `Whitelist` — whitelist-only access
- `Custom` — delegate to registered handler trait

Per-address compliance status (Clear / UnderReview / Flagged / Restricted) is stored under `(Address, policy_id)`.

`CompliancePolicy::Custom` iterates all registered handlers and requires all to pass. `check_compliance()` checks sender and recipient for Transfer, and all three parties (sender, from, to) for TransferFrom.

**Persistence:** Compliance state is persisted to DB via `save_compliance_state` / `load_compliance_state` in the node lifecycle.

---

## Production Readiness Assessment

| Component | Status |
|-----------|--------|
| Instruction execution | 🟢 Ready | Atomic rollback covers BalanceState + ComplianceEngine + ShieldedState |
| Balance management | 🟢 Ready | Checked arithmetic, no known gaps |
| Asset registry | 🟡 Partial | `registered_at` bug, total supply untracked |
| Gas/fee model | 🟡 Partial | PoolSponsor stubbed, no priority enforcement |
| Compliance engine | 🟢 Ready | Recipient checks, custom handlers, persistence all wired |
| Transaction validation | 🟢 Ready | Signature verification, sequential nonces, instruction limits |

---

## Remaining Gaps

### Gap #4 (partial) — PoolSponsor not implemented
**Severity:** High

`GasConfig::PoolSponsor` returns `"PoolSponsor not yet implemented in production"` in both `deduct_call_from_payer` and `deduct_stablecoin_from_payer`. `AuthorizedSponsor` and `PerTxSponsor` are fully implemented via `SponsorRegistry`.

**How to fix:** Implement `PoolSponsor` logic in `crates/protocol/src/sponsor.rs`:
1. Add a pool balance tracking field to `SponsorRegistry` (or a separate `GasSponsorPool` struct)
2. Implement `verify_and_deduct_pool_sponsor(balances, tx, fee)` that checks pool has sufficient balance and deducts it
3. Wire into `deduct_call_from_payer` and `deduct_stablecoin_from_payer` for the `PoolSponsor` variant
4. Add pool deposit/withdraw methods for managing the pool balance
5. Add tests: pool with sufficient balance deducts correctly, insufficient balance rejects

### Gap #7 — `registered_at` hardcoded to 0
**Severity:** Low

In `crates/protocol/src/registry.rs` line 84, `Asset::registered_at` is set to `0`. The comment says "set by caller with current block" but the `register_asset` function has no block height parameter.

**How to fix:** Add a `current_block: u64` parameter to `register_asset` (or `register_asset_at`) and pass it from the caller. The caller is in `instructions.rs` where `RegisterAsset` is handled — pass the block number from the execution context, or alternatively set it at the block execution level after the transaction succeeds.

### Gap #8 — Total supply untracked
**Severity:** Medium

The `Mint` instruction calls `balances.mint()` (updates balances) but never calls `registry.mint_supply()` (updates `Asset::total_supply`). `mint_supply()` exists on `AssetRegistry` but is never invoked. Total supply queries will always show 0 for minted tokens.

**How to fix:** Wire `registry.mint_supply(asset_id, amount)` into the `Mint` instruction handler in `crates/protocol/src/instructions.rs`. The `execute_instruction` for `Mint` already receives `&mut AssetRegistry` — add the supply update alongside the balance mint call. Add a test verifying `registry.get_asset(asset_id).unwrap().total_supply` increases after a successful mint.

### Gap #6 — No priority fee enforcement
**Severity:** Medium

`compute_fee()` calculates a priority component, but `max_fee` is compared against `gas_units * base_fee` only (priority is ignored in the mempool acceptance check). Senders can set `max_fee` just above base fee but set a very high `priority_fee` implicitly — the actual cap check `gas_units * base_fee <= max_fee` doesn't account for priority.

**How to fix:** In `accept_to_mempool` (`crates/protocol/src/transaction.rs`), change the fee check from `gas_units * base_fee <= max_fee` to `gas_units * (base_fee + max_priority_fee) <= max_fee`, or cap priority at a protocol-defined maximum. Alternatively, add a `max_priority_fee` field to `ProtocolTransaction` and validate `priority_fee <= max_priority_fee`.

---

## Test Status

- `cargo test -p call-protocol` — ~105 unit tests covering gas calculation, fee dynamics, instruction execution, balance operations, asset registry, compliance policies, memo validation, atomic rollback, signature verification (positive + negative), proptest roundtrip encode/decode
- Missing: PoolSponsor tests, total supply verification after mint, registered_at correctness, priority fee enforcement
