# How-To: Bridge Operations

The Callchain Bridge (`0x103`) enables cross-chain asset transfers between Callchain and Ethereum/Arbitrum. This guide covers the practical operations for validators, operators, and integrators.

---

## End-to-End Bridge Flow

```
User locks assets on Ethereum Bridge Contract
              |
              v
    BridgeDeposit event emitted
              |
    +---------+---------+
    |                   |
Path A              Path B
(Validator          (Light Client
Multi-Sig)          MPT Proof)
    |                   |
    v                   v
Validator signs      User submits
event hash           header + proofs
    |                   |
    v                   v
Aggregator collects  Light client verifies
14+ signatures       inclusion
    |                   |
    +---------+---------+
              |
              v
    externalDeposit() on Callchain
              |
              v
    Balance credited (optimistic)
              |
              v
    Challenge period (~14 days)
              |
    +---------+---------+
    |                   |
No challenge        Challenge initiated
    |                   |
    v                   v
Auto-finalized    resolveChallenge()
                  fraud proven → rollback
                  fraud not proven → bond forfeit
```

## Deposit Paths

### Path A: Validator Multi-Sig (Default)

Validators attest to Ethereum bridge events using secp256k1 signatures. Minimum 14 signatures required (2/3 of 21 validators).

**Event hash (signed by validators):**
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

**As a validator, submit an external deposit:**

```bash
# RPC call to submit validator-attested deposit
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000103",
      "data": "0x...externalDeposit ABI encoding..."
    }],
    "id": 1
  }'
```

**Requirements:**
- Caller must be a registered validator
- Caller and recipient must pass compliance check
- `sourceTxHash` must not have been processed before
- Asset must be in `allowed_assets` whitelist
- `sourceContract` must be in `authorized_contracts` for the chain
- Amount must not exceed `max_per_tx` or `daily_limit_per_asset`

### Path B: Light Client (No Validator Signatures)

Available with `light-client-bridge` feature. Users submit MPT proofs directly.

> **Production Note:** The `call_lightClientBridgeDeposit` RPC method is disabled on public RPC nodes for security. Direct EVM writes from external callers are not permitted. In production, use a dedicated bridge relayer with internal node access, or fall back to Path A (validator multi-sig).

**RPC endpoint:**
```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_lightClientBridgeDeposit",
    "params": [
      "0x...header_rlp...",
      "0x...tx_proof...",
      "0x...receipt_proof..."
    ],
    "id": 1
  }'
```

**Verification steps:**
1. Header verified via parent-hash chain
2. Tx inclusion verified via MPT proof against `transactions_root`
3. Receipt verified via MPT proof against `receipts_root`
4. Bridge event parsed from receipt logs
5. Consensus finalization checked

---

## Deposit Status Query

After a deposit is submitted, query its status via the bridge precompile view function:

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_call",
    "params": [{
      "to": "0x0000000000000000000000000000000000000103",
      "data": "0x...getChallengeStatus(0x<sourceTxHash>)..."
    }, "latest"],
    "id": 1
  }'
```

**Response fields:**

| Field | Meaning |
|-------|---------|
| `status` | `0` = none, `1` = pending, `2` = successful, `3` = failed, `4` = withdrawn |
| `deadline` | Block height when challenge period ends |
| `bond` | Challenger bond amount (if challenged) |
| `challenger` | Address that initiated the challenge (if any) |

**Other view functions:**

```bash
# Total deposits processed
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_call","params":[{"to":"0x0000000000000000000000000000000000000103","data":"0x...getTotalDeposits()..."},"latest"],"id":1}'

# Total withdrawals processed
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_call","params":[{"to":"0x0000000000000000000000000000000000000103","data":"0x...getTotalWithdrawals()..."},"latest"],"id":1}'
```

## Withdrawals

### Callchain → Ethereum

**Step 1: Call `externalWithdraw` on Callchain (precompile `0x103`):**

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000103",
      "data": "0x...externalWithdraw ABI encoding..."
    }],
    "id": 1
  }'
```

Flow on Callchain:
1. Protocol balance burned
2. `ExternalWithdraw` event emitted with `(targetChain, targetAddress, assetId, amount)`

**Step 2: Validators observe and release on Ethereum**

After the `ExternalWithdraw` event is emitted on Callchain, validators monitor for this event and execute the release on the Ethereum bridge contract:

1. Validators read the event from Callchain block receipts
2. A designated relayer or any validator calls `release()` on the Ethereum bridge contract
3. The Ethereum contract verifies the validator signatures or multisig threshold
4. Target address receives the assets on Ethereum

**Who executes the Ethereum release?**
- In production, a **bridge relayer service** runs alongside the validator set
- The relayer listens for `ExternalWithdraw` events on Callchain
- It constructs and submits the release transaction to Ethereum
- Validators sign attestations; the relayer aggregates and submits them
- Operators must ensure their relayer has ETH for gas on the Ethereum side

---

## Challenge Flow

Anyone can challenge a fraudulent deposit during the challenge period (~14 days).

### Initiate a Challenge

```bash
# Submit challenge with bond
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000103",
      "data": "0x...initiateChallenge ABI encoding..."
    }],
    "id": 1
  }'
```

Requirements:
- Bond: default `1,000` CALL
- Challenge period: default `2,419,200` blocks (~14 days at 250ms block time)
- Proof data: up to 16 KiB

### Resolve a Challenge

After the deadline, anyone can call `resolveChallenge`:

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000103",
      "data": "0x...resolveChallenge ABI encoding..."
    }],
    "id": 1
  }'
```

Outcomes:
- **Fraud proven** → Deposit rolled back, challenger receives bond + reward
- **Fraud not proven** → Challenger forfeits bond

### Fraud Proof Types

**For Light Client path:**

| Proof Type | Description |
|------------|-------------|
| `TxNonExistence` | MPT proof that source tx does not exist in source block's tx trie |
| `ReceiptConflict` | MPT proof that receipt contradicts recorded deposit metadata |

**For Validator path:**
Proof is arbitrary bytes evaluated during `resolveChallenge`. The challenger must demonstrate the original deposit was fraudulent.

---

## Bridge Configuration

Governance can update bridge parameters via proposals:

| Parameter | Default | Description |
|-----------|---------|-------------|
| `challenge_period_blocks` | 2,419,200 | Challenge window in blocks |
| `challenge_bond_amount` | 1,000 CALL | Bond required to challenge |
| `max_per_tx` | Asset-specific | Max deposit per transaction |
| `daily_limit_per_asset` | Asset-specific | Daily deposit limit |
| `min_validator_signatures` | 14 | Min signatures for Path A |
| `max_withdraw_per_period` | Asset-specific | Max withdrawal per period |

---

## Monitoring

| Metric | Source | Alert |
|--------|--------|-------|
| `total_deposits` | EVM storage | Growing = healthy bridge |
| `total_withdrawals` | EVM storage | Growing = healthy withdrawals |
| Pending challenges | EVM storage | Old challenges need resolution |
| Daily limit usage | EVM storage | Approaching limit = throttle |

---

## Emergency Procedures

**Pause bridge (governance only):**
```bash
# Submit GovernancePauseBridge proposal
# After timelock, bridge rejects all new deposits/withdrawals
```

**Resume bridge (governance only):**
```bash
# Submit GovernanceResumeBridge proposal
```
