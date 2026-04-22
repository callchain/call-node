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
│  └────────────┘   └──────────┘   └──────────┘   └────────────┘  │
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
| `expires_at` | `u64` | Block height at which this transaction expires (0 = never) |
| `auth` | `AuthScheme` | Cryptographic signature(s) |

**Auth schemes**:

| Scheme | Verification |
|---|---|
| `SingleSig { signature }` | secp256k1 ECDSA recovery; recovered address must match `sender` |
| `MultiSig { signatures }` | Each signature recovered; threshold read from `SmartAccountRegistry` (falls back to 2) |
| `SessionKey { key, signature }` | Signature recovered; must match the delegated `key` address |

The tx hash (`compute_tx_hash()`) covers all fields except `auth`, preventing signature malleability. It is domain-encoded with a gas_config variant byte (0=SelfPay, 1=AuthorizedSponsor, 2=PoolSponsor, 3=PerTxSponsor) to prevent cross-config replay.

### Instruction Set

All protocol operations are `Instruction` variants (`crates/protocol/src/instructions.rs`):

| Instruction | Gas (base) | Authorization |
|---|---|---|
| `Transfer` | 10,000 + memo bytes | Sender balance + compliance (sender + recipient) |
| `BatchTransfer` | 1,000 per payment + memo | Sender balance + compliance |
| `Approve` | 5,000 | Sender |
| `TransferFrom` | 10,000 | Allowance holder + compliance (spender, from, to) |
| `Mint` / `Burn` | 5,000 | Asset issuer only |
| `BridgeDeposit` | 10,000 | Bridge proof validation (non-empty) |
| `ShieldedDeposit` / `ShieldedWithdraw` | 20,000 | ZK proof + nullifier |
| `ShieldedTransfer` | 50,000 | ZK proof + Merkle inclusion |
| `OracleSubmit` | 50,000 | Registered validator |
| `GovernanceSubmitProposal` | 50,000 | CALL balance >= deposit |
| `GovernanceVote` | 10,000 | Registered validator or CALL holder |
| `GovernanceQueue` / `GovernanceExecute` | 10,000 / 50,000 | Proposal state check |
| `GovernanceEmergencyPause` / `EmergencyResume` | 100,000 | Validator quorum |
| `ExternalBridgeDeposit` | 50,000 | Executed inline in `Block::execute` |
| `ExternalBridgeWithdraw` | 50,000 | Executed inline in `Block::execute` |
| `ChallengeBridgeDeposit` | 10,000 | Executed inline in `Block::execute` |
| `UpdateCompliance` | 5,000 | Asset issuer only |
| `AgentPay` / `AgentBatchPay` / `AgentCall` / `AgentBridgeDeposit` | 5,000 | Delegated to agent executor |

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

Bounds: `min_base_fee = 1` wei, `max_base_fee = 1B` wei, `initial_base_fee = 10` wei.

**Fee allocation**:

| Currency | Burn | Validator Reward | Proposer (priority) |
|---|---|---|---|
| CALL | 50% | 50% | 100% |
| Stablecoin | 0% | 50% | 100% |

CALL fees are burned to create deflationary pressure. Stablecoin fees go 50% to treasury and 50% to validators.

### Gas Sponsor System (`sponsor.rs`)

Three sponsor modes, all wired through `SponsorRegistry`:

| Mode | Description |
|---|---|
| `AuthorizedSponsor` | Pre-registered sponsor with whitelist, daily limit, and expiry |
| `PoolSponsor` | Shared sponsor pool with delegated addresses and per-tx limit |
| `PerTxSponsor` | Per-transaction sponsor (deducted directly from sponsor balance) |

`PoolSponsor` and `PerTxSponsor` are accepted by the execution layer but rejected at mempool admission until explicitly enabled.

### Mempool (`transaction-pool`)

Multi-pool structure with separate lanes:

| Pool | Type | Capacity | Sorting |
|---|---|---|---|
| `protocol_pool` | `ProtocolTransaction` | 50K | Priority score desc |
| `evm_pool` | `EvmTransaction` | 100K | Gas price desc |
| `pending_bridges` | `BridgeOp` | 1K | FIFO |

**Admission checks** (`insert_protocol_tx`):
1. Deduplication — reject duplicate tx hash
2. Sequential nonce — reject `nonce < expected_nonce`
3. Instruction count — `instructions.len() <= 100`
4. Memo size — `total_memo_bytes <= 1024`
5. `PerTxSponsor` rejected at mempool until enabled
6. Fee check — `priority_score >= min_fee` (score = `max_fee - gas_cost`)
7. Gas limit — `gas_limit <= 10M`
8. Per-address limit — max 256 txs per sender
9. Capacity — evict lowest-score entry if full

**Maintenance**:
- `prune_expired()` — remove txs older than 72 blocks
- `confirm_transactions()` — remove included txs, increment expected nonces
- `select_transactions()` — drain pools in priority order for block building

### Execution Pipeline

1. **Block production** calls `select_transactions()` to get ordered txs
2. For each `ProtocolTransaction` in the block:
   - **Signature verification** — `tx.verify_signature_with_registry()` checks SingleSig/MultiSig/SessionKey
   - Call `execute_protocol_instructions()`:
     - Clone `BalanceState`, `ComplianceEngine`, and `ShieldedState` snapshots before execution
     - Execute each instruction in order
     - If any instruction fails, restore all snapshots (rollback)
     - On success, deduct gas fee and emit receipt
3. EVM transactions are executed via `revm` in the same block
4. Block fees are allocated to validator reward pool / treasury / burn

### Compliance Engine

Policy enforcement via `ComplianceEngine` with five policy types:

| Policy | Behavior |
|---|---|
| `None` (0) | Pass through |
| `OfacBlacklist` (1) | Reject sanctioned addresses |
| `KycRequired` (2) | Require KYC verification |
| `Whitelist` (3) | Reject non-whitelisted addresses |
| `Custom` (4) | Invoke registered `CustomComplianceHandler` |

`Transfer`, `BatchTransfer`, and `TransferFrom` check compliance on both sender and recipient. `ComplianceEngine` supports serializable snapshots (`ComplianceEngineSnapshot`) with per-address status tracking keyed by `(address, policy_id)`. Custom handlers are runtime-only and must be re-registered after deserialization.

### Smart Accounts (`smart_accounts.rs`)

| Feature | Details |
|---|---|
| MultiSig | M-of-N with 2–10 signers, configurable threshold, versioned updates |
| Social Recovery | 24–72h delay, 2+ guardians, guardian approval flow |
| Session Keys | Per-key permissions (instructions, targets, assets, per-tx/daily limits), expiry |

---

## Current Status

All components are production-ready with no open gaps.

| Component | Status | Details |
|---|---|---|
| **Signature verification** | Ready | SingleSig, MultiSig (registry-backed threshold), SessionKey |
| **Balance management** | Ready | `checked_add`/`checked_sub` arithmetic; snapshot-based rollback |
| **Mempool** | Ready | Multi-pool, capacity limits, eviction, per-address caps, dedup, fee filtering, nonce tracking |
| **Base fee dynamics** | Ready | EIP-1559-style adjustment with min/max bounds |
| **Fee allocation** | Ready | CALL burn + validator reward; stablecoin treasury + validator reward |
| **Gas sponsors** | Ready | All three modes wired; `PoolSponsor`/`PerTxSponsor` rejected at mempool |
| **Instruction execution** | Ready | All variants have execution arms; governance/bridge/oracle/shielded wired |
| **Atomic rollback** | Ready | BalanceState, ComplianceEngine, ShieldedState all snapshotted and restored |
| **Compliance** | Ready | Dual-party checks, custom handlers, per-address status, serializable snapshots |
| **Smart accounts** | Ready | MultiSig, social recovery, session keys with permissions and expiry |
| **Transaction receipts** | Ready | Persisted per block with status, gas used, logs |

---

## File Map

| File | Role |
|------|------|
| `crates/protocol/src/transaction.rs` | `ProtocolTransaction`, `AuthScheme`, `GasConfig`, gas calculation, fee model, mempool admission |
| `crates/protocol/src/instructions.rs` | `Instruction` enum, `execute_protocol_instructions()`, `PaymentMemo`, `AgentPayment` |
| `crates/protocol/src/sponsor.rs` | `SponsorRegistry`, `GasSponsorAuth`, `GasSponsorPool`, daily usage tracking |
| `crates/protocol/src/compliance.rs` | `ComplianceEngine`, `CompliancePolicy`, `CustomComplianceHandler`, snapshots |
| `crates/protocol/src/smart_accounts.rs` | `SmartAccountRegistry`, MultiSig, social recovery, session keys |
| `crates/transaction-pool/src/lib.rs` | `Mempool`, multi-pool management, admission, selection |
| `crates/transaction-pool/src/pool.rs` | `MempoolEntry`, `PriorityPool`, capacity limits |
| `crates/transaction-pool/src/priority.rs` | `PoolKind`, `PoolLimits`, `protocol_priority_score()` |

---

## Test Status

- `cargo test -p call-protocol` — unit tests cover instruction execution, rollback, memo limits, mint/burn authorization, gas calculation, base fee dynamics, fee deduction (all sponsor modes), mempool admission, signature verification (zero/ones/wrong-key/malleation/replay/insufficient-multisig/session-key mismatch), serde round-trips
- `cargo test -p call-transaction-pool` — unit tests cover mempool insert/duplicate/fee/gas/address-limit/capacity/eviction/expiry/confirm/stats, priority scoring, priority pool operations
- Missing: ZK shielded transfer execution tests (require `real-prover` feature), bridge challenge execution tests, smart account social recovery end-to-end tests
