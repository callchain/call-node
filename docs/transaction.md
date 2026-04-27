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

All protocol operations are `Instruction` variants (`crates/protocol/src/instructions/types.rs`):

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
| `ValidatorStake` | 50,000 | Executed inline in `Block::execute` via validator state manager |
| `ValidatorUnstake` | 25,000 | Executed inline in `Block::execute` via validator state manager |
| `ValidatorClaimUnbonded` | 25,000 | Executed inline in `Block::execute` via validator state manager |
| `RegisterAsset` | 50,000 | Asset registration authority |
| `RegisterAgent` | 50,000 | Agent registration authority |
| `GrantAgentBalance` / `RevokeAgentBalance` | 10,000 | Agent balance management |

**Validator instructions** (`ValidatorStake`, `ValidatorUnstake`, `ValidatorClaimUnbonded`) are special-cased in `Block::execute` because they require direct access to `ValidatorStateManager` and `AccountState` for escrow transfers. They cannot be routed through the generic `execute_protocol_instructions()` path.

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
     - On success, deduct gas fee
   - Validator instructions (`ValidatorStake`/`Unstake`/`ClaimUnbonded`) are executed inline via `execute_validator_instruction()` with direct access to `ValidatorStateManager`
3. EVM transactions are executed via `revm` in the same block
4. Block fees are allocated to validator reward pool / treasury / burn

### Block Execution Result

`Block::execute()` returns a `BlockExecutionResult` (`crates/consensus/src/block.rs`) with **transaction-level** results:

```rust
pub struct TransactionResult {
    pub tx_hash: TxHash,
    pub status: ExecutionStatus,        // Success | Reverted { reason }
    pub gas_used: u64,
    pub fee_amount: u128,
    pub instruction_count: usize,
    pub agent_events: Vec<call_agent::AgentEvent>,
}

pub struct BlockExecutionResult {
    pub transaction_results: Vec<TransactionResult>,
    pub payment_root: Hash,
    pub evm_state_root: Hash,
    pub bridge_root: Hash,
    pub receipt_root: Hash,
    pub state_root: Hash,
    pub evm_tx_count: usize,
    pub protocol_tx_count: usize,
    pub bridge_op_count: usize,
    pub system_tx_count: usize,
    pub total_validator_reward: Balance,
    pub evm_gas_used: u64,
    pub agent_events: Vec<call_agent::AgentEvent>,
    pub pending_rollback: Option<RollbackPlan>,
}
```

**Key properties**:
- `transaction_results` is a 1:1 mapping with protocol transactions in the block — each entry represents the full outcome of one tx (atomic success/failure).
- On tx failure, all state snapshots (`BalanceState`, `EvmState`, `BridgeState`, `ValidatorState`) are restored, and a `TransactionResult` with `status = Reverted { reason }` is recorded.
- `compute_receipt_root()` hashes `transaction_results` (tx hash + success flag + gas + fee) plus `agent_events` into the block header's `receipt_root`, making receipt availability verifiable.

### Receipts

**Auto-generation**: After `Block::execute()` succeeds, `transaction_results` are automatically converted to `ProtocolReceipt`s and stored in `RpcState.receipts` at block finalize time (in `bft_loop.rs`, `block_producer.rs`, and `sync.rs`).

```rust
pub struct ProtocolReceipt {
    pub tx_hash: TxHash,
    pub status: ExecutionStatus,         // Success | Reverted { reason }
    pub gas_used: u64,
    pub gas_payer: Address,
    pub fee_currency: FeeCurrency,
    pub fee_amount: u128,
    pub block_number: u64,
    pub instruction_results: Vec<InstructionExecResult>, // per-instruction detail (future)
    pub logs: Vec<LogEntry>,             // EVM logs + agent events (future)
    pub memos: Vec<MemoEntry>,
    pub state_changes: Vec<StateChange>,
}
```

**Storage**: Receipts are persisted incrementally (not full overwrite):
- `CallReceipts` table: `tx_hash -> ProtocolReceipt`
- `CallReceiptsByBlock` table: `block_number -> Vec<TxHash>` (block-level index)

The block-level index enables:
- Efficient `eth_getBlockReceipts` (O(1) block lookup)
- Efficient pruning: `delete_receipts_by_block()` deletes all receipts for a block plus the index entry in one pass

**RPC access**:
- `eth_getTransactionReceipt(tx_hash)` → returns `ProtocolReceipt` if present
- `eth_getBlockReceipts(block_number)` → returns all receipts for a block via the index

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
| **Instruction execution** | Ready | All variants have execution arms; governance/bridge/oracle/shielded/validator wired |
| **Atomic rollback** | Ready | BalanceState, ComplianceEngine, ShieldedState, EvmState, BridgeState, ValidatorState all snapshotted and restored |
| **Compliance** | Ready | Dual-party checks, custom handlers, per-address status, serializable snapshots |
| **Smart accounts** | Ready | MultiSig, social recovery, session keys with permissions and expiry |
| **Transaction receipts** | Ready | Auto-generated from `TransactionResult` at block finalize; block-level index (`CallReceiptsByBlock`); incremental persistence |
| **Receipt root** | Ready | `compute_receipt_root` hashes tx-level results + agent events into block header |

---

## File Map

| File | Role |
|------|------|
| `crates/protocol/src/transaction.rs` | `ProtocolTransaction`, `AuthScheme`, `GasConfig`, gas calculation, fee model, mempool admission |
| `crates/protocol/src/instructions/types.rs` | `Instruction` enum, all instruction variants |
| `crates/protocol/src/instructions/exec.rs` | `execute_protocol_instructions()`, per-instruction execution logic |
| `crates/protocol/src/tx/gas.rs` | Per-instruction gas cost table |
| `crates/protocol/src/receipts.rs` | `ProtocolReceipt`, `InstructionExecResult`, `LogEntry`, `MemoEntry`, `StateChange` |
| `crates/protocol/src/sponsor.rs` | `SponsorRegistry`, `GasSponsorAuth`, `GasSponsorPool`, daily usage tracking |
| `crates/protocol/src/compliance.rs` | `ComplianceEngine`, `CompliancePolicy`, `CustomComplianceHandler`, snapshots |
| `crates/protocol/src/smart_accounts.rs` | `SmartAccountRegistry`, MultiSig, social recovery, session keys |
| `crates/consensus/src/block.rs` | `Block`, `BlockHeader`, `BlockExecutionResult`, `TransactionResult`, `compute_receipt_root`, `Block::execute()` |
| `crates/consensus/src/exec/validator.rs` | `execute_validator_instruction()` — inline validator stake/unstake/claim execution |
| `crates/transaction-pool/src/lib.rs` | `Mempool`, multi-pool management, admission, selection |
| `crates/transaction-pool/src/pool.rs` | `MempoolEntry`, `PriorityPool`, capacity limits |
| `crates/transaction-pool/src/priority.rs` | `PoolKind`, `PoolLimits`, `protocol_priority_score()` |
| `crates/storage/src/reth_db.rs` | `CallReceipts`, `CallReceiptsByBlock` table definitions |
| `crates/node/src/state_persist.rs` | `save_receipts()`, `load_receipts()`, `load_receipts_by_block()`, `delete_receipts_by_block()` |
| `crates/rpc/src/handlers/state.rs` | `RpcState::store_receipt()`, `get_receipt()`, `get_receipts_by_block()` |

---

## Test Status

- `cargo test -p call-protocol` — unit tests cover instruction execution, rollback, memo limits, mint/burn authorization, gas calculation, base fee dynamics, fee deduction (all sponsor modes), mempool admission, signature verification (zero/ones/wrong-key/malleation/replay/insufficient-multisig/session-key mismatch), serde round-trips
- `cargo test -p call-transaction-pool` — unit tests cover mempool insert/duplicate/fee/gas/address-limit/capacity/eviction/expiry/confirm/stats, priority scoring, priority pool operations
- `cargo test -p call-consensus` — unit tests cover block execution, `TransactionResult` generation, receipt root computation, validator instruction execution, snapshot rollback
- Missing: ZK shielded transfer execution tests (require `real-prover` feature), bridge challenge execution tests, smart account social recovery end-to-end tests
