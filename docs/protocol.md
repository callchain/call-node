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

**Atomicity:** `execute_protocol_instructions()` snapshots `BalanceState`, `ComplianceEngine`, and `ShieldedState` before execution. If any instruction fails, all three are restored. `AssetRegistry` is passed as mutable reference and can be mutated during execution (e.g., supply tracking on Mint).

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
| Asset registry | 🟢 Ready | `registered_at` set by caller, total supply tracked on mint |
| Gas/fee model | 🟢 Ready | PoolSponsor implemented, all sponsor variants wired |
| Compliance engine | 🟢 Ready | Recipient checks, custom handlers, persistence all wired |
| Transaction validation | 🟢 Ready | Signature verification, sequential nonces, instruction limits |

---

## Remaining Gaps

### Gap #6 — No priority fee enforcement
**Severity:** Medium

`compute_fee()` calculates a priority component, but `max_fee` is compared against `gas_units * base_fee` only (priority is ignored in the mempool acceptance check). Senders can set `max_fee` just above base fee but set a very high `priority_fee` implicitly — the actual cap check `gas_units * base_fee <= max_fee` doesn't account for priority.

**How to fix:** In `accept_to_mempool` (`crates/protocol/src/transaction.rs`), change the fee check from `gas_units * base_fee <= max_fee` to `gas_units * (base_fee + max_priority_fee) <= max_fee`, or cap priority at a protocol-defined maximum. Alternatively, add a `max_priority_fee` field to `ProtocolTransaction` and validate `priority_fee <= max_priority_fee`.

---

## Test Status

- `cargo test -p call-protocol` — ~105 unit tests covering gas calculation, fee dynamics, instruction execution, balance operations, asset registry, compliance policies, memo validation, atomic rollback, signature verification (positive + negative), proptest roundtrip encode/decode
- Missing: priority fee enforcement tests
