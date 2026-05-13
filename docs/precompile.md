# CallChain Protocol Precompiles

**Infrastructure crate**: `crates/precompile/` (`call-precompile`)
**Domain crates**: `crates/asset/`, `crates/oracle/`, `crates/bridge/`, `crates/governance/`, `crates/validator/`, `crates/compliance/`, `crates/switch/`, `crates/agent/`, `crates/shielded/`

---

## Overview

All protocol-layer functionality is exposed through EVM precompiles at fixed addresses in the range `0x101`–`0x209`. Users interact with these precompiles via standard EVM transactions — no custom transaction format is required. This enables full compatibility with MetaMask, Ethers.js, Foundry, Hardhat, and any standard Ethereum tooling.

### Data Flow

```
+---------------+      +------------------+      +------------------+
|  MetaMask /   |  ->  |  EvmTransaction  |  ->  |  revm            |
|  Solidity     |      |  (RLP, to=0x201) |      |  (precompile)    |
+---------------+      +------------------+      +------------------+
                                                         |
                                                         v
+---------------+      +------------------+      +------------------+
|  EVM storage  |  <-  |  StorageRef      |  <-  |  selector + args |
|  slots        |      |  (safe wrapper)  |      |  ABI decode      |
+---------------+      +------------------+      +------------------+
```

### Address Map

| Address | Name | Functions |
|---------|------|-----------|
| `0x101` | **Oracle** | `getPrice`, `getTWAP`, `isStale`, `submitPrice`, `setTrackedAssets` |
| `0x103` | **Bridge** | `getTotalDeposits`, `getTotalWithdrawals`, `externalBridgeDeposit`, `externalBridgeWithdraw`, `challengeBridgeDeposit` |
| `0x201` | **Asset** | `getBalance`, `getAssetInfo`, `transfer`, `batchTransfer`, `approve`, `transferFrom`, `register`, `mint`, `burn` |
| `0x202` | **Shielded** | `deposit`, `withdraw`, `transfer` |
| `0x203` | **Governance** | `submitProposal`, `vote`, `queue`, `execute`, `emergencyPause`, `emergencyResume`, `getProposalStatus`, `getProposalVotes`, `isPaused`, `getProposalCount` |
| `0x204` | **Validator** | `stake`, `unstake`, `claimUnbonded`, `getValidatorStake`, `getValidatorStatus`, `getValidatorPubkey`, `getUnbondHeight`, `getValidatorByIndex` |
| `0x205` | **Compliance** | `updateCompliance`, `checkCompliance` |
| `0x207` | **Switch** | `switchToEvm`, `switchToProtocol` |
| `0x209` | **Agent** | `register`, `grant`, `revoke` |

> **Deprecated addresses**: `0x102` (Balance, merged into `0x201`), `0x208` (ExternalBridge, merged into `0x103`).

### State Access

Precompiles access EVM storage slots via `StorageRef`, a safe wrapper around revm's journal that decomposes the fat pointer without aliased mutable references. All state (balances, asset metadata, validator stakes, governance proposals, etc.) is stored directly in EVM storage under precompile-specific addresses. No in-memory protocol state structs (`AccountState`, `AssetRegistry`, etc.) are accessed during precompile execution.

```
┌──────────────┐     ┌─────────────────┐     ┌─────────────────────┐
│   MetaMask   │ ──▶ │  EVM tx (to=0x201) │ ──▶ │  revm journal       │
└──────────────┘     └─────────────────┘     └─────────────────────┘
                                                      │
                                          ┌───────────▼────────────┐
                                          │ StorageRef (safe)       │
                                          │  ├─ sload(addr, slot)   │
                                          │  └─ sstore(addr, slot)  │
                                          └───────────┬────────────┘
                                                      │
                                          ┌───────────▼────────────┐
                                          │ EVM storage slots       │
                                          │ (balance, asset_meta,   │
                                          │  validator_stake, etc.) │
                                          └─────────────────────────┘
```

---

## 1. Asset Precompile (`0x201`)

**File**: `crates/asset/src/precompile.rs`

Unified asset operations: queries, transfers, allowances, issuer mint/burn, and registration. Replaces the deprecated `0x102` Balance precompile.

### Solidity Interface

```solidity
interface IProtocolAsset {
    // ── Read ──
    function getBalance(uint64 assetId, address account)
        external view returns (uint128);

    function getAssetInfo(uint64 assetId)
        external view returns (
            bytes32 symbol,
            bytes32 name,
            uint8 decimals,
            address issuer,
            uint128 maxSupply,
            uint8 status
        );

    // ── Transfer ──
    function transfer(uint64 assetId, address to, uint128 amount)
        external;

    function batchTransfer(
        uint64 assetId,
        address[] calldata to,
        uint128[] calldata amounts
    ) external;

    function approve(uint64 assetId, address spender, uint128 amount)
        external;

    function transferFrom(
        uint64 assetId,
        address from,
        address to,
        uint128 amount
    ) external;

    // ── Issuer ──
    function register(
        string calldata symbol,
        string calldata name,
        uint8 decimals,
        uint128 maxSupply
    ) external returns (uint64 assetId);

    function registerErc20(address evmContract)
        external returns (uint64 assetId);

    function mint(uint64 assetId, address to, uint128 amount)
        external; // issuer only

    function burn(uint64 assetId, address from, uint128 amount)
        external; // any holder
}
```

### Behavior

- `transfer`: Deducts from sender's protocol balance, credits recipient. Checks compliance on both parties.
- `batchTransfer`: Executes multiple transfers atomically.
- `approve` / `transferFrom`: Protocol-level allowance system (separate from ERC-20 allowances).
- `register`: Registers a new asset in `AssetStorage` and auto-deploys a `WrappedToken` ERC-20 contract.
- `mint`: Only callable by the asset's registered issuer. Mints on the protocol layer.
- `burn`: Any holder can burn their own balance.

---

## 2. Switch Precompile (`0x207`)

**File**: `crates/switch/src/precompile.rs`

Bidirectional bridge between protocol-layer balances and EVM-layer wrapped ERC-20 tokens.

### Solidity Interface

```solidity
interface IProtocolSwitch {
    // protocol balance -> EVM ERC-20 (mint wrapped token)
    function switchToEvm(uint64 assetId, address to, uint128 amount)
        external;

    // EVM ERC-20 -> protocol balance (burn wrapped token)
    function switchToProtocol(uint64 assetId, address to, uint128 amount)
        external;
}
```

### Behavior

- `switchToEvm`:
  1. Deduct protocol balance from caller
  2. Mint wrapped ERC-20 token on EVM layer
- `switchToProtocol`:
  1. Burn wrapped ERC-20 token on EVM layer
  2. Credit protocol balance to recipient
- Asset ID `1` (native CALL) bridges as native EVM balance (not wrapped ERC-20).

---

## 3. Oracle Precompile (`0x101`)

**File**: `crates/oracle/src/precompile.rs`

Price feed queries and validator price submission.

### Solidity Interface

```solidity
interface IProtocolOracle {
    // ── Read ──
    function getPrice(uint64 assetId)
        external view returns (uint128);

    function getTWAP(uint64 assetId)
        external view returns (uint128);

    function isStale(uint64 assetId)
        external view returns (uint8);

    // ── Write (validator only) ──
    function submitPrice(
        uint64 assetId,
        uint128 price,
        uint64 timestamp,
        uint64 blockNumber
    ) external;

    function setTrackedAssets(uint64[] calldata assetIds)
        external;
}
```

### Behavior

- Read functions query `OracleStorage` for latest price, time-weighted average, and staleness.
- `submitPrice`: Rejects the call unless the caller is a **qualified validator** (staked and active in the current validator set).
- `setTrackedAssets`: Configures which asset IDs the oracle tracks. Callable by authorized callers.

---

## 4. Agent Precompile (`0x209`)

**File**: `crates/agent/src/precompile.rs`

Agent registration and balance management.

### Solidity Interface

```solidity
interface IProtocolAgent {
    function register(
        bytes calldata pubkey,
        string calldata name,
        string calldata url
    ) external returns (uint64 agentId);

    // owner only
    function grant(uint64 agentId, uint64 assetId, uint128 amount)
        external;

    // owner only
    function revoke(uint64 agentId, uint64 assetId)
        external;
}
```

### Behavior

- `register`: Registers a new agent with the caller as owner. Deducts base registration fee.
- `grant`: Owner deducts from their own protocol balance and grants to agent's sub-account.
- `revoke`: Owner revokes agent's balance for a specific asset.

---

## 5. Shielded Precompile (`0x202`)

**File**: `crates/shielded/src/precompile.rs`

Privacy-preserving deposits, withdrawals, and transfers via the Shielded Pool.

### Solidity Interface

```solidity
interface IProtocolShielded {
    function deposit(
        uint64 assetId,
        uint128 amount,
        bytes32 commitment,
        bytes calldata encryptedNote
    ) external;

    function withdraw(
        uint64 assetId,
        address target,
        uint128 amount,
        bytes calldata proof,
        bytes32 nullifier
    ) external;

    function transfer(
        uint64 assetId,
        bytes calldata proof,
        bytes32[] calldata nullifiers,
        bytes32[] calldata commitments,
        bytes[] calldata encryptedNotes
    ) external;
}
```

### Behavior

- `deposit`: Deducts transparent balance, appends commitment to the Merkle tree.
- `withdraw`: Verifies ZK proof and nullifier, credits transparent balance.
- `transfer`: Verifies ZK proof, spends nullifiers, appends new commitments. Groth16 proof verification is compute-intensive.

---

## 6. Validator Precompile (`0x204`)

**File**: `crates/validator/src/precompile.rs`

Validator staking operations.

### Solidity Interface

```solidity
interface IProtocolValidator {
    function stake(bytes32 pubkey, uint128 amount)
        external;

    function unstake(uint64 validatorId)
        external;

    function claimUnbonded(uint64 validatorId)
        external;

    function getValidatorStake(address validator)
        external view returns (uint128 stake);

    function getValidatorStatus(address validator)
        external view returns (uint8 status);

    function getValidatorPubkey(address validator)
        external view returns (bytes32 pubkey);

    function getUnbondHeight(address validator)
        external view returns (uint64 height);

    function getValidatorByIndex(uint64 index)
        external view returns (address validator);
}
```

### Behavior

- `stake`: Deducts CALL from sender's balance, registers validator in `ValidatorStorage`.
- `unstake`: Initiates unstake, moves stake to unbonding queue.
- `claimUnbonded`: Claims matured unbonded stake back to sender's balance.
- Query functions read validator state directly from EVM storage slots.

---

## 7. Governance Precompile (`0x203`)

**File**: `crates/governance/src/precompile.rs`

Governance proposal lifecycle and emergency controls.

### Solidity Interface

```solidity
interface IProtocolGovernance {
    function submitProposal(
        bytes32 title,
        bytes32 description,
        bytes32 dataHash,
        uint8 proposalType
    ) external;

    function vote(uint64 proposalId, uint8 vote)
        external;

    function queue(uint64 proposalId)
        external;

    function execute(uint64 proposalId)
        external;

    // validator only
    function emergencyPause(bytes32 reason)
        external;

    function emergencyResume()
        external;

    function getProposalStatus(uint64 proposalId)
        external view returns (uint8);

    function getProposalVotes(uint64 proposalId)
        external view returns (uint128 votesFor, uint128 votesAgainst, uint128 votesAbstain);

    function isPaused()
        external view returns (uint8);

    function getProposalCount()
        external view returns (uint64);
}
```

### Behavior

- `submitProposal`: Requires `proposal_deposit` CALL (governable, default 10,000 CALL). Creates a new governance proposal stored in EVM storage under `GOVERNANCE_ADDRESS`.
- `vote`: Casts a vote (1=For, 2=Against, 3=Abstain) on an active proposal.
- `queue`: Queues a passed proposal for execution after the timelock.
- `execute`: Executes a queued proposal's payload (requires `current_block >= execution_block`).
- `emergencyPause` / `emergencyResume`: Requires 2/3 validator consensus.
- Query functions read proposal state directly from EVM storage slots.

---

## 8. Bridge Precompile (`0x103`)

**File**: `crates/bridge/src/precompile.rs`

Cross-chain bridging with validator multi-signature attestation.

### Solidity Interface

```solidity
interface IProtocolBridge {
    // ── Read ──
    function getTotalDeposits()
        external view returns (uint256);

    function getTotalWithdrawals()
        external view returns (uint256);

    // ── External cross-chain ──
    function externalBridgeDeposit(
        bytes32 sourceTxHash,
        uint8 sourceChain,
        uint64 sourceBlockNumber,
        bytes calldata externalSender,
        address recipient,
        uint64 assetId,
        uint128 amount,
        bytes calldata validatorSignatures
    ) external;

    function externalBridgeWithdraw(
        uint8 targetChain,
        bytes calldata targetAddress,
        uint64 assetId,
        uint128 amount
    ) external;

    function challengeBridgeDeposit(
        bytes32 sourceTxHash,
        bytes calldata proof
    ) external;
}
```

### Behavior

- `externalBridgeDeposit`: Verifies validator signatures, queues deposit in challenge period, then mints on Callchain.
- `externalBridgeWithdraw`: Burns Callchain assets, queues withdrawal for validator attestation.
- `challengeBridgeDeposit`: Anyone can challenge a fraudulent deposit during the challenge period.

---

## 9. Compliance Precompile (`0x205`)

**File**: `crates/compliance/src/precompile.rs`

Per-asset compliance policy enforcement.

### Solidity Interface

```solidity
interface IProtocolCompliance {
    // issuer only
    function updateCompliance(uint64 assetId, address target, uint8 status)
        external;

    function checkCompliance(uint64 assetId, address target)
        external view returns (uint8);
}
```

### Behavior

- `updateCompliance`: Asset issuer sets address compliance status (0=Clear, 1=UnderReview, 2=Flagged, 3=Restricted).
- `checkCompliance`: Returns the compliance status for an address under an asset's policy.
- `Restricted` status blocks all value-moving operations for that address unconditionally.

---

## Gas Pricing

Gas is computed at two layers:

1. **Fixed base gas**: Deducted at the start of each precompile method via `dispatch::view` / `dispatch::mutate`. This covers decoding, validation, and business logic overhead.
2. **Dynamic storage gas**: Automatically tracked by `EvmStorageProvider` on every `sload`/`sstore` call through `StorageRef`. Warm/cold access and SSTORE refunds are applied per Cancun rules.

| Function | Base Gas | Notes |
|----------|----------|-------|
| `getBalance` | 800 | + warm/cold sload |
| `getAssetInfo` | 1,000 | + multiple sloads |
| `transfer` | 5,000 | + 2 sloads + 2 sstores |
| `batchTransfer` (per recipient) | 5,000 | + sload/sstore per recipient |
| `approve` | 4,000 | + sload + sstore |
| `transferFrom` | 5,500 | + 3 sloads + 2 sstores |
| `register` | 50,000 | + contract deployment gas |
| `registerErc20` | 50,000 | + ERC-20 metadata reads (sloads) |
| `mint` | 6,000 | + sload + sstore + supply update |
| `burn` | 5,000 | + sload + sstore + supply update |
| `switchToEvm` | 8,000 | + EVM contract call |
| `switchToProtocol` | 8,000 | + EVM contract call |
| `getPrice` | 1,000 | + sload |
| `getTWAP` | 1,500 | + multiple sloads |
| `isStale` | 800 | + sload |
| `submitPrice` | 3,000 | + sstore |
| `setTrackedAssets` | 3,000 | + sstores |
| `register` (agent) | 6,000 | + sstore |
| `grant` | 6,000 | + balance transfer |
| `revoke` | 6,000 | + balance transfer |
| `deposit` (shielded) | 50,000 | + Merkle tree update |
| `withdraw` (shielded) | 50,000 | + ZK verification |
| `transfer` (shielded) | 100,000 | + ZK verification |
| `stake` | 20,000 | + balance transfer + validator registration |
| `unstake` | 20,000 | + unbonding queue |
| `claimUnbonded` | 15,000 | + balance transfer |
| `submitProposal` | 10,000 | + deposit transfer + proposal sstores |
| `vote` | 10,000 | + vote sstore |
| `queue` | 15,000 | + status sstore |
| `execute` | 30,000 | + side effects |
| `emergencyPause` | 20,000 | + pause sstore |
| `emergencyResume` | 20,000 | + pause sstore |
| `getTotalDeposits` | 800 | + sload |
| `getTotalWithdrawals` | 800 | + sload |
| `externalBridgeDeposit` | 10,000 | + signature verification |
| `externalBridgeWithdraw` | 8,000 | + burn + queue |
| `challengeBridgeDeposit` | 6,000 | + challenge sstore |
| `updateCompliance` | 6,000 | + sstore |
| `checkCompliance` | 1,000 | + sload |

---

## Registration

Precompile address constants are defined in `crates/precompile/src/lib.rs`:

```rust
pub const ORACLE_ADDRESS: Address = address!("0000000000000000000000000000000000000101");
pub const BRIDGE_ADDRESS: Address = address!("0000000000000000000000000000000000000103");
pub const ASSET_ADDRESS: Address = address!("0000000000000000000000000000000000000201");
pub const SHIELDED_ADDRESS: Address = address!("0000000000000000000000000000000000000202");
pub const GOVERNANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000203");
pub const VALIDATOR_ADDRESS: Address = address!("0000000000000000000000000000000000000204");
pub const COMPLIANCE_ADDRESS: Address = address!("0000000000000000000000000000000000000205");
pub const SWITCH_ADDRESS: Address = address!("0000000000000000000000000000000000000207");
pub const AGENT_ADDRESS: Address = address!("0000000000000000000000000000000000000209");
```

Custom precompiles are registered in `crates/evm/src/executor.rs` via `CallPrecompiles::with_custom()`:

```rust
let precompiles = call_precompile::build_precompiles()
    .with_custom(ORACLE_ADDRESS,     Box::new(OraclePrecompile))
    .with_custom(BRIDGE_ADDRESS,     Box::new(BridgePrecompile))
    .with_custom(ASSET_ADDRESS,      Box::new(AssetPrecompile))
    .with_custom(SHIELDED_ADDRESS,   Box::new(ShieldedPrecompile))
    .with_custom(GOVERNANCE_ADDRESS, Box::new(GovernancePrecompile))
    .with_custom(VALIDATOR_ADDRESS,  Box::new(ValidatorPrecompile))
    .with_custom(COMPLIANCE_ADDRESS, Box::new(CompliancePrecompile))
    .with_custom(SWITCH_ADDRESS,     Box::new(SwitchPrecompile))
    .with_custom(AGENT_ADDRESS,      Box::new(AgentPrecompile));
```

---

## File Map

### Shared infrastructure (`call-precompile`)

| File | Role |
|------|------|
| `crates/precompile/src/lib.rs` | `StatefulPrecompile` trait, address constants, `CallPrecompiles` provider |
| `crates/precompile/src/storage.rs` | `StorageRef` — safe wrapper around revm's journal, `StorageProvider` trait, `EvmStorageProvider`, `HashMapStorageProvider` |
| `crates/precompile/src/dispatch.rs` | `view` / `view_void` / `mutate` / `mutate_void` — unified dispatch helpers |
| `crates/precompile/src/helpers/` | ABI encode/decode utilities, `storage_slot()` helper, slot constants |

### Domain precompiles (one crate per precompile)

| File | Role |
|------|------|
| `crates/asset/src/precompile.rs` | Asset precompile (`0x201`) |
| `crates/asset/src/lib.rs` | `AssetStorage<B>` business logic |
| `crates/switch/src/precompile.rs` | Switch precompile (`0x207`) |
| `crates/switch/src/lib.rs` | `SwitchStorage<B>` business logic |
| `crates/oracle/src/precompile.rs` | Oracle precompile (`0x101`) |
| `crates/oracle/src/lib.rs` | `OracleStorage<B>` business logic |
| `crates/agent/src/precompile.rs` | Agent precompile (`0x209`) |
| `crates/agent/src/lib.rs` | `AgentStorage<B>` business logic |
| `crates/shielded/src/precompile.rs` | Shielded precompile (`0x202`) |
| `crates/shielded/src/lib.rs` | `ShieldedStorage<B>` business logic |
| `crates/validator/src/precompile.rs` | Validator precompile (`0x204`) |
| `crates/validator/src/lib.rs` | `ValidatorStorage<B>` business logic |
| `crates/governance/src/precompile.rs` | Governance precompile (`0x203`) |
| `crates/governance/src/lib.rs` | `GovernanceStorage<B>` business logic |
| `crates/bridge/src/precompile.rs` | Bridge precompile (`0x103`) |
| `crates/bridge/src/lib.rs` | `BridgeStorage<B>` business logic |
| `crates/compliance/src/precompile.rs` | Compliance precompile (`0x205`) |
| `crates/compliance/src/lib.rs` | `ComplianceStorage<B>` business logic |

---

## Architecture Patterns

### `StorageRef` — Safe Storage Access

Precompiles receive a `StorageRef` that provides safe EVM storage access without aliased mutable references. `StorageRef` decomposes revm's journal fat pointer into separate load/store handles.

```rust
pub struct StorageRef {
    pub load: StorageLoadFn,
    pub store: StorageStoreFn,
}

impl StorageRef {
    pub fn sload(address: Address, key: U256) -> Option<U256> { ... }
    pub fn sstore(address: Address, key: U256, value: U256) -> Option<()> { ... }
}
```

**Benefits:**
- No aliased mutability — `StorageRef` decomposes the fat pointer safely.
- Precompile methods receive `&StorageRef` explicitly (no hidden TLS).
- All state changes are subject to revm's Journal rollback on failure.
- No `JournalBackend` or `StorageCtx` TLS indirection needed.

### Unified Dispatch Framework

All domain precompiles use `alloy_sol_types::sol!` to generate ABI types from Solidity interfaces, then route via `dispatch::view` / `dispatch::mutate`:

```rust
impl StatefulPrecompile for AssetPrecompile {
    fn call(&mut self, calldata: &[u8], msg_sender: Address, storage: &StorageRef) -> PrecompileResult {
        let selector = &calldata[..4];
        match selector {
            IProtocolAsset::transfer::SELECTOR => {
                dispatch::mutate_void::<IProtocolAsset::transferCall, _>(
                    calldata, 5000, storage, |call, storage| {
                        let mut store = AssetStorage::new(storage);
                        store.transfer(call.assetId, msg_sender, call.to, call.amount)
                            .map_err(|e| PrecompileError::Other(e.to_string().into()))
                    }
                )
            }
            // ... other arms
        }
    }
}
```

`dispatch::view`, `dispatch::mutate`, and their `_void` variants handle:
- Gas deduction for the base operation cost
- ABI decoding of calldata via `SolCall`
- ABI encoding of return values via `SolValue`
- Checkpoint wrapping for mutating operations (auto-revert on error)
- Gas accounting propagation via `fill_precompile_output`

### Built-in Gas Metering

`EvmStorageProvider` wraps revm's live journal and automatically applies EIP-7623 gas rules:

| Operation | Gas Charged |
|-----------|-------------|
| `sload` (warm) | 100 |
| `sload` (cold) | 2,100 |
| `sstore` (static) | 20,000 |
| `sstore` (cold) | +2,100 |
| `sstore` refund (reset to original) | -4,800 |
| `sstore` refund (clear slot) | -4,800 |
| `tload` | 100 |
| `tstore` | 100 |
| `balance_add/sub/get` | 100 |
| `emit_event` | 375 + 375/topic + 8/byte |

Precompile developers do not manually calculate storage gas — it is deducted automatically at the `StorageProvider` layer. Only the fixed base gas for each operation needs to be specified in `dispatch::view` / `dispatch::mutate`.

---

## Dependency Graph

```
call-precompile (shared infra)
  ├── StorageRef, dispatch, helpers
  └── call-protocol (StorageBackend trait)

Domain crates (each depends on call-precompile + call-protocol)
  ├── call-asset      ──▶ AssetStorage<B>   ──▶ AssetPrecompile (0x201)
  ├── call-switch     ──▶ SwitchStorage<B>  ──▶ SwitchPrecompile (0x207)
  ├── call-oracle     ──▶ OracleStorage<B>  ──▶ OraclePrecompile (0x101)
  ├── call-agent      ──▶ AgentStorage<B>   ──▶ AgentPrecompile (0x209)
  ├── call-shielded   ──▶ ShieldedStorage<B> ──▶ ShieldedPrecompile (0x202)
  ├── call-validator  ──▶ ValidatorStorage<B> ──▶ ValidatorPrecompile (0x204)
  ├── call-governance ──▶ GovernanceStorage<B> ──▶ GovernancePrecompile (0x203)
  ├── call-bridge     ──▶ BridgeStorage<B>  ──▶ BridgePrecompile (0x103)
  └── call-compliance ──▶ ComplianceStorage<B> ──▶ CompliancePrecompile (0x205)

call-evm (registers all precompiles via with_custom())
```

No circular dependencies exist. `call-precompile` and `call-protocol` form the base layer; all domain crates depend on them but not on each other (except for cross-domain reads via `StorageRef::sload` to other precompile addresses).
