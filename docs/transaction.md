# CallChain Transaction System

**Crate**: `crates/protocol/` (`call-protocol`), `crates/transaction-pool/` (`call-transaction-pool`)
**Spec**: §3.5, §12.2, §17

---

## Goal

Provide a secure, deterministic, and economically sound transaction execution engine that:

1. Supports both **native protocol operations** (`ProtocolTransaction`) and **EVM transactions** in the same block.
2. Guarantees **atomic execution** — either all instructions in a transaction succeed, or none of them are committed.
3. Enforces **replay protection** via nonces and cryptographic signatures.
4. Meters **gas consumption** per instruction and charges fees fairly.
5. Manages **mempool admission** to prevent spam and DoS.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────┐
│                        Transaction Lifecycle                         │
│                                                                     │
│  ┌────────────┐   ┌────────────┐   ┌──────────┐   ┌────────────┐  │
│  │  Submit    │──▶│  Mempool   │──▶│  Block   │──▶│  Execute   │  │
│  │  (RPC/P2P) │   │  Validate  │   │  Select  │   │  + Commit  │  │
│  └────────────┘   └────────────┘   └──────────┘   └────────────┘  │
│         │                │                │              │          │
│         ▼                ▼                ▼              ▼          │
│    verify_signature()  fee/gas/nonce   priority      atomic        │
│                        dedup check     ordering    instruction     │
│                                              execution+rollback    │
└─────────────────────────────────────────────────────────────────────┘
```

### Transaction Model

`ProtocolTransaction` (`crates/protocol/src/transaction.rs`):

| Field | Type | Purpose |
|---|---|---|
| `sender` | `Address` | Transaction originator |
| `nonce` | `u64` | Replay protection sequence number |
| `instructions` | `Vec<Instruction>` | Ordered list of protocol operations |
| `gas_config` | `GasConfig` | Fee payment strategy (SelfPay / Sponsor) |
| `fee_currency` | `FeeCurrency` | CALL (asset_id=0) or stablecoin |
| `gas_limit` | `u64` | Maximum gas units willing to consume |
| `max_fee` | `u128` | Maximum total fee willing to pay |
| `auth` | `AuthScheme` | Cryptographic signature(s) |

**Auth schemes**:

| Scheme | Verification |
|---|---|
| `SingleSig { signature }` | secp256k1 ECDSA recovery; recovered address must match `sender` |
| `MultiSig { signatures }` | Each signature recovered; threshold read from `SmartAccountRegistry` (falls back to 2) |
| `SessionKey { key, signature }` | Signature recovered; must match the delegated `key` address |

Signature verification is performed via `ProtocolTransaction::verify_signature()`, which calls `recover_secp256k1_signer()` and checks the recovered address against the expected signer. For MultiSig, `verify_signature_with_registry()` reads the account's configured threshold from `SmartAccountRegistry`.

### Instruction Set

All protocol operations are `Instruction` variants (`crates/protocol/src/instructions.rs`):

| Instruction | Gas (base) | Authorization |
|---|---|---|
| `Transfer` | 10,000 + memo bytes | Sender balance |
| `BatchTransfer` | 1,000 per payment + memo | Sender balance |
| `Approve` | 5,000 | Sender |
| `TransferFrom` | 10,000 | Allowance holder |
| `Mint` / `Burn` | 5,000 | Asset issuer only |
| `BridgeDeposit` | 10,000 | Bridge proof validation |
| `ShieldedDeposit` / `ShieldedWithdraw` | 20,000 | ZK proof + nullifier |
| `ShieldedTransfer` | 50,000 | ZK proof |
| `OracleSubmit` | 50,000 | Registered validator |
| `GovernanceSubmitProposal` | 50,000 | CALL balance >= deposit |
| `GovernanceVote` | 10,000 | Registered validator or CALL holder |
| `GovernanceQueue` / `GovernanceExecute` | 10,000 / 50,000 | Proposal state check |
| `GovernanceEmergencyPause` / `EmergencyResume` | 100,000 | Validator quorum |
| `ExternalBridgeDeposit` | 50,000 | Multi-sig validator proof |
| `UpdateCompliance` | 5,000 | Asset issuer only |
| `AgentPay` / `AgentBatchPay` / `AgentCall` / `AgentBridgeDeposit` | 5,000 | Agent permission check |

**Multi-instruction gas discount**:
- 1st instruction: 1.0x
- 2nd–10th: 0.5x
- 11th+: 0.25x

### Gas & Fee Model

**Fee calculation**:
```
fee = gas_units * (base_fee + priority_fee_per_gas)
```

**Base fee adjustment** (EIP-1559-style, per block):
```
if gas_used > target:
    increase = base_fee * (diff/target) * (1/8)
if gas_used < target:
    decrease = base_fee * (|diff|/target) * (1/8)
```

**Fee allocation**:

| Currency | Burn | Validator Reward | Proposer (priority) |
|---|---|---|---|
| CALL | 50% | 50% | 100% |
| Stablecoin | 0% | 50% | 100% |

CALL fees are burned to create deflationary pressure. Stablecoin fees go 50% to treasury (not burned) and 50% to validators.

### Mempool (`transaction-pool`)

Multi-pool structure with separate lanes:

| Pool | Type | Capacity | Sorting |
|---|---|---|---|
| `protocol_pool` | `ProtocolTransaction` | 50K | Priority score desc |
| `evm_pool` | `EvmTransaction` | 100K | Gas price desc |
| `pending_bridges` | `BridgeOp` | FIFO | Entry order |

**Admission checks** (`insert_protocol_tx`):
1. Deduplication — reject duplicate `(sender, nonce)`
2. Sequential nonce — reject `nonce < expected_nonce`
3. Instruction count — `instructions.len() <= MAX_INSTRUCTIONS_PER_TX` (100)
4. Memo size — `total_memo_bytes <= MAX_TOTAL_MEMO_BYTES` (1KB)
5. Sponsor check — `PoolSponsor` and `PerTxSponsor` rejected until implemented
6. Fee check — `score >= min_fee` where `score = max_fee - gas_units * base_fee`
7. Gas limit — `gas_limit <= max_gas_limit`
8. Per-address limit — max txs per sender
9. Capacity — evict lowest-score entry if full

**Maintenance**:
- `prune_expired()` — remove txs older than 72 blocks
- `confirm_transactions()` — remove included txs by hash
- `select_transactions()` — drain pools in priority order for block building

### Execution Pipeline

1. **Block production** calls `select_transactions()` to get ordered txs
2. For each `ProtocolTransaction` in the block:
   - **Signature verification** — `tx.verify_signature_with_registry(smart_accounts)` checks SingleSig/MultiSig/SessionKey; for MultiSig, threshold is read from `SmartAccountRegistry`
   - Call `execute_protocol_instructions()`:
     - Clone `BalanceState`, `ComplianceEngine`, and `ShieldedState` snapshots before execution
     - Execute each instruction in order
     - If any instruction fails, restore all snapshots (rollback)
     - On success, deduct gas fee and emit receipt
3. EVM transactions are executed via `revm` in the same block
4. Block fees are allocated to validator reward pool / treasury / burn

---

## Current Status

### What Works

| Component | Status | Details |
|---|---|---|
| **Signature verification** | Ready | `verify_signature()` covers SingleSig, MultiSig, SessionKey; enforced during block execution via `Block::execute()` |
| **Balance management** | Ready | `checked_add`/`checked_sub` arithmetic; snapshot-based rollback for `BalanceState` |
| **Mempool structure** | Ready | Multi-pool with capacity limits, eviction, per-address caps, dedup, fee filtering |
| **Base fee dynamics** | Ready | EIP-1559-style adjustment with min/max bounds |
| **Fee allocation** | Ready | CALL burn + validator reward; stablecoin treasury + validator reward |
| **Instruction execution** | Ready | All instruction variants have execution arms; governance/bridge/oracle instructions wired |
| **Transaction receipts** | Ready | Persisted per block with status, gas used, logs |

### Recent Fixes

| Fix | Commit | Description |
|---|---|---|
| Governance → Instruction pipeline | `84b2151` | Governance operations now flow through `ProtocolTransaction` + mempool + consensus |
| Bridge → Instruction pipeline | `84b2151` | Bridge deposits submitted as `Instruction::ExternalBridgeDeposit` |
| Signature verification in block execution | — | `Block::execute()` calls `tx.verify_signature_with_registry()` before executing each protocol transaction |
| Economic constants governable | `38c2a4c` | `proposal_deposit`, `min_self_stake`, `base_fee`, etc. mutable via governance |
| Atomic rollback (Gap 1) | `1d742f7` | `ComplianceEngine` and `ShieldedState` now cloned for snapshot-based rollback alongside `BalanceState` |
| Sequential nonce enforcement (Gap 2) | `1d742f7` | Mempool tracks `expected_nonces` per sender; rejects old nonces |
| Gas sponsor stubs (Gap 3) | `1d742f7` | `AuthorizedSponsor` fully implemented via `SponsorRegistry`; `PoolSponsor` and `PerTxSponsor` rejected at mempool |
| Stablecoin fee deduction (Gap 4) | `1d742f7` | `deduct_stablecoin_from_payer()` now uses correct `asset_id` instead of hard-coded CALL |
| Priority fee scoring (Gap 5) | `1d742f7` | `protocol_priority_score()` computes `max_fee - gas_cost` and is enforced at mempool admission |
| Compliance recipient checks (Gap 6) | `1d742f7` | `Transfer`, `BatchTransfer`, `TransferFrom` now check both sender and recipient compliance |
| Custom compliance policy (Gap 8) | `1d742f7` | `CompliancePolicy::Custom` now invokes registered handlers from `ComplianceEngine::custom_handlers` |
| Instruction count limit (Gap 9) | `1d742f7` | `MAX_INSTRUCTIONS_PER_TX` enforced at mempool admission and in payload builder |
| Memo size enforcement (Gap 10) | `1d742f7` | `MAX_TOTAL_MEMO_BYTES` enforced at mempool admission |
| MultiSig threshold (Gap 11) | `1d742f7` | `verify_signature_with_registry()` reads threshold from `SmartAccountRegistry`; falls back to 2 |

---

## Gaps & Suggestions

### Critical / High Severity

#### Gap 7: Compliance State Not Persisted

**Problem**: `ComplianceEngine` is held in `RpcState` in memory only. There is no DB schema for sanctioned addresses, KYC status, or per-address compliance states. On node restart, all compliance data is lost.

**Impact**: After restart, previously blacklisted addresses can transact freely.

**Suggestion**: Add MDBX tables for compliance state:
- `CallCompliancePolicy { asset_id, policy }`
- `CallComplianceStatus { address, policy_id, status }`
Wire load/save in `CallNode::new()` and `persist_state_to_db()`.

---

### Resolved Gaps

The following gaps have been fixed and are documented here for reference:

| Gap | Status | Resolution |
|---|---|---|
| **Gap 1**: Incomplete Atomic Rollback | **Fixed** | `ComplianceEngine` and `ShieldedState` now implement `Clone`; `execute_protocol_instructions()` snapshots all three mutable states and restores on failure |
| **Gap 2**: No Sequential Nonce Enforcement | **Fixed** | Mempool maintains `expected_nonces: HashMap<Address, u64>`; rejects `nonce < expected` at admission |
| **Gap 3**: Gas Sponsors Are Stubs | **Fixed** | `AuthorizedSponsor` fully wired through `SponsorRegistry`; `PoolSponsor` and `PerTxSponsor` explicitly rejected at mempool with descriptive error |
| **Gap 4**: Stablecoin Fee Deducts CALL Instead | **Fixed** | `deduct_stablecoin_from_payer()` now passes the correct `asset_id` to `deduct_balance()` instead of hard-coded `0` |
| **Gap 5**: Priority Fee Ignored in Mempool Admission | **Fixed** | `protocol_priority_score()` computes `max_fee - gas_units * base_fee`; score validated against `min_fee` at admission |
| **Gap 6**: Compliance Only Checks Sender | **Fixed** | `Transfer`, `BatchTransfer`, `TransferFrom` now call `check_compliance_by_policy_id()` on both sender and recipient |
| **Gap 8**: Custom Compliance Policy Always Passes | **Fixed** | `CompliancePolicy::Custom` now iterates registered `custom_handlers` and requires all to approve |
| **Gap 9**: No Instruction Count Limit | **Fixed** | `MAX_INSTRUCTIONS_PER_TX` (100) enforced in mempool admission and payload builder |
| **Gap 10**: Memo Size Not Enforced at Mempool Time | **Fixed** | `MAX_TOTAL_MEMO_BYTES` (1KB) enforced in mempool admission via `total_memo_bytes()` |
| **Gap 11**: MultiSig Has No M-of-N Threshold Validation | **Fixed** | `verify_signature_with_registry()` reads `threshold` from `SmartAccountRegistry`; falls back to 2 if no config found |

---

## Production Readiness Assessment

| Component | Status | Blocker |
|---|---|---|
| Signature verification | Ready | None |
| Balance management | Ready | None |
| Mempool structure | Ready | None |
| Gas/fee model | Ready | None |
| Instruction execution | Ready | None |
| Compliance | Partial | State persistence (Gap 7) |

## Recommended Fix Order

1. **Compliance state persistence** (Gap 7) — requires MDBX schema + node boot/shutdown wiring

---

*Last updated: 2026-04-20*
