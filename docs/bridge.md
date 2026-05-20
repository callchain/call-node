# Callchain Bridge — Cross-Chain Asset Bridge

> This document describes the external cross-chain bridge between Callchain and external chains (Ethereum, Arbitrum) via the Bridge precompile (`0x103`). Internal protocol balance ↔ EVM cross-layer operations are handled by the Switch precompile (`0x207`).

## 1. Overview

The Bridge precompile manages **cross-chain** asset flow. It supports two deposit paths and one withdrawal path:

| Direction | Path | Mechanism | Trust Model |
|-----------|------|-----------|-------------|
| Ethereum → Callchain | Validator Multi-Sig | 14+ validator secp256k1 signatures | Optimistic + challenge period |
| Ethereum → Callchain | Light Client | MPT proof + beacon consensus | Cryptographic (no validators) |
| Callchain → Ethereum | Validator Release | Balance burn + event emission | Validator attestation |

All deposits enter a **challenge period** (~14 days) before finalization. Anyone can challenge a deposit by posting a fraud proof and a bond.

---

## 2. Architecture

```
+-------------------------------------------------------------+
|  Cross-Chain Bridge (Callchain <-> Ethereum/Arbitrum)      |
|                                                             |
|  Deposit Path A: Validator Multi-Sig                        |
|    1. User locks assets on Ethereum Bridge Contract         |
|    2. BridgeDeposit event emitted (txHash, recipient, amt)  |
|    3. Validators observe event and sign with secp256k1      |
|    4. 14+ signatures aggregated by a validator              |
|    5. externalDeposit() called on Callchain (0x103)         |
|    6. Recipient balance credited immediately                |
|    7. Challenge period starts (~2,419,200 blocks ≈ 14d)     |
|    8. If challenged -> resolveChallenge() after deadline    |
|    9. If no challenge -> auto-finalized                     |
|                                                             |
|  Deposit Path B: Light Client (light-client-bridge feature) |
|    1. User locks assets on Ethereum Bridge Contract         |
|    2. User submits: block header + tx MPT proof + receipt   |
|    3. EthLightClient verifies header chain + tx inclusion   |
|    4. Receipt parsed for BridgeDeposit event                |
|    5. Beacon consensus finalization checked                 |
|    6. Same限额/challenge flow as Path A                     |
|                                                             |
|  Withdrawal:                                                |
|    1. User calls externalWithdraw() on Callchain (0x103)    |
|    2. Protocol balance burned, ExternalWithdraw emitted     |
|    3. Validators observe event and release on Ethereum      |
|                                                             |
|  Challenge Flow:                                            |
|    1. Challenger calls initiateChallenge(txHash, proof)     |
|    2. Bond deducted, deadline = current + challenge_period  |
|    3. After deadline: anyone calls resolveChallenge()       |
|    4. Proof verified -> rollback (success) or bond forfeit  |
|                                                             |
|  +--------------------+  +------------------------------+  |
|  | Bridge State       |  | Bridge Config                |  |
|  | - processed[hash]  |  | - challenge_period_blocks    |  |
|  | - challenge[hash]  |  | - challenge_bond_amount      |  |
|  | - total_deposits   |  | - max_per_tx                 |  |
|  | - total_withdrawals|  | - daily_limit_per_asset      |  |
|  | - period_withdrawn |  | - max_withdraw_per_period    |  |
|  | - paused           |  | - min_validator_signatures   |  |
|  | - authorized[chain]|  | - allowed_assets             |  |
|  +--------------------+  +------------------------------+  |
+-------------------------------------------------------------+
```

---

## 3. Deposit Paths

### 3.1 Path A: Validator Multi-Sig

The default mode. Validators attest to Ethereum events using secp256k1 signatures.

**Event Hash (signed by validators):**
```
event_hash = keccak256(
    chain_id ||
    source_tx_hash ||
    source_block_number ||
    sender ||
    recipient ||
    asset_id ||
    amount
)
```

**Signature requirements:**
- Minimum 14 signatures (2/3 of 21 validators)
- Each signature must be from a distinct validator
- Recovered signer must be in the active validator set

**Processing (`externalDeposit`):**
1. Caller must be a registered validator
2. Caller and recipient must pass `check_compliance`
3. `sourceTxHash` must not have been processed before
4. Asset must be in `allowed_assets` whitelist
5. `sourceContract` must be in `authorized_contracts` for the chain
6. Amount must not exceed `max_per_tx`
7. Amount must not exceed `daily_limit_per_asset`
8. Bridge fee deducted (if configured), net amount credited
9. Deposit metadata recorded (asset, recipient, amount, block, validator)
10. Emit `ExternalDeposit(...)` event

### 3.2 Path B: Light Client

Available with `light-client-bridge` feature. No validator signatures needed.

**Verification steps (`process_light_client_deposit_evm`):**
1. `submit_header(header)` — verify parent-hash chain continuity
2. `verify_tx_inclusion(block, tx_hash, tx_proof)` — MPT proof of tx existence
3. `verify_receipt_and_parse_bridge_event(block, receipt_proof)` — MPT proof of receipt log
4. `is_consensus_verified(block)` — block finalized by beacon chain BLS
5. Match parsed event fields (recipient, asset_id, amount) against submission
6. Same限额 checks as Path A

---

## 4. Core Concepts

### 4.1 Optimistic Verification

Deposits are credited immediately upon validator attestation (or light client verification). Safety is guaranteed by the economic challenge model:

- **Validators** risk stake slash if they submit fraudulent deposits
- **Challengers** earn rewards for successfully proving fraud
- **Challengers** forfeit bond for false challenges

### 4.2 Challenge Period

A fixed block window during which any deposit can be challenged.

- `challenge_period_blocks`: configurable, default `2,419,200` (~14 days at 250ms block time)
- `challenge_bond_amount`: fixed bond required to initiate, default `1,000` CALL
- Measured in blocks (not timestamps) to align with EVM block numbering

### 4.3 Fraud Proof

The `proof` parameter in `initiateChallenge` contains evidence of fraud.

**For Light Client path:**

| Proof Type | Description |
|------------|-------------|
| `TxNonExistence` | MPT proof that source tx does not exist in source block's tx trie |
| `ReceiptConflict` | MPT proof that receipt contradicts recorded deposit metadata |

**For Validator path:**
The proof is arbitrary bytes (up to 16 KiB) evaluated during `resolveChallenge`. The challenger must demonstrate the original deposit was fraudulent.

---

## 5. Storage Layout

All data stored in `BRIDGE_ADDRESS` (`0x103`) EVM storage.

### 5.1 Core Slots

| Slot | Purpose |
|------|---------|
| `U256::from(0)` | `total_deposits` |
| `U256::from(1)` | `total_withdrawals` |
| `storage_slot(&[b"paused"])` | bridge paused flag |
| `storage_slot(&[b"processed", &tx_hash])` | processed block height per source tx |

### 5.2 Challenge Slots

```rust
// Challenge state per sourceTxHash
fn slot_bridge_challenge_status(tx_hash) -> U256;       // enum: None/Pending/Successful/Failed/Withdrawn
fn slot_bridge_challenge_challenger(tx_hash) -> U256;   // address
fn slot_bridge_challenge_deadline(tx_hash) -> U256;     // uint64 block height
fn slot_bridge_challenge_bond(tx_hash) -> U256;         // uint128
fn slot_bridge_challenge_proof_hash(tx_hash) -> U256;   // keccak256(proof)
fn slot_bridge_challenge_proof_len(tx_hash) -> U256;    // uint64
// Proof stored in 32-byte chunks:
fn slot_bridge_challenge_proof_chunk(tx_hash, i) -> U256;
fn slot_bridge_challenge_original_validator(tx_hash) -> U256; // address

// Deposit metadata (set at externalDeposit time)
fn slot_bridge_deposit_asset_id(tx_hash) -> U256;       // uint64
fn slot_bridge_deposit_recipient(tx_hash) -> U256;      // address
fn slot_bridge_deposit_amount(tx_hash) -> U256;         // uint128
fn slot_bridge_deposit_block_height(tx_hash) -> U256;   // uint64
```

### 5.3 Limit Slots

```rust
fn slot_bridge_max_per_tx(asset_id) -> U256;
fn slot_bridge_daily_limit(asset_id) -> U256;
fn slot_bridge_daily_used(asset_id, day) -> U256;
fn slot_bridge_max_withdraw_per_period() -> U256;
fn slot_bridge_period_withdrawn(asset_id) -> U256;
fn slot_bridge_current_period_start() -> U256;
fn slot_bridge_withdraw_period() -> U256;
fn slot_bridge_asset_allowed(asset_id) -> U256;
fn slot_bridge_authorized_contract(chain_id, contract) -> U256;
```

### 5.4 Challenge Status Values

| Value | Meaning |
|-------|---------|
| `0` | No challenge / empty |
| `1` | Pending (initiated, awaiting resolution) |
| `2` | Successful (fraud proven, deposit rolled back) |
| `3` | Failed (false challenge, bond forfeited) |
| `4` | Withdrawn (bond reclaimed after resolution) |

---

## 6. ABI Specification

### 6.1 Precompile Functions

| Selector | Function | Input | Output | Gas |
|----------|----------|-------|--------|-----|
| `0xa87e4f2a` | `getTotalDeposits()` | — | `uint128` | 1,500 |
| `0x9c3e6d1b` | `getTotalWithdrawals()` | — | `uint128` | 1,500 |
| `0x5f76` | `externalDeposit(uint64,address,bytes32,uint64,address,uint128)` | sourceChain, sourceContract, sourceTxHash, assetId, recipient, amount | — | 30,000 |
| `0x8f1e` | `externalWithdraw(uint64,bytes,uint64,uint128)` | targetChain, targetAddress, assetId, amount | — | 30,000 |
| `0x07dee8d0` | `initiateChallenge(bytes32,bytes)` | sourceTxHash, proof | — | 50,000 |
| `0x8a1e5018` | `resolveChallenge(bytes32)` | sourceTxHash | — | 150,000 |
| `0x2a5d97e9` | `getChallengeStatus(bytes32)` | sourceTxHash | `(uint64,uint64,uint128,address)` | 1,500 |
| `0x9c4e5e8b` | `withdrawChallengeBond(bytes32)` | sourceTxHash | — | 5,000 |

### 6.2 Function Details

#### `externalDeposit(uint64 sourceChain, address sourceContract, bytes32 sourceTxHash, uint64 assetId, address recipient, uint128 amount)`

Called by validators to relay a confirmed deposit from an external chain.

- Caller must be a registered validator
- Caller and recipient must pass compliance checks
- Validates asset is registered, allowed, and bridge not paused
- Checks `sourceTxHash` has not been processed before
- Checks `sourceContract` is authorized for `sourceChain`
- Enforces `max_per_tx` and `daily_limit_per_asset`
- Deducts bridge fee (if configured), credits net amount to recipient
- Records deposit metadata (assetId, recipient, amount, block height, validator)
- Increments `total_deposits`
- Marks `processed[sourceTxHash] = block_height`
- Emit `ExternalDeposit(...)` event

#### `externalWithdraw(uint64 targetChain, bytes targetAddress, uint64 assetId, uint128 amount)`

Called by users to withdraw protocol balance to an external chain.

- Caller must pass compliance check
- Validates asset is registered and bridge not paused
- Deducts caller protocol balance
- Deducts bridge fee (if configured), fee credited to Bridge address
- Enforces `max_external_withdraw_per_period` rate limit
- Increments `total_withdrawals`
- Emit `ExternalWithdraw(...)` event

#### `initiateChallenge(bytes32 sourceTxHash, bytes proof)`

Called by anyone who suspects a fraudulent deposit.

- Validates `sourceTxHash` was processed (deposit block height > 0)
- Validates no existing challenge is pending
- Validates current block is within challenge period
- Validates proof length (≥32 bytes, ≤16 KiB)
- Deducts `challenge_bond` from challenger balance
- Stores challenge metadata and proof chunks
- Emit `ChallengeInitiated(...)` event

#### `resolveChallenge(bytes32 sourceTxHash)`

Called by anyone after the challenge deadline has passed.

- Validates challenge status is Pending
- Validates current block >= deadline
- Verifies the stored fraud proof
- **If proof valid (successful):**
  - Decrements `total_deposits` by the deposit amount
  - Returns challenge bond to challenger
  - Set status = Successful
- **If proof invalid (failed):**
  - Transfers bond to original validator as reward
  - Set status = Failed
- Cleans up all challenge storage (proof chunks, metadata)
- Emit `ChallengeResolved(...)` event

#### `withdrawChallengeBond(bytes32 sourceTxHash)`

Called by challenger to reclaim bond after resolution.

- Validates status is Successful or Failed
- Validates caller is the original challenger
- Credits bond + reward (if any) to challenger
- Cleans up remaining challenge storage
- Set status = Withdrawn
- Emit `ChallengeBondWithdrawn(...)` event

---

## 7. State Machine

```
                    externalDeposit(sourceTxHash)
                           |
                           v
                    +--------------+
                    |   Credited   |  <- recipient balance +
                    |  (block=N)   |  <- challenge period starts
                    +------+-------+
                           |
              +------------+------------+
              |            |            |
              v            |            v (N + 2,419,200 blocks pass)
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

## 8. Security Model

### 8.1 Economic Incentives

| Actor | Honest Behavior | Dishonest Behavior | Penalty |
|-------|----------------|-------------------|---------|
| Validator | Submit valid deposits, sign withdrawals | Submit fraudulent deposits | Stake at risk, bond reward to challenger |
| Challenger | Challenge only fraudulent deposits | Challenge valid deposits | Bond forfeited to validator |
| Keeper | Call `resolveChallenge` after deadline | Do nothing | No penalty (gas refunded via priority fee) |

### 8.2 Rate Limits

| Limit | Default | Purpose |
|-------|---------|---------|
| `max_per_tx` | 10²¹ | Prevent single oversized deposit |
| `daily_limit_per_asset` | 10²² | Daily inflow cap per asset |
| `max_external_withdraw_per_period` | 5×10²¹ | Blast radius limit if validator keys compromised |
| `withdraw_period_blocks` | 2,419,200 | Withdrawal rate-limit window |

### 8.3 Attack Vectors

| Attack | Description | Mitigation |
|--------|-------------|------------|
| **Fraudulent deposit** | Validator submits deposit for non-existent source tx | Challengers prove fraud via MPT proof and earn reward |
| **Replay attack** | Same `sourceTxHash` processed twice | `processed[txHash]` with retention period (4,838,400 blocks) |
| **DOS by challenging** | Attacker spams challenges on valid deposits | Each challenge requires bond; false challenges forfeit bond |
| **Never resolve** | No one calls `resolveChallenge` after deadline | Anyone can call permissionlessly; gas refunded |
| **Unauthorized contract** | Deposit claimed from unapproved bridge contract | `authorized_contracts` whitelist per chain |
| **Compliance evasion** | Sanctioned address receives bridged funds | `check_compliance` on recipient and validator |

### 8.4 Rollback Safety

When a challenge succeeds, `total_deposits` is decremented by the deposit amount. The recipient's balance is **not** directly debited — if they have already spent the funds, the economic loss is absorbed by the system via the inflated deposit counter. The validator who submitted the fraudulent deposit bears the reputational and financial risk.

---

## 9. Configuration

All parameters are governance-configurable via storage slots:

```rust
pub struct BridgeConfig {
    pub max_per_tx: u128,                        // default: 10²¹
    pub daily_limit_per_asset: u128,             // default: 10²²
    pub eth_min_confirmations: u64,              // default: 12
    pub bridge_fee: u128,                        // default: 0
    pub allowed_assets: Vec<AssetId>,            // default: [1]
    pub signature_timeout_secs: u64,             // default: 300
    pub min_validator_signatures: u64,           // default: 14
    pub challenge_period_blocks: u64,            // default: 2,419,200
    pub withdraw_period_blocks: u64,             // default: 2,419,200
    pub max_external_withdraw_per_period: u128,  // default: 5×10²¹
    pub blocks_per_day: u64,                     // default: 345,600
    pub processed_tx_retention_blocks: u64,      // default: 4,838,400
    pub challenge_bond: u128,                    // default: 1,000
    pub authorized_contracts: HashMap<u64, Vec<Address>>, // default: empty
}
```

---

## 10. Files

| File | Role |
|------|------|
| `crates/bridge/src/precompile.rs` | Bridge precompile implementation (`0x103`) — ABI dispatch, handlers, events |
| `crates/bridge/src/external/deposit.rs` | External deposit logic: validator multi-sig and light client verification |
| `crates/bridge/src/external/withdraw.rs` | Validator signing helper for bridge events |
| `crates/bridge/src/external/types.rs` | Bridge types: `ExternalChain`, `ExternalBridgeOp`, signature verification |
| `crates/bridge/src/withdraw.rs` | EVM balance check helper for withdrawals |
| `crates/precompile/src/storage.rs` | `StorageRef`, `EvmStorageProvider`, storage slot helpers |
| `crates/light-client/` | Ethereum light client for `light-client-bridge` feature |

---

## 11. Relationship to Switch

The Bridge and Switch precompiles have distinct responsibilities:

| | Bridge (0x103) | Switch (0x207) |
|---|---|---|
| **Scope** | Cross-chain (Callchain ↔ Ethereum/Arbitrum) | Same-chain cross-layer (Protocol balance ↔ EVM) |
| **Trust** | Validator multi-sig or light client | Escrow model + nested EVM calls |
| **Challenge period** | Yes (~14 days) | No |
| **Deposit method** | `externalDeposit` | `switchToProtocol` |
| **Withdraw method** | `externalWithdraw` | `switchToEvm` |

Do not confuse Bridge `externalDeposit` with the removed legacy `deposit` method. The legacy `deposit` was an internal protocol-balance transfer that duplicated Switch functionality and has been removed.
