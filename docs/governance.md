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
│  │ 1. call_governanceSubmitProposal (RPC, signed)    │        │
│  │ 2. call_governanceVote (RPC, signed)              │        │
│  │ 3. call_governanceQueue (RPC)                     │        │
│  │ 4. call_governanceExecute (RPC)                   │        │
│  │ 5. call_governanceEmergencyPause (RPC)            │        │
│  └──────────────────────────────────────────────────┘        │
└─────────────────────┬───────────────────────────────────────┘
                      │
                      ▼
              ┌───────────────────┐
              │ GovernanceManager │ ← persisted to reth-db (CallGovernanceState)
              │ (Arc<RwLock>)     │    balance_source reads from real BalanceState
              └────────┬──────────┘
                       │
                       ├── auto-advance every block in production loop
                       │  (Pending → Active → Queued → Executed/Expired)
                       │
              ┌────────┴──────────┐
              │ NodeProposalExecutor │ ← wired via wire_governance_executor()
              │ (trait impl)         │
              └────────┬────────────┘
                       │
      ┌────────────────┼────────────────┬──────────────┬─────────────┬──────────────┐
      ▼                ▼                ▼              ▼             ▼              ▼
 ForkManager      ValidatorState    FeeCurrencyReg   AssetRegistry  OracleManager  ConsensusParams
 (upgrade)        (slash/remove)    (add/remove/cap) (compliance)   (config)       (params)
                                    FeeParams        GovernanceConfig
                                    (fee params)     (governance params)
```

---

## GovernanceManager

### State

| Field | Purpose |
|---|---|
| `proposals` | All proposals keyed by ID |
| `validator_addresses` | Validator ID → Address mapping for 1=1 voting |
| `call_balances` | Address → CALL balance for balance-weighted voting (fallback) |
| `balance_source` | `Option<BalanceSource>` — reads real on-chain CALL balances |
| `asset_issuers` | Asset ID → Issuer Address for joint voting |
| `delegations` | Delegator → VoteDelegation for delegated voting |
| `deposits` | Proposer → Deposit amount (escrowed) |
| `voted_addresses` | Proposal ID → Set of voters who already voted |
| `current_block` | Current block height for time-based transitions |
| `emergency_pause` | Emergency pause state with signature collection |
| `executor` | `Option<Arc<dyn ProposalExecutor>>` — real on-chain effects |
| `scheduled_upgrades` | Protocol upgrades scheduled via governance proposals |
| `compliance_policies` | Asset ID → compliance policy ID |
| `fee_currencies` | Asset ID → `FeeCurrencyEntry` (name, oracle key, added block) |
| `fee_currencies_pending_removal` | Asset ID → grace period end block |
| `fee_currency_cap_bps` | Fee currency cap in basis points |
| `validator_pubkeys` | Validator ID → Ed25519 public key (32 bytes) |

### Proposal Types (10 variants)

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
| `ValidatorKeyRotation` | 1=1 validator | Simple majority | Rotate validator Ed25519 key |

### Voting Weights

- **Validator 1=1**: Each registered validator gets 1 vote. Used for `ParameterChange`, `ProtocolUpgrade`, `ValidatorSlash`, `EmergencyPause`, `FeeCurrency*`.
- **CALL balance-weighted**: Voting power equals caller's real on-chain CALL balance + delegated power. Used for `TreasurySpend` (primary), and as max(1, balance) for `ParameterChange`/`ProtocolUpgrade`/`ValidatorSlash`. Balance is read via `get_voting_balance()` which checks `balance_source` (real `BalanceState`) first, falling back to `call_balances`.
- **Joint (issuer + validator)**: Validators get 1 vote each; asset issuers get `TOTAL_SUPPLY / 10` weight. Used for `ComplianceUpdate`.

### Proposal State Machine

Proposals auto-advance every block via `GovernanceManager::advance()` called in the block production loop:

```
Pending ──(review period passes)──► Active ──(voting period passes)──┐
  │                                                                   │
  │                                                                   ├── quorum met ──► Passed ──► Queued ──► Executed
  │                                                                   │                                    │
  │                                                                   └── quorum failed ─► Defeated       └── timeout ──► Expired
  │
  └── deposit confiscated on Defeated/Expired
```

**Auto-transitions** (`advance()`):
- `Pending → Active`: when `current_block >= start_block`
- `Active → Passed/Queued`: when `current_block > end_block` and quorum met
- `Active → Defeated`: when `current_block > end_block` and quorum failed
- `Queued → Executed`: when `current_block >= execution_block`
- `Queued → Expired`: when `current_block > execution_block + EXECUTION_TIMEOUT_BLOCKS`

### Timeline

| Phase | Duration | Blocks (250ms) |
|---|---|---|
| Review period | 2 days | 691,200 |
| Voting period | 7 days | 2,419,200 |
| Timelock | 7 days | 2,419,200 |
| Execution timeout | 30 days | 10,368,000 |

**Note**: `EmergencyPause` proposals skip the timelock — execution block equals queue block.

### Constants

**Hardcoded** (require code change):

| Constant | Value | Description |
|---|---|---|
| `REVIEW_PERIOD_BLOCKS` | 691,200 | ~2 days before voting opens |
| `VOTING_PERIOD_BLOCKS` | 2,419,200 | ~7 days voting window |
| `TIMELOCK_PERIOD_BLOCKS` | 2,419,200 | ~7 days timelock after passing |
| `EXECUTION_TIMEOUT_BLOCKS` | 10,368,000 | ~30 days max time to execute after queuing |
| `PROPOSAL_COOLDOWN_BLOCKS` | 345,600 | ~1 day between proposals by same address |
| `TOTAL_SUPPLY` | 1B CALL (10^27) | Total token supply |

**Governable** (mutable via `ParameterChange` proposal):

| Field | Default | Config Location | `param_id` Prefix |
|---|---|---|---|
| `proposal_deposit` | 10,000 CALL | `GovernanceConfig` | `governance.proposal_deposit` |
| `asset_registration_fee` | 10 CALL | `GovernanceConfig` | `governance.asset_registration_fee` |
| `min_self_stake` | 1,000,000 CALL | `ValidatorStateManager` | `validator.min_self_stake` |
| `offline_slash_rate_bps` | 10 (0.1%) | `ValidatorStateManager` | `validator.offline_slash_rate_bps` |
| `min_market_cap_usd` | 100,000,000 | `FeeCurrencyRegistry` | `fee_currency.min_market_cap_usd` |
| `stablecoin_cap_bps` | 5000 (50%) | `FeeCurrencyRegistry` | `fee_currency.stablecoin_cap_bps` |
| `validator_quorum_bps` | 6667 (2/3) | `GovernanceConfig` | `governance.validator_quorum_bps` |
| `supply_quorum_bps` | 2000 (20%) | `GovernanceConfig` | `governance.supply_quorum_bps` |
| `treasury_quorum_bps` | 2000 (20%) | `GovernanceConfig` | `governance.treasury_quorum_bps` |
| `simple_majority_bps` | 5001 (50%+1) | `GovernanceConfig` | `governance.simple_majority_bps` |
| `review_period_blocks` | 691,200 | `GovernanceConfig` | `governance.review_period_blocks` |
| `voting_period_blocks` | 2,419,200 | `GovernanceConfig` | `governance.voting_period_blocks` |
| `timelock_period_blocks` | 2,419,200 | `GovernanceConfig` | `governance.timelock_period_blocks` |
| `execution_timeout_blocks` | 10,368,000 | `GovernanceConfig` | `governance.execution_timeout_blocks` |
| `base_fee` | 10 | `FeeParams` | `fee.base_fee` |
| `target_gas_per_block` | 10,000,000 | `FeeParams` | `fee.target_gas_per_block` |
| `max_gas_per_block` | 20,000,000 | `FeeParams` | `fee.max_gas_per_block` |
| `oracle_fee_share_bps` | 100 (1%) | `FeeParams` | `fee.oracle_fee_share_bps` |
| `max_validators` | 1000 | `ConsensusParams` | `consensus.max_validators` |
| `subset_size` | 21 | `ConsensusParams` | `consensus.subset_size` |
| `block_time_millis` | 250 | `ConsensusParams` | `consensus.block_time_millis` |
| `epoch_length` | 1000 | `ConsensusParams` | `consensus.epoch_length` |
| `update_interval` | 1000 | `OracleConfig` | `oracle.update_interval` |
| `outlier_threshold_bps` | 500 (5%) | `OracleConfig` | `oracle.outlier_threshold_bps` |
| `outlier_tolerance` | 10 | `OracleConfig` | `oracle.outlier_tolerance` |
| `twap_window_secs` | 86,400 (24h) | `OracleConfig` | `oracle.twap_window_secs` |
| `staleness_secs` | 900 (15min) | `OracleConfig` | `oracle.staleness_secs` |
| `min_data_sources` | 2 | `OracleConfig` | `oracle.min_data_sources` |

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

- **Deposit requirement**: `proposal_deposit` CALL to submit a proposal (default 10,000 CALL; prevents spam)
- **Deposit return**: Returned to proposer upon successful execution
- **Deposit confiscation**: Lost if proposal is defeated (fails quorum) or expires (not executed within timeout)
- **All economic constants are governable**: The deposit amount, stake minimums, slash rates, and fee parameters can be changed via `ParameterChange` proposals without a code upgrade.

### Balance Source

Deposit checks and voting power calculations read from the real on-chain `BalanceState` (asset ID 0 = CALL), not a separate in-memory map:

```rust
pub type BalanceSource = Arc<dyn Fn(Address) -> Balance + Send + Sync>;
```

Wired in `wire_governance_executor()`:
```rust
let balance_source = Arc::new(move |addr: Address| {
    state.balance_state.read().ok()
        .map(|s| s.balances.get_balance(0, &addr)).unwrap_or(0)
});
```

`call_balances` is retained as a fallback (used in tests where real balances aren't set up). The field is `#[serde(skip)]` and rewired on every node load.

### Execution Model

Governance uses a two-phase execution model:

1. **`apply_proposal`** — Updates governance-internal state (always runs)
2. **`ProposalExecutor::on_proposal_executed`** — Applies cross-system side effects (runs if executor is wired)

```rust
pub trait ProposalExecutor: Send + Sync {
    fn on_proposal_executed(&self, proposal: &Proposal) -> Result<(), String>;
}
```

`GovernanceManager` holds `executor: Option<Arc<dyn ProposalExecutor>>`. The node layer provides `NodeProposalExecutor` (`crates/rpc/src/handlers.rs`) which dispatches by proposal type.

#### `apply_proposal` Internal Effects

| Proposal Type | Internal State Change |
|---|---|
| `ParameterChange` | Updates `GovernanceConfig` fields for `governance.*` and `protocol.*` params |
| `ProtocolUpgrade` | Appends to `scheduled_upgrades` (tracks proposal_id, activation_block, changelog) |
| `TreasurySpend` | Transfers CALL from treasury to recipient via `call_balances` |
| `ValidatorSlash` | Removes validator from `validator_addresses`, `call_balances`, and `validator_pubkeys` |
| `ComplianceUpdate` | Updates `compliance_policies[asset_id] = new_policy` |
| `EmergencyPause` | Sets `emergency_pause.is_paused = true` and records reason |
| `FeeCurrencyAdd` | Inserts into `fee_currencies` and cancels any pending removal |
| `FeeCurrencyRemove` | Records grace period end block in `fee_currencies_pending_removal` |
| `FeeCurrencyCap` | Updates `fee_currency_cap_bps` |
| `ValidatorKeyRotation` | Updates `validator_pubkeys[validator_id] = new_pubkey` |

#### `NodeProposalExecutor` Cross-System Effects

| Proposal Type | Cross-System Action |
|---|---|
| `ProtocolUpgrade` | `ForkManager.schedule_governance_upgrade(version, activation_block, proposal_id, current_height)` |
| `ValidatorSlash` | `ValidatorStateManager.remove_validator(validator_id)` — removes from consensus set |
| `EmergencyPause` | Confirmed via logging (pause state is governance-internal) |
| `ParameterChange` | Routes by `param_id` prefix to consensus, validator, oracle, fee_currency, and fee params (see below) |
| `ComplianceUpdate` | Maps `new_policy` u8 to `CompliancePolicy`, updates `AssetRegistry.compliance_policy` |
| `FeeCurrencyAdd` | `FeeCurrencyRegistry.add_fee_currency(entry, proposal_id)` with decoded oracle key |
| `FeeCurrencyRemove` | `FeeCurrencyRegistry.remove_fee_currency(asset_id, grace_period_blocks)` |
| `FeeCurrencyCap` | `FeeCurrencyRegistry.stablecoin_cap_bps = new_cap_bps` |
| `TreasurySpend` | Confirmed via logging (transfer is governance-internal) |
| `ValidatorKeyRotation` | Verifies old-key signature, calls `ValidatorStateManager.rotate_key()` |

#### ParameterChange Prefix Routing

`ParameterChange` proposals use a `param_id` prefix system to route updates to the correct subsystem. The `new_value` field is parsed as JSON. Supported prefixes:

| Prefix | Handler | Target Subsystem | Example `param_id` | Example `new_value` |
|---|---|---|---|---|
| `governance.*` | `apply_proposal` (internal) | `GovernanceConfig` | `governance.proposal_deposit` | `{"proposal_deposit": 5000000000000000000000}` |
| `protocol.*` | `apply_proposal` (internal) | `GovernanceConfig` (protocol-level) | `protocol.asset_registration_fee` | `{"asset_registration_fee": 5000000000000000000}` |
| `consensus.*` | Executor | `ConsensusParams` | `consensus.subset_size` | `{"subset_size": 31}` |
| `validator.*` | Executor | `ValidatorStateManager` | `validator.min_self_stake` | `{"min_self_stake": 500000000000000000000000}` |
| `oracle.*` | Executor | `OracleConfig` | `oracle.outlier_threshold_bps` | `{"outlier_threshold_bps": 300}` |
| `fee_currency.*` | Executor | `FeeCurrencyRegistry` | `fee_currency.min_market_cap_usd` | `{"min_market_cap_usd": 50000000}` |
| (no prefix / `fee.*`) | Executor | `FeeParams` | `base_fee` | `{"base_fee": 20}` |

All parameter changes take effect immediately upon proposal execution (no restart required).

### Validator Registration

Validators are registered into governance from two sources:

1. **Genesis boot**: In `boot.rs`, genesis validators are registered during `Genesis::apply()`:
   ```rust
   for (i, val) in self.validators.iter().enumerate() {
       gov.register_validator(i as u32, parse_address(&val.address)?);
   }
   ```
2. **Runtime sync**: Every block in the production loop (step 8c), all validators from `ValidatorStateManager` are synced idempotently into governance:
   ```rust
   for (id, stake) in vs.get_all_validators().iter() {
       gov.register_validator(*id, stake.address);
   }
   ```

### Persistence

`GovernanceManager` derives `Serialize`/`Deserialize`. Non-serializable fields (`executor`, `balance_source`) are skipped via `#[serde(skip)]`. State is persisted to reth-db:

| Table | Purpose |
|---|---|
| `CallGovernanceState` | Single-entry snapshot of full `GovernanceManager` state |

Persistence is wired through:
- `save_governance_state` / `load_governance_state` in `crates/node/src/lib.rs`
- Called in `persist_state_to_db` (full rebuild every 1000 blocks + shutdown flush)
- Called in `persist_state_incremental` (every block)
- Loaded in `CallNode::new()` and `load_state_from_db`
- `executor` and `balance_source` rewired after load via `wire_governance_executor()`

---

## RPC Interface

| Method | Parameters | Returns |
|---|---|---|
| `call_governanceSubmitProposal` | `{proposer, proposalType, title, description, signature?}` | `{proposalId, status}` |
| `call_governanceVote` | `{proposalId, voter, vote, signature?}` or `(id, voter, vote)` | `{status}` |
| `call_governanceQueue` | `{proposalId}` | `{status}` |
| `call_governanceExecute` | `{proposalId}` | `{status}` |
| `call_governanceGetProposal` | `{proposalId}` | Proposal details |
| `call_governanceGetAllProposals` | — | All proposals |
| `call_governanceEmergencyPause` | `{validatorId, reason}` | `{activated, isPaused}` |
| `call_governanceIsPaused` | — | `{isPaused}` |

### Authentication

`call_governanceSubmitProposal` and `call_governanceVote` accept an optional `signature` field. When provided, the signature is verified as a secp256k1 signature over the call payload:

- **SubmitProposal**: signs `keccak256(proposer ++ title ++ description)`, recovered address must match `proposer`
- **Vote**: signs `keccak256(proposalId_be ++ voter ++ vote_string)`, recovered address must match `voter`

When no signature is provided, the call proceeds (backwards compatible for devnet/testing).

---

## Resolved Gaps

### Gap 1: `apply_proposal` only logs — no real execution callbacks

**Status**: Resolved. `apply_proposal` now applies real internal state changes for all 10 proposal types (governance config, scheduled upgrades, treasury transfers, validator slashing, compliance policies, emergency pause, fee currency registry, fee currency cap, and key rotations). Cross-system effects (consensus, fork manager, oracle, asset registry) are handled by `NodeProposalExecutor` (see "Execution Model" tables above).

### Gap 2: Governance state is in-memory only

**Status**: Resolved. Governance state persists across node restarts via reth-db (MDBX). `Executor` and `balance_source` fields skipped during serialization, rewired after load.

### Gap 3: No ForkManager integration for `ProtocolUpgrade`

**Status**: Resolved. `ProposalExecutor` calls `ForkManager.schedule_governance_upgrade()` for `ProtocolUpgrade` proposals.

### Additional gaps resolved in production hardening:

- **No automatic state machine advancement**: `advance()` method auto-transitions proposals every block.
- **Deposit uses in-memory balances**: `BalanceSource` closure reads real on-chain CALL balances.
- **No authentication on governance RPC**: Optional secp256k1 signature verification on SubmitProposal and Vote.
- **No fee currency registry in RpcState**: `FeeCurrencyRegistry` added to `RpcState`, wired into executor.
- **No `register_validator` wiring**: Genesis + runtime sync into governance from consensus.
- **Economic constants hardcoded**: `proposal_deposit`, `min_self_stake`, `offline_slash_rate_bps`, `asset_registration_fee`, `min_market_cap_usd`, and oracle config are now live fields updateable via `ParameterChange` proposals.

---

## Crate Structure

```
crates/governance/
├── Cargo.toml
└── src/
    └── lib.rs          # GovernanceManager, ProposalType, Vote, errors,
                        # ProposalExecutor trait, BalanceSource, EmergencyPauseState,
                        # ScheduledUpgrade, FeeCurrencyEntry
```

### Dependencies

- `call-primitives` — `Address`, `AssetId`, `Balance`, `ValidatorId`
- `serde`, `serde_json` — serialization for proposals and execution data
- `thiserror` — error types
- `tracing` — logging for proposal lifecycle events

### Downstream Crates

| Crate | Usage |
|---|---|
| `call-rpc` | `GovernanceManager` in `RpcState`, RPC handlers, `NodeProposalExecutor`, `FeeCurrencyRegistry` |
| `call-protocol` | Test suite (`test_governance_flow.rs`), `FeeCurrencyRegistry`, `AssetRegistry` |
| `call-node` | Integration tests, DB persistence wiring, boot sequence, block loop advance |
| `call-storage` | `CallGovernanceState` table definition |
| `call-consensus` | `ValidatorStateManager.remove_validator()` for slashing |
