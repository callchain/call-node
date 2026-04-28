# CallChain Transaction System

**Crate**: `crates/protocol/` (`call-protocol`), `crates/transaction-pool/` (`call-transaction-pool`)
**Spec**: §3.5, §12.2, §17

---

## Goal

Provide a secure, deterministic, and economically sound transaction execution engine that:

1. Uses **standard EVM transactions exclusively** — all protocol operations are invoked via precompile addresses (`0x101`–`0x209`).
2. Exposes all protocol features to MetaMask, Solidity contracts, and standard Ethereum tooling without any custom transaction format.
3. Guarantees **atomic execution** via revm's built-in revert semantics; precompile state mutations are also snapshotted for rollback.
4. Enforces **replay protection** via standard EVM nonces and cryptographic signatures.
5. Meters **gas consumption** per precompile call using standard EVM gas accounting.
6. Manages **mempool admission** to prevent spam and DoS.

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

All transactions are standard **EVM transactions** (`EvmTransaction`, RLP-encoded). Protocol operations are invoked by setting the `to` field to a precompile address (`0x101`–`0x209`) and encoding the function selector + arguments in the `data` field.

| Field | Type | Purpose |
|---|---|---|
| `nonce` | `u64` | Standard EVM replay protection sequence number |
| `gas_price` / `max_fee_per_gas` | `u128` | EIP-1559 fee fields |
| `max_priority_fee_per_gas` | `u128` | Priority tip per gas |
| `gas_limit` | `u64` | Maximum gas units willing to consume |
| `to` | `Option<Address>` | Precompile address (`0x101`–`0x209`) or contract address |
| `value` | `u128` | Native CALL value transfer |
| `data` | `Vec<u8>` | ABI-encoded function selector + arguments |
| `v`, `r`, `s` | `u64`, `U256`, `U256` | Standard Ethereum ECDSA signature |

**Signature verification:** Standard secp256k1 ECDSA recovery. The signer address is recovered from `v/r/s` and must have sufficient balance to cover `gas_limit * max_fee_per_gas`.

### Precompile Operations

All protocol operations are exposed through EVM precompiles (`crates/precompiles/src/`):

| Address | Function | Gas (base) | Authorization |
|---|---|---|---|
| `0x201` | `transfer(uint64,address,uint128)` | 5,000 | Sender balance + compliance |
| `0x201` | `batchTransfer(uint64,address[],uint128[])` | 5,000 per recipient | Sender balance + compliance |
| `0x201` | `approve(uint64,address,uint128)` | 4,000 | Sender |
| `0x201` | `transferFrom(uint64,address,address,uint128)` | 5,500 | Allowance holder |
| `0x201` | `mint(uint64,address,uint128)` | 6,000 | Asset issuer only |
| `0x201` | `burn(uint64,address,uint128)` | 5,000 | Asset issuer only |
| `0x207` | `switchToEvm(uint64,address,uint128)` | 8,000 | Sender balance |
| `0x207` | `switchToProtocol(uint64,address,uint128)` | 8,000 | Sender EVM balance |
| `0x202` | `shieldedDeposit` / `shieldedWithdraw` | 50,000 | ZK proof + nullifier |
| `0x202` | `shieldedTransfer` | 100,000 | ZK proof |
| `0x101` | `submitPrice` | 3,000 | Registered validator |
| `0x103` | `externalBridgeDeposit` | 10,000 | Validator signatures |
| `0x103` | `externalBridgeWithdraw` | 8,000 | Sender balance |
| `0x103` | `challengeBridgeDeposit` | 6,000 | Anyone |
| `0x205` | `updateCompliance` | 6,000 | Asset issuer only |
| `0x209` | `registerAgent` / `grantAgentBalance` / `revokeAgentBalance` | 6,000 | Sender / owner |
| `0x203` | `submitProposal` / `vote` / `queue` / `execute` | 10,000–20,000 | CALL balance / validator |
| `0x203` | `emergencyPause` / `emergencyResume` | 20,000 | Validator quorum |
| `0x204` | `stake` / `unstake` / `claimUnbonded` | 20,000 | Sender balance |

All precompile functions use standard EVM gas accounting. A single EVM transaction can call multiple precompiles (e.g., via a Solidity contract) and revm's built-in revert mechanism ensures atomicity.

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

Single-pool structure for EVM transactions:

| Pool | Type | Capacity | Sorting |
|---|---|---|---|
| `evm_pool` | `EvmTransaction` | 100K | Gas price desc |
| `pending_bridges` | `BridgeOp` | 1K | FIFO |

**Admission checks** (`submit_evm_tx`):
1. Deduplication — reject duplicate tx hash
2. Sequential nonce — reject `nonce < current_evm_nonce`
3. Balance check — `gas_limit * max_fee_per_gas <= sender_balance`
4. Gas limit — `gas_limit <= 10M`
5. Per-address limit — max 2000 pending txs per sender
6. Capacity — evict lowest-gas-price entry if full

**Maintenance**:
- `prune_expired()` — remove txs older than 72 blocks
- `confirm_transactions()` — remove included txs
- `select_transactions()` — drain pool by gas price for block building

### Execution Pipeline

1. **Block production** calls `select_transactions()` to get ordered EVM txs by gas price
2. For each `EvmTransaction` in the block:
   - **Signature verification** — standard secp256k1 recovery from `v/r/s`
   - **Balance check** — verify sender can cover `gas_limit * max_fee_per_gas`
   - Execute via `revm`:
     - If `to` is a precompile address (`0x101`–`0x209`), the corresponding Rust precompile function is called
     - Precompile write operations snapshot `BalanceState`, `ComplianceEngine`, and `ShieldedState` before mutating
     - If a precompile call fails, the snapshot is restored and revm reverts the transaction
     - Standard EVM gas deduction applies
3. Block fees are allocated to validator reward pool / treasury / burn

### Block Execution Result

`Block::execute()` returns a `BlockExecutionResult` (`crates/consensus/src/block.rs`) with **transaction-level** results:

```rust
pub struct TransactionResult {
    pub tx_hash: TxHash,
    pub status: ExecutionStatus,        // Success | Reverted { reason }
    pub gas_used: u64,
    pub fee_amount: u128,
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
    pub bridge_op_count: usize,
    pub system_tx_count: usize,
    pub total_validator_reward: Balance,
    pub evm_gas_used: u64,
    pub agent_events: Vec<call_agent::AgentEvent>,
    pub pending_rollback: Option<RollbackPlan>,
}
```

**Key properties**:
- `transaction_results` is a 1:1 mapping with EVM transactions in the block — each entry represents the full outcome of one tx (atomic success/failure).
- On tx failure, revm automatically reverts all state. Precompile-level snapshots (`BalanceState`, `ComplianceEngine`, `ShieldedState`) are also restored.
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
| `crates/protocol/src/transaction.rs` | `EvmTransaction` handling, gas calculation, fee model, mempool admission |
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
