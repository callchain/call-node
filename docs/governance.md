# Governance Design

## Overview

The CallChain governance system enables decentralized decision-making for protocol parameters, upgrades, treasury spending, and emergency actions. Validators and CALL token holders participate in a dual-track voting system with review periods, timelocks, and economic incentives.

**Crate**: `crates/governance/` (`call-governance`)
**Spec**: §13.3

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      User / Validator                         │
│  ┌──────────────────────────────────────────────────┐        │
│  │ 1. call_governanceSubmitProposal (RPC)            │        │
│  │ 2. call_governanceVote (RPC)                      │        │
│  │ 3. call_governanceQueue (RPC)                     │        │
│  │ 4. call_governanceExecute (RPC)                   │        │
│  │ 5. call_governanceEmergencyPause (RPC)            │        │
│  └──────────────────────────────────────────────────┘        │
└─────────────────────┬───────────────────────────────────────┘
                      │
                      ▼
              ┌───────────────┐
              │GovernanceManager│ ← persisted to reth-db (CallGovernanceState)
              │ (Arc<RwLock>)   │
              └───────┬────────┘
                      │
              ┌───────┴────────┐
              │ ProposalExecutor│ ← wired via wire_governance_executor()
              │ (trait impl)    │
              └───────┬────────┘
                      │
      ┌───────────────┼───────────────┐
      ▼               ▼               ▼
 ForkManager     Consensus       Fee Params / Compliance / etc.
 (upgrade)      (slash)
```

---

## GovernanceManager

### State

| Field | Purpose |
|---|---|
| `proposals` | All proposals keyed by ID |
| `validator_addresses` | Validator ID → Address mapping for 1=1 voting |
| `call_balances` | Address → CALL balance for balance-weighted voting |
| `asset_issuers` | Asset ID → Issuer Address for joint voting |
| `delegations` | Delegator → VoteDelegation for delegated voting |
| `deposits` | Proposer → Deposit amount (escrowed) |
| `voted_addresses` | Proposal ID → Set of voters who already voted |
| `current_block` | Current block height for time-based transitions |
| `emergency_pause` | Emergency pause state with signature collection |
| `executor` | `Option<Arc<dyn ProposalExecutor>>` — real on-chain effects |

### Proposal Types (9 variants)

| Type | Voting Model | Quorum | Description |
|---|---|---|---|
| `ParameterChange` | 1=1 validator + balance-weighted | 2/3 validators | Update protocol parameters |
| `ProtocolUpgrade` | 1=1 validator + balance-weighted | max(2/3 validators, 20% supply) | Schedule protocol version upgrade |
| `TreasurySpend` | CALL balance-weighted | 20% supply | Transfer from treasury |
| `ValidatorSlash` | 1=1 validator + balance-weighted | 2/3 validators | Remove/slash a validator |
| `ComplianceUpdate` | Joint (issuer + validator) | Simple majority | Update asset compliance policy |
| `EmergencyPause` | Validator signatures | 2/3 validators | Halt chain operations |
| `FeeCurrencyAdd` | 1=1 validator | Simple majority | Register new fee currency |
| `FeeCurrencyRemove` | 1=1 validator | Simple majority | Remove fee currency (with grace period) |
| `FeeCurrencyCap` | 1=1 validator | Simple majority | Change fee currency cap (BPS) |

### Voting Weights

- **Validator 1=1**: Each registered validator gets 1 vote. Used for `ParameterChange`, `ProtocolUpgrade`, `ValidatorSlash`, `EmergencyPause`, `FeeCurrency*`.
- **CALL balance-weighted**: Voting power equals caller's CALL balance + delegated power. Used for `TreasurySpend` (primary), and as max(1, balance) for `ParameterChange`/`ProtocolUpgrade`/`ValidatorSlash`.
- **Joint (issuer + validator)**: Validators get 1 vote each; asset issuers get `TOTAL_SUPPLY / 10` weight. Used for `ComplianceUpdate`.

### Proposal State Machine

```
Pending ──(review period passes)──► Active ──(voting period passes)──┐
  │                                                                   │
  │                                                                   ├── quorum met ──► Passed ──► Queued ──► Executed
  │                                                                   │                                    │
  │                                                                   └── quorum failed ─► Defeated       └── timeout ──► Expired
  │
  └── deposit confiscated on Defeated/Expired
```

### Timeline

| Phase | Duration | Blocks (250ms) |
|---|---|---|
| Review period | 2 days | 691,200 |
| Voting period | 7 days | 2,419,200 |
| Timelock | 7 days | 2,419,200 |
| Execution timeout | 30 days | 10,368,000 |

**Note**: `EmergencyPause` proposals skip the timelock — execution block equals queue block.

### Constants

| Constant | Value | Description |
|---|---|---|
| `PROPOSAL_DEPOSIT` | 10,000 CALL (10^22) | Required deposit to submit proposal |
| `REVIEW_PERIOD_BLOCKS` | 691,200 | ~2 days before voting opens |
| `VOTING_PERIOD_BLOCKS` | 2,419,200 | ~7 days voting window |
| `TIMELOCK_PERIOD_BLOCKS` | 2,419,200 | ~7 days timelock after passing |
| `EXECUTION_TIMEOUT_BLOCKS` | 10,368,000 | ~30 days max time to execute after queuing |
| `TOTAL_SUPPLY` | 1B CALL (10^27) | Total token supply |

### Vote Delegation

Addresses can delegate voting power to another address:

```rust
struct VoteDelegation {
    delegator: Address,
    delegate: Address,
    amount: Balance,
    expires_at: u64, // block height
}
```

Delegated power is added to the delegate's CALL balance when computing voting power. Delegations auto-expire at `expires_at`.

### Emergency Pause

Two independent mechanisms can pause the chain:

1. **Governance proposal**: `ProposalType::EmergencyPause` follows the standard lifecycle but skips timelock upon execution.
2. **Validator signatures**: `emergency_pause_initiate()` collects validator addresses. Once 2/3 of validators have signed, the pause activates immediately. `emergency_pause_resume()` resets the state (requires governance).

### Economic Incentives

- **Deposit requirement**: 10,000 CALL to submit a proposal (prevents spam)
- **Deposit return**: Returned to proposer upon successful execution
- **Deposit confiscation**: Lost if proposal is defeated (fails quorum) or expires (not executed within timeout)

### Execution Callbacks

`ProposalExecutor` trait enables real on-chain side effects when proposals are executed:

```rust
pub trait ProposalExecutor: Send + Sync {
    fn on_proposal_executed(&self, proposal: &Proposal) -> Result<(), String>;
}
```

`GovernanceManager` holds `executor: Option<Arc<dyn ProposalExecutor>>`. The node layer provides `NodeProposalExecutor` (`crates/rpc/src/handlers.rs`) which dispatches by proposal type:

| Proposal Type | Executor Action |
|---|---|
| `ProtocolUpgrade` | `ForkManager.schedule_governance_upgrade(...)` |
| `ValidatorSlash` | Logs (TODO: consensus integration) |
| `EmergencyPause` | Already handled by `apply_proposal` |
| `FeeCurrencyAdd` | Logs (TODO: fee registry integration) |
| `FeeCurrencyRemove` | Logs (TODO: fee registry integration) |
| `FeeCurrencyCap` | Logs (TODO: fee registry integration) |
| `ParameterChange` | Logs (TODO: consensus params integration) |
| `ComplianceUpdate` | Logs (TODO: compliance engine integration) |
| `TreasurySpend` | Handled in `apply_proposal` (in-memory transfer) |

### Persistence

`GovernanceManager` derives `Serialize`/`Deserialize` (the `executor` field is skipped via `#[serde(skip)]`). State is persisted to reth-db:

| Table | Purpose |
|---|---|
| `CallGovernanceState` | Single-entry snapshot of full `GovernanceManager` state |

Persistence is wired through:
- `save_governance_state` / `load_governance_state` in `crates/node/src/lib.rs`
- Called in `persist_state_to_db` (full rebuild every 1000 blocks + shutdown flush)
- Called in `persist_state_incremental` (every block)
- Loaded in `CallNode::new()` and `load_state_from_db`
- Executor rewired after load via `wire_governance_executor()`

---

## RPC Interface

| Method | Parameters | Returns |
|---|---|---|
| `call_governanceSubmitProposal` | `{proposer, proposalType, title, description}` | `{proposalId, status}` |
| `call_governanceVote` | `{proposalId, voter, vote}` | `{status}` |
| `call_governanceQueue` | `{proposalId}` | `{status}` |
| `call_governanceExecute` | `{proposalId}` | `{status}` |
| `call_governanceGetProposal` | `{proposalId}` | Proposal details |
| `call_governanceGetAllProposals` | — | All proposals |
| `call_governanceEmergencyPause` | `{validatorId, reason}` | `{activated, isPaused}` |
| `call_governanceIsPaused` | — | `{isPaused}` |

---

## Identified Gaps

### Gap 1: `apply_proposal` only logs — no real execution callbacks

**Problem**: `GovernanceManager::apply_proposal()` only emits `tracing::info!` log messages for most proposal types. No actual on-chain state changes occur:

- `ParameterChange`: logs but doesn't update consensus/protocol params
- `ProtocolUpgrade`: logs but doesn't call `ForkManager.schedule_governance_upgrade()`
- `ValidatorSlash`: removes from governance's local validator list but doesn't slash consensus state
- `ComplianceUpdate`: logs but doesn't update compliance engine
- `FeeCurrencyAdd/Remove/Cap`: logs but doesn't update fee currency registry
- `EmergencyPause`: correctly sets `emergency_pause.is_paused` (this one works)

**Solution**: Introduce a `ProposalExecutor` trait (see "Execution Callbacks" section above).

**Status**: Implemented. `NodeProposalExecutor` in `crates/rpc/src/handlers.rs` dispatches to fork manager. Other proposal types still log-only — `TODO` comments mark where integration is needed (consensus slash, fee registry, compliance engine, consensus params).

### Gap 2: Governance state is in-memory only

**Problem**: `GovernanceManager` is initialized as `GovernanceManager::new()` in `RpcState::new()`. All proposals, votes, deposits, and delegations are lost on node restart.

**Impact**:
- Proposals in flight are wiped
- Votes cast are lost
- Deposits are not recoverable
- Emergency pause state is reset

**Solution**: Add DB persistence (see "Persistence" section above).

**Status**: Implemented. Governance state persists across node restarts via reth-db (MDBX). `Executor` field skipped during serialization, rewired after load.

### Gap 3: No ForkManager integration for `ProtocolUpgrade`

**Problem**: `ProtocolUpgrade` proposals set `activation_block` but the governance module doesn't communicate this to the `ForkManager` in `crates/consensus/src/fork.rs`. The `ForkManager` already supports `schedule_governance_upgrade()` but it's never called from the governance flow.

**Solution**: Addressed by Gap 1. The `ProposalExecutor` implementation for `ProtocolUpgrade` calls:

```rust
ForkManager.schedule_governance_upgrade(
    version,
    activation_height,
    proposal_id,
    current_height,
);
```

**Status**: Implemented (via Gap 1).

---

## Crate Structure

```
crates/governance/
├── Cargo.toml
└── src/
    └── lib.rs          # GovernanceManager, ProposalType, Vote, errors,
                        # ProposalExecutor trait, EmergencyPauseState
```

### Dependencies

- `call-primitives` — `Address`, `AssetId`, `Balance`, `ValidatorId`
- `serde`, `serde_json` — serialization for proposals and execution data
- `thiserror` — error types
- `tracing` — logging for proposal lifecycle events

### Downstream Crates

| Crate | Usage |
|---|---|
| `call-rpc` | `GovernanceManager` in `RpcState`, RPC handlers, `NodeProposalExecutor` |
| `call-protocol` | Test suite (`test_governance_flow.rs`) |
| `call-node` | Integration tests (`test_fork_upgrade.rs`), DB persistence wiring |
| `call-storage` | `CallGovernanceState` table definition |
