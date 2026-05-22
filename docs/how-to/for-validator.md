# How-To: For Validators

This guide covers validator-specific operations: staking, rewards, slashing, key management, oracle price submission, and bridge attestation.

---

## Table of Contents

- [Staking & Unstaking](#staking--unstaking)
- [Rewards](#rewards)
- [Slashing](#slashing)
- [Key Rotation](#key-rotation)
- [Oracle Price Submission](#oracle-price-submission)
- [Bridge Validator Operations](#bridge-validator-operations)

---

## Staking & Unstaking

Validators participate in consensus by staking CALL tokens via precompile `0x204`.

### Become a Validator

```bash
# Stake minimum self-stake (1,000,000 CALL = 1e18 wei units)
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000204",
      "data": "0x...stake ABI encoding...",
      "value": "0x0de0b6b3a7640000"
    }],
    "id": 1
  }'
```

**Requirements:**
- Minimum self-stake: `1,000,000` CALL (18 decimals)
- Validator Ed25519 consensus key must be registered
- Address must pass compliance check

**Check your stake:**
```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_call",
    "params": [{
      "to": "0x0000000000000000000000000000000000000204",
      "data": "0x...getValidatorStake(0x<your_addr>)..."
    }, "latest"],
    "id": 1
  }'
```

### Unstake

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000204",
      "data": "0x...unstake ABI encoding..."
    }],
    "id": 1
  }'
```

**Safety floor check:** Unstake is rejected if the post-operation qualified validator count would drop below the safety threshold (default: 28 validators).

### Claim Unbonded Stake

After the unbonding period (testnet: ~8.4 hours; mainnet: ~84 hours max), anyone may call `claimUnbonded` to return stake to the original staker:

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000204",
      "data": "0x...claimUnbonded ABI encoding..."
    }],
    "id": 1
  }'
```

See [`docs/validator_staking.md`](../validator_staking.md) for full parameter tables and mechanism details.

---

## Rewards

### Oracle Rewards

Validators who submit prices within the acceptable deviation band receive oracle rewards added directly to their staked balance.

**How rewards work:**
- Block producer aggregates valid price submissions
- Validators whose price is within the median band receive proportional reward
- Reward is added to the validator's EVM stake (compounding)

**Check your validator status:**
```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_call",
    "params": [{
      "to": "0x0000000000000000000000000000000000000204",
      "data": "0x...getValidatorStatus(0x<your_addr>)..."
    }, "latest"],
    "id": 1
  }'
```

**Status codes:**
- `0` = inactive
- `1` = active
- `2` = unbonding

---

## Slashing

### Slashable Offenses

| Offense | Penalty | Result |
|---------|---------|--------|
| Double-sign | 100% of stake | Removed from active set immediately |
| Offline (missed rounds) | 0.1% per round | Stake reduced; removed if stake hits 0 |
| Oracle outlier (>5% from median) | 0.1% of stake | Tracked; repeated offenses accumulate |

### Monitoring for Slashes

Watch your validator's stake in logs or via RPC:

```bash
# Query stake via RPC
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_call",
    "params": [{
      "to": "0x0000000000000000000000000000000000000204",
      "data": "0x...getValidatorStake(0x<your_addr>)..."
    }, "latest"],
    "id": 1
  }'
```

**Prometheus metric:**
```
validator_stake{validator_id="N"}
```

Alert if stake drops unexpectedly:
```yaml
- alert: ValidatorStakeDrop
  expr: validator_stake < validator_stake offset 1h * 0.99
  for: 1m
  labels:
    severity: warning
  annotations:
    summary: "Validator {{ $labels.validator_id }} stake dropped"
```

---

## Key Rotation

Validators may rotate their **Ed25519 consensus public key** via on-chain governance (proposal type 9). This does **not** require unstaking.

> **Note:** Only the Ed25519 consensus key is rotated. The BLS12-381 aggregate vote key (generated at boot) and the secp256k1 EVM signing key are **not** covered.

### Submit Key Rotation Proposal

```bash
# Use governance precompile 0x203 or RPC call_governanceSubmitProposal
# Execution data format (96 bytes):
#   bytes 24-32: uint64 validator_id (big-endian)
#   bytes 32-64: old Ed25519 pubkey (32 bytes)
#   bytes 64-96: new Ed25519 pubkey (32 bytes)
```

### Operational Checklist

- [ ] New private key is backed up securely (HSM / keyring / Vault)
- [ ] New public key has been cross-checked off-chain
- [ ] Rotation proposal has passed voting and timelock
- [ ] Node config / signer has been updated to new key **before** execution block
- [ ] Old key material has been securely destroyed (if compromised)

See [`docs/validator_key_rotation.md`](../validator_key_rotation.md) for the full protocol.

---

## Oracle Price Submission

Your node automatically participates in the decentralized price oracle. No manual intervention is required during normal operation.

**How it works:**
1. Block producer broadcasts `OraclePriceRequest` on P2P channel 4 at each oracle update interval (every 1,000 blocks)
2. Your validator fetches prices from configured sources (HTTP / local)
3. Signs an `OraclePriceSubmission` with Ed25519: `(validator_id, pair, price, block_number, timestamp)`
4. Submits via P2P to the block producer
5. Block producer aggregates submissions, computes median, detects outliers, and writes result to EVM storage via precompile `0x101`

**Operator responsibilities:**
- Ensure your node has outbound internet access to price data sources (Binance, Coinbase, etc.)
- Monitor for outlier flags: prices deviating > 5% from median will mark your validator as an outlier
- Repeated outlier submissions may lead to slashing (0.1% of stake per offense)

**Check current tracked pairs:**
```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_oracleTrackedPairs","id":1}'
```

**Check latest price:**
```bash
# CALL/USD (pair base=1, quote=0)
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_oracleGetPrice","params":[1,0],"id":1}'
```

Prices use **6 decimal places** (e.g. `$2.00` = `2_000_000`).

---

## Bridge Validator Operations

As a validator, you participate in the cross-chain bridge by attesting to Ethereum deposit events.

### Deposit Attestation Flow

1. Monitor Ethereum bridge contract for `BridgeDeposit` events (via Ethereum RPC)
2. When an event is observed, compute the event hash:
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
3. Sign the event hash with your validator **secp256k1** key
4. Submit the signed attestation to a designated aggregator validator
5. Once 14+ distinct validator signatures are collected (2/3 of 21), the aggregator calls `externalDeposit()` on precompile `0x103`

### Operator Responsibilities

- Ensure your node has access to an Ethereum RPC endpoint (mainnet or Arbitrum)
- Monitor bridge metrics: pending deposits, daily limit usage, challenge status
- Be prepared to respond to challenges during the challenge period (~14 days)

### Check Bridge Status

```bash
# Total deposits
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_call","params":[{"to":"0x0000000000000000000000000000000000000103","data":"0x..."},"latest"],"id":1}'

# Check if bridge is paused
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_bridgeIsPaused","id":1}'
```

### Submit External Deposit (Aggregator Only)

```bash
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendTransaction",
    "params": [{
      "to": "0x0000000000000000000000000000000000000103",
      "data": "0x...externalDeposit ABI with signatures..."
    }],
    "id": 1
  }'
```

**Key requirements:**
- Caller must be a registered validator
- `sourceTxHash` must not have been processed before
- Asset must be in the `allowed_assets` whitelist
- Amount must not exceed `max_per_tx` or `daily_limit_per_asset`
- Minimum 14 signatures from distinct validators

---

**See also:**
- [`docs/validator_staking.md`](../validator_staking.md) — Full staking mechanics and parameters
- [`docs/validator_key_rotation.md`](../validator_key_rotation.md) — Key rotation protocol
- [`docs/how-to/for-operator.md`](for-operator.md) — Node deployment, monitoring, and backup
- [`docs/how-to/for-bridge.md`](for-bridge.md) — Bridge user/integrator guide
