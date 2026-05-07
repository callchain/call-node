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
│                                                             │
│  OR: EVM transaction to Governance precompile (0x203)       │
│     submitProposal / vote / queue / execute /               │
│     emergencyPause / emergencyResume                        │
└─────────────────────┬───────────────────────────────────────┘
                      │
                      ▼
              ┌───────────────────┐
              │ GovernanceStorage │ ← stateless, backed by EVM storage slots
              │ (B: StorageBackend) │    reads all state from EVM, writes results back
              └────────┬────────────┘
                       │
                       ├── submit / vote / queue / execute (precompile calls)
                       │
              ┌────────┴──────────┐
              │ GovernanceAdvancer │ ← stateless per-block state machine
              │   auto-advance    │    (Pending → Active → Queued → Executed/Expired)
              └────────┬───────────┘
                       │
              ┌────────┴──────────┐
              │ ProposalExecutor  │ ← wired via node layer for side effects
              │ (trait impl)      │
              └────────┬──────────┘
                       │
      ┌────────────────┼────────────────┬──────────────┬─────────────┬──────────────┐
      ▼                ▼                ▼              ▼             ▼              ▼
 ForkManager      ValidatorPrecompile FeeCurrencyReg   AssetRegistry   OracleStorage  ConsensusParams
 (upgrade)        (slash/remove)      (add/remove/cap) (compliance)    (config)       (params)
                                      FeeParams        GovernanceConfig
                                      (fee params)     (governance params)
```

---

## Precompile Alternative

The **Governance precompile at `0x203`** exposes all governance operations via standard EVM transactions:

| Operation | Function | Gas |
|---|---|---|
| Submit proposal | `submitProposal(uint8,string,string,bytes)` | 50,000 |
| Vote | `vote(uint64,uint8)` | 10,000 |
| Queue | `queue(uint64)` | 15,000 |
| Execute | `execute(uint64)` | 30,000 |
| Emergency pause | `emergencyPause(string)` | 20,000 |
| Emergency resume | `emergencyResume()` | 20,000 |

See [precompile.md](precompile.md) for the full ABI.

## GovernanceStorage

`GovernanceStorage<B: StorageBackend>` is a stateless business-logic wrapper that reads and writes governance state directly from EVM storage. It has no in-memory fields — all proposal data lives in EVM storage slots under `GOVERNANCE_ADDRESS` (`0x203`).

### EVM Storage Layout

| Slot Key | Purpose |
|---|---|
| `slot_gov_proposal_count()` | Total number of proposals |
| `slot_gov_proposal(id, "status")` | Proposal status (0=Pending, 1=Active, 2=Queued, 3=Executed, 4=Defeated, 5=Expired) |
| `slot_gov_proposal(id, "proposer")` | Proposer address |
| `slot_gov_proposal(id, "start_block")` | Voting start block |
| `slot_gov_proposal(id, "end_block")` | Voting end block |
| `slot_gov_proposal(id, "proposal_type")` | Proposal type (0–9) |
| `slot_gov_proposal(id, "votes_for")` | Yes vote tally (u128) |
| `slot_gov_proposal(id, "votes_against")` | No vote tally (u128) |
| `slot_gov_proposal(id, "votes_abstain")` | Abstain vote tally (u128) |
| `slot_gov_proposal(id, "deposit")` | Escrowed deposit amount |
| `slot_gov_proposal(id, "quorum_required")` | Computed quorum for this proposal |
| `slot_gov_proposal(id, "execution_block")` | Block when execution becomes available |
| `slot_gov_voter(id, addr)` | Individual voter's choice (1=Yes, 2=No, 3=Abstain) |
| `slot_gov_config(suffix)` | Governance configuration (timelock, periods, quorum BPS) |
| `slot_gov_paused()` | Chain pause flag |
| `slot_gov_last_submission(addr)` | Rate-limit tracking per proposer |

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
- **CALL balance-weighted**: Voting power equals caller's real on-chain CALL balance + delegated power. Used for `TreasurySpend` (primary), and as max(1, balance) for `ParameterChange`/`ProtocolUpgrade`/`ValidatorSlash`. Balance is read from EVM storage under the asset precompile (`0x201`).
- **Joint (issuer + validator)**: Validators get 1 vote each; asset issuers get `TOTAL_SUPPLY / 10` weight. Used for `ComplianceUpdate`.

### Proposal State Machine

Proposals auto-advance every block via `GovernanceAdvancer::advance()` called in the block production loop:

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
| `min_self_stake` | 1,000,000 CALL | Validator EVM storage (`0x204`) | `validator.min_self_stake` |
| `offline_slash_rate_bps` | 10 (0.1%) | Validator EVM storage (`0x204`) | `validator.offline_slash_rate_bps` |
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

Deposit checks and voting power calculations read CALL balances directly from EVM `ASSET_ADDRESS` (`0x201`) storage via `AssetStorage::read_balance()`:

```rust
let balance = AssetStorage::new(backend).read_balance(CALL_ASSET_ID, voter);
```

For `GovernanceStorage<B>` used in precompiles, the `StorageBackend` (`StorageRef`) provides access to the live EVM journal, so balance checks see the current block's state including any prior transactions. For `GovernanceAdvancer` used in consensus, it reads from committed EVM storage via `InMemoryStateProvider`.

### Execution Model

Governance execution is split across three layers, all reading/writing EVM storage under `GOVERNANCE_ADDRESS` (`0x203`):

1. **Precompile operations** (`GovernanceStorage<B>`) — Handles `submitProposal`, `vote`, `queue`, and `execute` calls from EVM transactions. Reads proposal state from EVM slots, validates rules, and writes results back.

2. **Per-block auto-advance** (`GovernanceAdvancer`) — Called every block in the production loop. Scans all proposals from EVM storage, computes status transitions (Pending→Active, Active→Queued/Defeated, Queued→Executed/Expired), and writes new statuses back.

3. **Cross-system side effects** — Applied by the node layer when proposals reach `Executed`. The node wiring dispatches by proposal type to update consensus params, validator state, fee currency registry, fork manager, asset registry, and oracle config via their respective EVM storage domains.

#### Proposal Execution Effects

When a proposal is executed, the following effects are applied by writing to EVM storage or calling domain storage modules:

| Proposal Type | Effect |
|---|---|
| `ParameterChange` | Writes new parameter value to `slot_gov_config(suffix)` (governance params); node layer routes `consensus.*`, `validator.*`, `oracle.*`, `fee_currency.*`, and `fee.*` prefixes to their respective EVM storage domains |
| `ProtocolUpgrade` | Schedules upgrade via `ForkManager`; records activation block in EVM storage |
| `TreasurySpend` | Transfers CALL from treasury address to recipient via `AssetStorage` |
| `ValidatorSlash` | Removes validator via `ValidatorStorage`; slashes stake in validator EVM storage |
| `ComplianceUpdate` | Updates compliance policy in `ComplianceStorage` |
| `EmergencyPause` | Sets `slot_gov_paused()` to active |
| `FeeCurrencyAdd` | Registers fee currency in `FeeCurrencyRegistry` |
| `FeeCurrencyRemove` | Records grace period in fee-currency EVM storage |
| `FeeCurrencyCap` | Updates stablecoin cap BPS in fee-currency EVM storage |
| `ValidatorKeyRotation` | Updates validator Ed25519 pubkey in validator EVM storage |

#### ParameterChange Prefix Routing

`ParameterChange` proposals use a `param_id` prefix system to route updates to the correct subsystem. The `new_value` field is parsed as JSON. Supported prefixes:

| Prefix | Target Subsystem | Example `param_id` | Example `new_value` |
|---|---|---|---|
| `governance.*` | `slot_gov_config` (governance params) | `governance.proposal_deposit` | `{"proposal_deposit": 5000000000000000000000}` |
| `protocol.*` | `slot_gov_config` (protocol-level params) | `protocol.asset_registration_fee` | `{"asset_registration_fee": 5000000000000000000}` |
| `consensus.*` | `ConsensusParams` EVM storage | `consensus.subset_size` | `{"subset_size": 31}` |
| `validator.*` | `ValidatorStorage` | `validator.min_self_stake` | `{"min_self_stake": 500000000000000000000000}` |
| `oracle.*` | `OracleConfig` / `OracleStorage` | `oracle.outlier_threshold_bps` | `{"outlier_threshold_bps": 300}` |
| `fee_currency.*` | `FeeCurrencyRegistry` | `fee_currency.min_market_cap_usd` | `{"min_market_cap_usd": 50000000}` |
| (no prefix / `fee.*`) | `FeeParams` | `base_fee` | `{"base_fee": 20}` |

All parameter changes take effect immediately upon proposal execution (no restart required).

### Validator Registration

Validators are registered into governance from two sources:

1. **Genesis boot**: In `boot.rs`, genesis validators are registered during `Genesis::apply()`:
   ```rust
   for (i, val) in self.validators.iter().enumerate() {
       gov.register_validator(i as u32, parse_address(&val.address)?);
   }
   ```
2. **Runtime sync**: Every block in the production loop, all validators are read from EVM storage under the validator precompile (`0x204`) and synced idempotently into governance:
   ```rust
   let count = read_validator_count(&provider);
   for id in 1..=count {
       let addr = read_validator_addr(&provider, id);
       gov.register_validator(id as u32, addr);
   }
   ```

### Persistence

Governance state lives entirely in EVM storage under `GOVERNANCE_ADDRESS` (`0x203`). No separate serialization or sidecar persistence is required — the state is saved and loaded automatically as part of EVM state via `CallEvmAccounts`.

| Table | Purpose |
|---|---|
| `CallEvmAccounts` | EVM accounts and storage (includes all governance slots) |

This means:
- Governance state is committed atomically with every block's EVM state root
- On node restart, governance state is restored from EVM state DB snapshot
- No replay reconstruction is needed — all state lives in EVM storage

`GovernanceAdvancer` is stateless; it scans EVM storage every block and has no persisted state of its own.

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
    ├── lib.rs          # GovernanceStorage<B>, ProposalType, Vote, errors,
    ├── types.rs        # Proposal, Vote, GovernanceEvent, ProposalStatus
    ├── config.rs       # GovernanceConfig
    ├── error.rs        # GovernanceError
    ├── precompile.rs   # GovernancePrecompile (selector dispatch)
    └── tests.rs        # GovernanceStorage unit tests (StorageRef)
```

```
crates/node/src/
└── governance_advancer.rs  # GovernanceAdvancer — per-block state machine
```

### Dependencies

- `call-primitives` — `Address`, `AssetId`, `Balance`, `ValidatorId`
- `serde`, `serde_json` — serialization for proposals and execution data
- `thiserror` — error types
- `tracing` — logging for proposal lifecycle events

### Downstream Crates

| Crate | Usage |
|---|---|
| `call-rpc` | `GovernanceStorage` in RPC handlers (read-only queries), `NodeProposalExecutor` |
| `call-protocol` | Test suite (`test_governance_flow.rs`) |
| `call-node` | `GovernanceAdvancer`, integration tests, DB persistence wiring, boot sequence, block loop advance |
| `call-consensus` | `ValidatorStorage.remove_validator()` for slashing |
