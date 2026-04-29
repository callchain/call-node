# CallChain Protocol Precompiles

**Crate**: `crates/precompiles/` (`call-precompiles`)

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
|  AccountState |  <-  |  Rust handler    |  <-  |  selector + args |
|  ShieldedState|      |  (protocol state)|      |  ABI decode      |
+---------------+      +------------------+      +------------------+
```

### Address Map

| Address | Name | Functions |
|---------|------|-----------|
| `0x101` | **Oracle** | `getPrice`, `getTWAP`, `isStale`, `submitPrice` |
| `0x103` | **Bridge** | `getTotalDeposits`, `getTotalWithdrawals`, `externalBridgeDeposit`, `externalBridgeWithdraw`, `challengeBridgeDeposit` |
| `0x201` | **Asset** | `getBalance`, `getAssetInfo`, `transfer`, `batchTransfer`, `approve`, `transferFrom`, `register`, `mint`, `burn` |
| `0x202` | **Shielded** | `shieldedDeposit`, `shieldedWithdraw`, `shieldedTransfer` |
| `0x203` | **Governance** | `submitProposal`, `vote`, `queue`, `execute`, `emergencyPause`, `emergencyResume` |
| `0x204` | **Validator** | `stake`, `unstake`, `claimUnbonded` |
| `0x205` | **Compliance** | `updateCompliance`, `checkCompliance` |
| `0x207` | **Switch** | `switchToEvm`, `switchToProtocol` |
| `0x209` | **Agent** | `registerAgent`, `grantAgentBalance`, `revokeAgentBalance` |

> **Deprecated addresses**: `0x102` (Balance, merged into `0x201`), `0x208` (ExternalBridge, merged into `0x103`).

### State Access

Precompiles access native protocol state (`AccountState`, `AssetRegistry`, `ComplianceEngine`, etc.) through a thread-local `StateHookGuard` injected at the start of `Block::execute()`. The guard provides raw pointer references to all protocol state and is automatically cleared when execution completes.

---

## 1. Asset Precompile (`0x201`)

**File**: `crates/precompiles/src/asset.rs`

Unified asset operations: queries, transfers, allowances, and issuer mint/burn. Replaces the deprecated `0x102` Balance precompile.

### Solidity Interface

```solidity
interface IProtocolAsset {
    // ── Read ──
    function getBalance(uint64 assetId, address account)
        external view returns (uint128);

    function getAssetInfo(uint64 assetId)
        external view returns (
            string memory symbol,
            string memory name,
            uint8 decimals,
            address issuer,
            address erc20Address
        );

    // ── Transfer ──
    function transfer(uint64 assetId, address to, uint128 amount)
        external returns (bool);

    function batchTransfer(
        uint64 assetId,
        address[] calldata to,
        uint128[] calldata amounts
    ) external returns (bool);

    function approve(uint64 assetId, address spender, uint128 amount)
        external returns (bool);

    function transferFrom(
        uint64 assetId,
        address from,
        address to,
        uint128 amount
    ) external returns (bool);

    // ── Issuer ──
    function register(
        string calldata symbol,
        string calldata name,
        uint8 decimals,
        uint128 maxSupply
    ) external returns (uint64 assetId, address erc20Address);

    function mint(uint64 assetId, address to, uint128 amount)
        external returns (bool); // issuer only

    function burn(uint64 assetId, address from, uint128 amount)
        external returns (bool); // any holder
}
```

### Behavior

- `transfer`: Deducts from sender's protocol balance, credits recipient. Checks compliance on both parties.
- `batchTransfer`: Executes multiple transfers atomically. Gas = 5,000 per recipient.
- `approve` / `transferFrom`: Protocol-level allowance system (separate from ERC-20 allowances).
- `register`: Registers a new asset in `AssetRegistry` and auto-deploys a `WrappedToken` ERC-20 contract via `EvmExecutor::deploy_erc20_template`.
- `mint`: Only callable by the asset's registered issuer.
- `burn`: Any holder can burn their own balance (or a balance they have allowance over via `transferFrom` semantics).

---

## 2. Switch Precompile (`0x207`)

**File**: `crates/precompiles/src/switch.rs`

Bidirectional bridge between protocol-layer balances and EVM-layer wrapped ERC-20 tokens.

### Solidity Interface

```solidity
interface IProtocolSwitch {
    // protocol balance -> EVM ERC-20 (mint wrapped token)
    function switchToEvm(uint64 assetId, address to, uint128 amount)
        external returns (bool);

    // EVM ERC-20 -> protocol balance (burn wrapped token)
    function switchToProtocol(uint64 assetId, address to, uint128 amount)
        external returns (bool);
}
```

### Behavior

- `switchToEvm`:
  1. Deduct protocol balance from caller
  2. Mint wrapped ERC-20 token on EVM layer (via `evm_call_mint`)
  3. Update `AssetRegistry.evm_supply`
- `switchToProtocol`:
  1. Burn wrapped ERC-20 token on EVM layer (via `evm_call_burn`)
  2. Credit protocol balance to recipient
  3. Update `AssetRegistry.evm_supply`
- Asset ID `0` (virtual USD) is rejected on both directions.
- Asset ID `1` (native CALL) bridges as native EVM balance (not wrapped ERC-20).

---

## 3. Oracle Precompile (`0x101`)

**File**: `crates/precompiles/src/oracle.rs`

Price feed queries and validator price submission.

### Solidity Interface

```solidity
interface IProtocolOracle {
    // ── Read ──
    function getPrice(uint64 assetId)
        external view returns (uint128);

    function getTWAP(uint64 assetId, uint64 currentTimestamp)
        external view returns (uint128);

    function isStale(uint64 assetId, uint64 currentTimestamp)
        external view returns (bool);

    // ── Write (validator only) ──
    function submitPrice(
        uint64 assetId,
        uint128 price,
        uint64 blockNumber,
        uint64 timestamp,
        bytes calldata signature,
        bytes[] calldata sources
    ) external returns (bool);
}
```

### Behavior

- Read functions query the `OracleManager` for latest price, time-weighted average, and staleness.
- `submitPrice`: Verifies the caller is a registered validator, then submits the price to `OracleManager::submit_price()`.

---

## 4. Agent Precompile (`0x209`)

**File**: `crates/precompiles/src/agent.rs`

Agent registration and balance management.

### Solidity Interface

```solidity
interface IProtocolAgent {
    function registerAgent(
        bytes calldata pubkey,
        string calldata name,
        string calldata url
    ) external returns (uint64 agentId);

    // owner only
    function grantAgentBalance(uint64 agentId, uint64 assetId, uint128 amount)
        external returns (bool);

    // owner only
    function revokeAgentBalance(uint64 agentId, uint64 assetId)
        external returns (bool);
}
```

### Behavior

- `registerAgent`: Registers a new agent with the caller as owner. Deducts base registration fee.
- `grantAgentBalance`: Owner deducts from their own protocol balance and grants to agent's sub-account.
- `revokeAgentBalance`: Owner revokes agent's balance for a specific asset.

---

## 5. Shielded Precompile (`0x202`)

**File**: `crates/precompiles/src/shielded.rs`

Privacy-preserving deposits, withdrawals, and transfers via the Shielded Pool.

### Solidity Interface

```solidity
interface IProtocolShielded {
    function shieldedDeposit(
        uint64 assetId,
        uint128 amount,
        bytes32 commitment,
        bytes calldata encryptedNote
    ) external returns (bool);

    function shieldedWithdraw(
        uint64 assetId,
        address target,
        uint128 amount,
        bytes calldata proof,
        bytes32 nullifier
    ) external returns (bool);

    function shieldedTransfer(
        uint64 assetId,
        bytes calldata proof,
        bytes32[] calldata nullifiers,
        bytes32[] calldata commitments,
        bytes[] calldata encryptedNotes
    ) external returns (bool);
}
```

### Behavior

- `shieldedDeposit`: Deducts transparent balance, appends commitment to the Merkle tree.
- `shieldedWithdraw`: Verifies ZK proof and nullifier, credits transparent balance.
- `shieldedTransfer`: Verifies ZK proof, spends nullifiers, appends new commitments. Groth16 proof verification is compute-intensive.

---

## 6. Validator Precompile (`0x204`)

**File**: `crates/precompiles/src/validator.rs`

Validator staking operations.

### Solidity Interface

```solidity
interface IProtocolValidator {
    function stake(bytes32 ed25519Pubkey, uint128 amount)
        external returns (bool);

    function unstake(uint32 validatorId)
        external returns (bool);

    function claimUnbonded(uint32 validatorId)
        external returns (bool);
}
```

### Behavior

- `stake`: Deducts CALL from sender's protocol balance, registers validator in `ValidatorStateManager`.
- `unstake`: Initiates unstake, moves stake to unbonding queue.
- `claimUnbonded`: Claims matured unbonded stake back to sender's balance.

---

## 7. Governance Precompile (`0x203`)

**File**: `crates/precompiles/src/governance.rs`

Governance proposal lifecycle and emergency controls.

### Solidity Interface

```solidity
interface IProtocolGovernance {
    function submitProposal(
        uint8 proposalType,
        string calldata title,
        string calldata description,
        bytes calldata executionData
    ) external returns (uint64 proposalId);

    function vote(uint64 proposalId, uint8 vote)
        external returns (bool);

    function queue(uint64 proposalId)
        external returns (bool);

    function execute(uint64 proposalId)
        external returns (bool);

    // validator only
    function emergencyPause(string calldata reason)
        external returns (bool);

    function emergencyResume()
        external returns (bool);
}
```

### Behavior

- `submitProposal`: Requires 10,000 CALL deposit. Creates a new governance proposal.
- `vote`: Casts a vote (For/Against/Abstain) on an active proposal.
- `queue`: Queues a passed proposal for execution after the timelock.
- `execute`: Executes a queued proposal's payload.
- `emergencyPause` / `emergencyResume`: Requires 2/3 validator consensus.

---

## 8. Bridge Precompile (`0x103`)

**File**: `crates/precompiles/src/bridge.rs`

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
    ) external returns (bool);

    function externalBridgeWithdraw(
        uint8 targetChain,
        bytes calldata targetAddress,
        uint64 assetId,
        uint128 amount
    ) external returns (bool);

    function challengeBridgeDeposit(
        bytes32 sourceTxHash,
        bytes calldata proof
    ) external returns (bool);
}
```

### Behavior

- `externalBridgeDeposit`: Verifies 14-of-21 validator signatures, queues deposit in challenge period (~7 days), then mints on Callchain.
- `externalBridgeWithdraw`: Burns Callchain assets, queues withdrawal for validator attestation.
- `challengeBridgeDeposit`: Anyone can challenge a fraudulent deposit during the challenge period.

---

## 9. Compliance Precompile (`0x205`)

**File**: `crates/precompiles/src/compliance.rs`

Per-asset compliance policy enforcement.

### Solidity Interface

```solidity
interface IProtocolCompliance {
    // issuer only
    function updateCompliance(uint64 assetId, address target, uint8 status)
        external returns (bool);

    function checkCompliance(uint64 assetId, address target)
        external view returns (uint8);
}
```

### Behavior

- `updateCompliance`: Asset issuer sets address compliance status (`Clear`/`UnderReview`/`Flagged`/`Restricted`).
- `checkCompliance`: Returns the compliance status for an address under an asset's policy.
- `Restricted` status blocks all value-moving operations for that address unconditionally.

---

## Gas Pricing

| Function | Gas | EVM Equivalent |
|----------|-----|----------------|
| `getBalance` | 800 | ERC-20 `balanceOf` ~2,100 |
| `getAssetInfo` | 1,000 | Read query |
| `transfer` | 5,000 | ERC-20 `transfer` ~25,000 |
| `batchTransfer` (per recipient) | 5,000 | Loop ERC-20 ~25,000 each |
| `approve` | 4,000 | ERC-20 `approve` ~20,000 |
| `transferFrom` | 5,500 | ERC-20 `transferFrom` ~28,000 |
| `register` | 50,000 | Contract deployment |
| `mint` | 6,000 | ERC-20 `mint` ~35,000 |
| `burn` | 5,000 | ERC-20 `burn` ~25,000 |
| `switchToEvm` | 8,000 | Protocol->EVM switch |
| `switchToProtocol` | 8,000 | EVM->Protocol switch |
| `getPrice` | 1,000 | Read query |
| `getTWAP` | 1,500 | Read query |
| `isStale` | 800 | Read query |
| `submitPrice` | 3,000 | Oracle submission |
| `registerAgent` | 6,000 | Agent registration |
| `grantAgentBalance` | 6,000 | Balance grant |
| `revokeAgentBalance` | 6,000 | Balance revoke |
| `shieldedDeposit` | 50,000 | Not possible in Solidity |
| `shieldedWithdraw` | 50,000 | Not possible in Solidity |
| `shieldedTransfer` | 100,000 | Not possible in Solidity |
| `stake` | 20,000 | Not possible in Solidity |
| `unstake` | 20,000 | Not possible in Solidity |
| `claimUnbonded` | 15,000 | Not possible in Solidity |
| `submitProposal` | 10,000 | Governor Bravo ~80,000 |
| `vote` | 10,000 | Governor Bravo ~50,000 |
| `queue` | 15,000 | Governance queue |
| `execute` | 30,000 | Governance execute |
| `emergencyPause` | 20,000 | Emergency pause |
| `emergencyResume` | 20,000 | Emergency resume |
| `getTotalDeposits` | 800 | Read query |
| `getTotalWithdrawals` | 800 | Read query |
| `externalBridgeDeposit` | 10,000 | Multi-sig verification |
| `externalBridgeWithdraw` | 8,000 | Cross-chain withdrawal |
| `challengeBridgeDeposit` | 6,000 | Fraud challenge |
| `updateCompliance` | 6,000 | Compliance update |
| `checkCompliance` | 1,000 | Read query |

---

## Registration

All precompiles are registered in `build_precompiles_for_spec()` in `crates/precompiles/src/lib.rs`:

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

---

## File Map

| File | Role |
|------|------|
| `crates/precompiles/src/lib.rs` | Precompile registration, address constants |
| `crates/precompiles/src/state_hook.rs` | Thread-local state sharing (`StateHookGuard`) |
| `crates/precompiles/src/asset.rs` | Asset precompile (`0x201`) |
| `crates/precompiles/src/switch.rs` | Switch precompile (`0x207`) |
| `crates/precompiles/src/oracle.rs` | Oracle precompile (`0x101`) |
| `crates/precompiles/src/agent.rs` | Agent precompile (`0x209`) |
| `crates/precompiles/src/shielded.rs` | Shielded precompile (`0x202`) |
| `crates/precompiles/src/validator.rs` | Validator precompile (`0x204`) |
| `crates/precompiles/src/governance.rs` | Governance precompile (`0x203`) |
| `crates/precompiles/src/bridge.rs` | Bridge precompile (`0x103`) |
| `crates/precompiles/src/compliance.rs` | Compliance precompile (`0x205`) |
