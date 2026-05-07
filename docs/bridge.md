# Callchain Bridge — Optimistic Cross-Chain Deposits

> This document describes the optimistic verification challenge mechanism for cross-chain deposits via the Bridge precompile (`0x103`). It replaces the legacy `external_bridge.md` design.

## 1. Overview

The Bridge precompile manages cross-chain asset flow between Callchain and external chains (primarily Ethereum). Deposits follow an **optimistic verification** model:

1. Validators submit `externalDeposit` with proof-of-lock on the source chain.
2. The deposit is marked `processed` and recipient balance is credited immediately.
3. A **challenge period** (default 100 blocks) begins.
4. Anyone can `initiateChallenge` during the period by posting a fraud proof and a bond.
5. After the challenge period expires, `resolveChallenge` settles the dispute.

**Supported flows:**

- **External Deposit:** User locks assets on Ethereum -> validators submit attestation -> deposit credited with challenge period protection
- **External Withdrawal:** User burns assets on Callchain -> validators sign release attestation -> assets released on Ethereum
- **Challenge:** Anyone submits fraud proof within challenge period -> dispute settled after period expiry

---

## 2. Architecture

```
+-------------------------------------------------------------+
|  Optimistic Bridge (Callchain <-> Ethereum)                |
|                                                             |
|  External Deposit:                                          |
|    1. Validators observe lock on Ethereum                  |
|    2. Validators submit externalDeposit(txHash, proof)     |
|    3. Deposit marked processed, balance credited           |
|    4. Challenge period starts (~100 blocks)                |
|    5. If challenged: dispute queued for resolution         |
|    6. After period: auto-finalized (or resolved)           |
|                                                             |
|  External Withdrawal:                                       |
|    1. User burns assets on Callchain                        |
|    2. Validators sign release attestation                  |
|    3. Assets released on Ethereum                          |
|                                                             |
|  Challenge Flow:                                            |
|    1. Challenger calls initiateChallenge(txHash, proof)    |
|    2. Bond deducted, deadline set                          |
|    3. After deadline: anyone calls resolveChallenge()      |
|    4. Fraud proof verified -> rollback or bond forfeit     |
|                                                             |
|  +--------------------+  +------------------------------+ |
|  | Bridge State       |  | Bridge Config                | |
|  | - processed[hash]  |  | - challenge_period_blocks    | |
|  | - challenge[hash]  |  | - challenge_bond_amount      | |
|  | - total_deposits   |  | - max_per_tx                 | |
|  | - total_withdrawals|  | - daily_limit_per_asset      | |
|  | - paused           |  | - min_validator_signatures   | |
|  +--------------------+  +------------------------------+ |
+-------------------------------------------------------------+
```

---

## 3. Core Concepts

### 3.1 Optimistic Verification

Instead of waiting for full cryptographic proof before crediting balances, the bridge assumes validator-submitted deposits are honest. The safety guarantee comes from the economic incentive model:

- **Validators** risk slash if they submit fraudulent deposits.
- **Challengers** earn rewards for successfully proving fraud.
- **Challengers** forfeit bond for false challenges.

### 3.2 Challenge Period

A fixed block window during which any `externalDeposit` can be challenged. The period is measured in blocks (not timestamps) to align with EVM block numbering.

- `challenge_period_blocks`: configurable, default `100`.
- `challenge_bond_amount`: fixed bond required to initiate a challenge, default `1000` CALL.

### 3.3 Fraud Proof

The `proof` parameter in `initiateChallenge` contains evidence that the original `externalDeposit` was fraudulent. The exact format depends on the bridge type:

| Bridge Type | Proof Content |
|-------------|---------------|
| Committee (multi-sig) | Merkle proof showing the source tx was never included, or conflicting signatures from the same validator |
| Light Client | SPV proof demonstrating the source tx is on a re-orged fork, or invalid state root |
| Rollup | State transition fraud proof showing the deposit state root is invalid |

The `verify_fraud_proof()` function is bridge-type-specific. For the initial implementation, a stub returning `false` is acceptable. Full cryptographic verification can be added incrementally without changing the challenge state machine.

---

## 4. Storage Layout

All challenge data is stored in `BRIDGE_ADDRESS` (`0x103`) EVM storage.

### 4.1 Existing Slots

| Slot | Purpose |
|------|---------|
| `U256::from(0)` | `total_deposits` |
| `U256::from(1)` | `total_withdrawals` |
| `storage_slot(&[b"paused"])` | bridge paused flag |
| `storage_slot(&[b"processed", &tx_hash])` | processed flag per source tx |

### 4.2 Challenge Slots

```rust
// Challenge state per sourceTxHash
fn slot_bridge_challenge_status(tx_hash: [u8; 32]) -> U256;
fn slot_bridge_challenge_challenger(tx_hash: [u8; 32]) -> U256;      // address
fn slot_bridge_challenge_deadline(tx_hash: [u8; 32]) -> U256;       // uint64 block height
fn slot_bridge_challenge_bond(tx_hash: [u8; 32]) -> U256;           // uint128
fn slot_bridge_challenge_proof_hash(tx_hash: [u8; 32]) -> U256;     // keccak256(proof)
fn slot_bridge_challenge_original_validator(tx_hash: [u8; 32]) -> U256; // address

// Deposit metadata (set at externalDeposit time)
fn slot_bridge_deposit_asset_id(tx_hash: [u8; 32]) -> U256;         // uint64
fn slot_bridge_deposit_recipient(tx_hash: [u8; 32]) -> U256;        // address
fn slot_bridge_deposit_amount(tx_hash: [u8; 32]) -> U256;           // uint128
fn slot_bridge_deposit_block_height(tx_hash: [u8; 32]) -> U256;     // uint64

// Challenge period configuration
fn slot_challenge_period() -> U256;                          // uint64
fn slot_challenge_bond_amount() -> U256;                     // uint128
```

### 4.3 Challenge Status Values

| Value | Meaning |
|-------|---------|
| `0` | No challenge / empty |
| `1` | Pending (initiated, awaiting resolution) |
| `2` | Successful (fraud proven, deposit rolled back) |
| `3` | Failed (false challenge, bond forfeited) |
| `4` | Withdrawn (bond reclaimed after successful challenge) |

---

## 5. ABI Specification

### 5.1 Precompile Functions

| Selector | Function | Input | Output | Gas |
|----------|----------|-------|--------|-----|
| `0xa87e4f2a` | `getTotalDeposits()` | - | `uint128` | 1,500 |
| `0x9c3e6d1b` | `getTotalWithdrawals()` | - | `uint128` | 1,500 |
| `0xdbae8a2a` | `bridgeToEvm(uint64,address,uint128)` | assetId, to, amount | - | 30,000 |
| `0xf0c861e4` | `bridgeToProtocol(uint64,address,uint128)` | assetId, to, amount | - | 30,000 |
| `0x1aba0700` | `externalDeposit(bytes32,uint64,address,uint128)` | sourceTxHash, assetId, recipient, amount | - | 30,000 |
| `0x393da669` | `externalWithdraw(uint64,bytes,uint64,uint128)` | targetChain, targetAddress, assetId, amount | - | 30,000 |
| `0x2689cfc0` | `deposit(uint64,address,uint128,uint64,bytes)` | sourceChain, targetAddress, amount, assetId, proof | - | 30,000 |
| `0x07dee8d0` | `initiateChallenge(bytes32,bytes)` | sourceTxHash, proof | - | 50,000 |
| `0x8a1e5018` | `resolveChallenge(bytes32)` | sourceTxHash | - | 100,000 |
| `0x2a5d97e9` | `getChallengeStatus(bytes32)` | sourceTxHash | `(uint8,uint64,uint128,address)` | 1,500 |
| `0x9c4e5e8b` | `withdrawChallengeBond(bytes32)` | sourceTxHash | - | 5,000 |

### 5.2 Function Details

#### `externalDeposit(bytes32 sourceTxHash, uint64 assetId, address recipient, uint128 amount)`

Called by validators to relay a confirmed deposit from an external chain.

- Validates asset is registered and active.
- Checks `sourceTxHash` has not been processed before.
- Records deposit metadata (assetId, recipient, amount, current block height, original validator).
- Credits recipient balance.
- Increments `total_deposits`.
- Marks `processed[sourceTxHash] = true`.

#### `initiateChallenge(bytes32 sourceTxHash, bytes proof)`

Called by anyone who suspects a fraudulent deposit.

- Validates `sourceTxHash` is processed.
- Validates no existing challenge is pending.
- Validates current block is within challenge period (`current < deposit_block + period`).
- Deducts `challenge_bond` from challenger.
- Stores challenge metadata (challenger, deadline, bond, proof_hash, status=Pending).

#### `resolveChallenge(bytes32 sourceTxHash)`

Called by anyone after the challenge deadline has passed. Gas-intensive because it verifies the fraud proof.

- Validates challenge status is Pending.
- Validates current block >= deadline.
- Calls `verify_fraud_proof(sourceTxHash)`.
- **If proof valid (successful):**
  - Attempt to debit recipient balance (ignored if already spent), decrement `total_deposits`.
  - Clear `processed[sourceTxHash]` (allows re-processing the correct deposit).
  - Credit bond + 10% reward to challenger.
  - Slash original validator (minimal: clears validator status to inactive).
  - Set status = Successful.
- **If proof invalid (failed):**
  - Transfer bond to original validator as reward.
  - Set status = Failed.

#### `withdrawChallengeBond(bytes32 sourceTxHash)`

Called by challenger after a successful challenge to reclaim bond + 10% reward.

- Validates status = Successful.
- Validates caller is the original challenger.
- Credits bond + 10% reward to challenger.
- Sets status = Withdrawn.

---

## 6. State Machine

```
                    externalDeposit(sourceTxHash)
                           |
                           v
                    +--------------+
                    |   Processed  |  <- balance credited
                    |  (block=N)   |  <- challenge period starts
                    +------+-------+
                           |
              +------------+------------+
              |            |            |
              v            |            v (N + period blocks pass)
    initiateChallenge()    |         auto-finalized
              |            |            |
              v            |            v
        +---------+        |       +----------+
        | Pending |        |       | Finalized|
        |(deadline)|       |       | (no undo)|
        +----+----+        |       +----------+
             |             |
             v             |
      resolveChallenge()   |
             |             |
        +----+----+        |
        v         v        |
   Successful   Failed     |
        |         |        |
        v         v        |
   rollback    bond to     |
   + reward    validator   |
        |                  |
        v                  |
  withdrawBond()           |
        |                  |
        v                  |
    Withdrawn              |
```

---

## 7. Security Model

### 7.1 Economic Incentives

| Actor | Honest Behavior | Dishonest Behavior | Penalty |
|-------|----------------|-------------------|---------|
| Validator | Submit valid deposits | Submit fraudulent deposits | Stake slashed |
| Challenger | Challenge only fraudulent deposits | Challenge valid deposits | Bond forfeited |
| Keeper | Call `resolveChallenge` after deadline | Do nothing | No penalty (no reward either) |

### 7.2 Attack Vectors

| Attack | Description | Mitigation |
|--------|-------------|------------|
| **Fraudulent deposit** | Validator submits deposit for non-existent source tx | Challengers can prove fraud and earn reward |
| **Self-challenge** | Validator challenges own deposit to prevent others from challenging | No economic benefit; deposit still rolled back if proven fraud |
| **DOS by challenging** | Attacker spams challenges on all valid deposits | Each challenge requires bond; false challenges forfeit bond to validators |
| **Never resolve** | No one calls `resolveChallenge` after deadline | Anyone can call it permissionlessly; incentivized by gas refund or system reward |
| **Recipient spent funds** | Recipient transfers balance before challenge success | Validator stake is slashed instead of debiting recipient (Option C) |

### 7.3 Rollback Safety

When a challenge succeeds, the implementation attempts to debit the recipient's balance. If the recipient has already spent the funds, the debit is silently ignored (`let _ = debit_bal(...)`), and the deposit total is still decremented.

The validator who submitted the fraudulent deposit is also slashed (current minimal implementation clears their validator status to inactive). This places the economic loss on the dishonest validator while protecting innocent recipients who acted in good faith.

Full debt-tracking against validators (e.g., transferring slashed stake to treasury) can be added incrementally.

---

## 8. Implementation Notes

### 8.1 Block Height Access

Precompiles need access to `block.number` for deadline calculations. This is provided through `StorageRef` via `EvmStorageProvider`, which receives `number` from revm's block context during `CallPrecompiles::run`.

### 8.2 Proof Verification Architecture

`verify_fraud_proof()` is intentionally decoupled from the bridge precompile:

```rust
fn verify_fraud_proof(source_tx_hash: [u8; 32]) -> bool {
    // Option A: Inline for known chain types
    // Option B: Governance-upgradable verifier contract
}
```

For the initial implementation, a stub returning `false` is acceptable. The full cryptographic verification can be added incrementally without changing the challenge state machine.

### 8.3 Linked-List Deadline Index (Future Enhancement)

To enable efficient batch settlement, challenges could be indexed by their deadline block height using an in-storage linked list:

```
challenges_at_height[height] -> head_tx_hash
challenge_next[tx_hash_A] -> tx_hash_B
challenge_next[tx_hash_B] -> tx_hash_C
challenge_next[tx_hash_C] -> 0x00...00 (end)
```

A keeper could then iterate the list at each block height and call `resolveChallenge` for all expired challenges. This is **not yet implemented**; individual `resolveChallenge` calls are used for now.

### 8.4 Governance Parameters

The following parameters should be governance-configurable via the Governance precompile (`0x203`):

- `challenge_period_blocks`
- `challenge_bond_amount`
- `slash_reward_ratio` (percentage of slashed stake awarded to challenger)

---

## 9. Files

| File | Role |
|------|------|
| `crates/bridge/src/precompile.rs` | Bridge precompile implementation (`0x103`) |
| `crates/validator/src/precompile.rs` | Validator stake slash mechanism |
| `crates/precompile/src/storage.rs` | `StorageRef`, `EvmStorageProvider`, storage slot helpers |

---

## 10. TODO

- [x] Implement `initiateChallenge` in `bridge.rs`
- [x] Implement `resolveChallenge` in `bridge.rs`
- [x] Implement `getChallengeStatus` in `bridge.rs`
- [x] Implement `withdrawChallengeBond` in `bridge.rs`
- [x] Add challenge storage slot helpers
- [x] Implement `verify_fraud_proof` stub (full crypto deferred)
- [x] Add validator slash helper (`bridge.rs`)
- [x] Add challenge period tests
- [ ] Add deadline linked-list index for batch settlement
- [ ] Add governance parameter integration (`governance.rs`)
- [ ] Add keeper/bot for batch resolution
- [ ] Implement full cryptographic `verify_fraud_proof`
