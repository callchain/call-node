# Callchain 4-Node Devnet E2E Testing Guide

## Overview

This guide covers running a 4-node Callchain devnet and executing end-to-end tests:
- **Transaction tests** — basic transfers, nonce handling, fee deduction
- **Functional tests** — asset registration, governance, oracle, bridge, shielded, EVM, agent
- **Stress tests** — concurrent load, sustained throughput, TPS measurement, consistency validation

---

## Phase 1: Environment Setup

### 1.1 Generate Test Keypairs

Generate 4 deterministic keypairs for the funded accounts:

```bash
for i in 1 2 3 4; do
  ./target/debug/calld wallet generate-keys
done
```

Record the outputs (secretKey + address pairs). These will be the genesis-funded accounts.

### 1.2 Update Genesis

Edit `devnet/genesis.json`. Replace the 4 placeholder addresses with your generated addresses, keeping the same 1M CALL balance per account.

Example format:
```json
{
  "balances": [
    {"address": "0xd107...", "asset_id": 1, "amount": "1000000000000000000000000"},
    ...
  ],
  "validators": [
    {"address": "0xd107...", "pubkey": "0x2e92...", "stake": "100000000000000000000000"},
    ...
  ]
}
```

**Note**: The validator pubkey in genesis should be the Ed25519 key (32 bytes), not the secp256k1 public key. For devnet testing, you can keep the existing placeholder pubkeys since consensus signing is simulated.

### 1.3 Build and Start Devnet

```bash
# Clean previous state (if any)
./devnet/scripts/clean.sh

# Start all 4 nodes
./devnet/scripts/start.sh

# Wait for nodes to sync (~10 seconds)
sleep 10

# Verify health
./devnet/scripts/status.sh
```

**Port mapping**:

| Node | HTTP RPC | WS RPC | P2P    | Metrics |
|------|----------|--------|--------|---------|
| 1    | 5005     | 5006   | 51235  | 9090    |
| 2    | 5007     | 5008   | 51236  | 9091    |
| 3    | 5009     | 5010   | 51237  | 9092    |
| 4    | 5011     | 5012   | 51238  | 9093    |

### 1.4 Verify Genesis Funds

```bash
# Query balance on node1 for account 1
./devnet/scripts/query.sh 1 call_protocolBalance
# params: [1, "0xYOUR_ADDRESS_1"]

# Or use wallet CLI
./target/debug/calld wallet balance \
  --address 0xYOUR_ADDRESS_1 \
  --asset-id 1 \
  --rpc-url http://127.0.0.1:5005
```

---

## Phase 2: Transaction Tests (`e2e_test.py`)

Run the comprehensive transaction test suite:

```bash
python3 devnet/e2e/e2e_test.py --config devnet/e2e/test_config.json
```

### Test Coverage

| Test | Description | RPC Methods |
|------|-------------|-------------|
| `test_balance_query` | Verify genesis balances on all 4 nodes | `call_protocolBalance` |
| `test_single_transfer` | Send 1 CALL from account1 → account2 via node1, verify on node2 | `call_sendPayment` |
| `test_cross_node_transfer` | Send from account1 via node1, verify balance on node3 | `call_sendPayment` + `call_protocolBalance` |
| `test_nonce_sequence` | Send 10 txs with sequential nonces, verify all execute | `call_sendPayment` |
| `test_duplicate_nonce_rejection` | Send 2 txs with same nonce, verify only one executes | `call_sendPayment` |
| `test_fee_deduction` | Send tx, verify sender balance reduced by (amount + fee) | `call_protocolBalance` before/after |
| `test_insufficient_balance` | Attempt to over-spend, verify rejection | `call_sendPayment` |
| `test_mempool_gossip` | Submit tx to node1, verify appears in node2 mempool | `txpool_status` |
| `test_block_subscription` | Subscribe via WS on node1, wait for block | WebSocket `block` subscription |
| `test_multi_asset` | Register new asset, transfer it, query balance | `call_registerAsset` + `call_sendPayment` |

### Key Validation Points

- **Cross-node consistency**: After each transfer, query balance on all 4 nodes. They must match.
- **Nonce handling**: `nonce=N` tx must execute before `nonce=N+1`. Out-of-order rejection.
- **Fee correctness**: `sender_balance_after = sender_balance_before - amount - actual_fee`
- **Mempool propagation**: Tx submitted to node1 must reach node2/3/4 mempool within 5 seconds.

---

## Phase 3: Functional Tests

### 3.1 Asset Registration

```bash
# Register a new asset via governance or direct RPC
curl -X POST http://127.0.0.1:5005 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_registerAsset",
    "params": [{
      "symbol": "TEST",
      "name": "Test Token",
      "decimals": 18,
      "totalSupply": "1000000000000000000000000",
      "issuer": "0xYOUR_ADDRESS_1"
    }],
    "id": 1
  }'
```

Verify: asset appears in `call_assetList` on all nodes.

### 3.2 Governance E2E

Submit a ParameterChange proposal, vote, and execute:

1. Submit proposal via `call_submitProposal`
2. Vote via `call_castVote` (from validator accounts)
3. Advance blocks (wait for review + voting + timelock periods)
4. Execute via `call_executeProposal`
5. Verify parameter changed via `call_protocolConfig`

### 3.3 Oracle

```bash
# Query price (precompile reads from OracleManager)
curl -X POST http://127.0.0.1:5005 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_getPrice",
    "params": [1],
    "id": 1
  }'
```

### 3.4 Bridge (External Deposits)

Requires `light-client-bridge` feature. Test via `call_bridgeDeposit` with mock proof data.

### 3.5 Shielded

Requires running `call-prover` service:

```bash
# Terminal 1: start prover
cargo run -p call-prover -- --dev-setup --listen-addr 127.0.0.1:8550

# Terminal 2: submit shielded tx via RPC
curl -X POST http://127.0.0.1:5005 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "call_shieldedDeposit",
    "params": [...],
    "id": 1
  }'
```

### 3.6 EVM Compatibility

```bash
# Send raw EVM tx
curl -X POST http://127.0.0.1:5005 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_sendRawTransaction",
    "params": ["0x..."],
    "id": 1
  }'
```

Verify via `eth_getTransactionReceipt`.

---

## Phase 4: Stress Tests (`stress_test.py`)

### 4.1 Quick Load Test (1 minute)

```bash
python3 devnet/e2e/stress_test.py \
  --duration 60 \
  --tps 50 \
  --nodes 4 \
  --senders 10 \
  --config devnet/e2e/test_config.json
```

### 4.2 Sustained Load Test (30 minutes)

```bash
python3 devnet/e2e/stress_test.py \
  --duration 1800 \
  --tps 100 \
  --nodes 4 \
  --senders 50 \
  --config devnet/e2e/test_config.json \
  --output stress_report.json
```

### 4.3 Spike Test

Burst to 500 TPS for 10 seconds, then return to baseline:

```bash
python3 devnet/e2e/stress_test.py \
  --duration 120 \
  --tps 10 \
  --spike-tps 500 \
  --spike-duration 10 \
  --nodes 4 \
  --config devnet/e2e/test_config.json
```

### Metrics Collected

| Metric | Source | Target |
|--------|--------|--------|
| Submission TPS | Client-side counter | > 90% of target |
| Confirmation TPS | Blocks × txs/block / time | > 80% of submission |
| Avg latency | Block time − submission time | < 5s |
| P99 latency | 99th percentile | < 15s |
| Mempool backlog | `txpool_status` | < 1000 pending |
| Block time | `server_info` or block headers | ~250ms |
| Cross-node divergence | Balance diff across nodes | 0 |
| Memory growth | Metrics endpoint (`call_node_memory_bytes`) | < 2GB/node |
| Failed tx rate | Rejected / total submitted | < 1% |

---

## Phase 5: Monitoring and Validation

### 5.1 Live Metrics

```bash
# Node 1 Prometheus metrics
curl -s http://127.0.0.1:9090/metrics | grep call_

# Mempool status across nodes
for port in 5005 5007 5009 5011; do
  curl -s -X POST http://127.0.0.1:$port \
    -H "Content-Type: application/json" \
    -d '{"jsonrpc":"2.0","method":"txpool_status","params":[],"id":1}'
  echo
done
```

### 5.2 Log Collection

```bash
# Collect all node logs
./devnet/scripts/logs.sh > logs/devnet_$(date +%Y%m%d_%H%M%S).log

# Search for errors
docker compose -f devnet/docker-compose.yml logs | grep -i "error\|warn\|panic"
```

### 5.3 Final Consistency Check

After stress test completes:

```bash
python3 devnet/e2e/consistency_check.py \
  --nodes 4 \
  --config devnet/e2e/test_config.json
```

Validates:
- All nodes have identical block height (±1)
- All nodes agree on balance for every test account
- No unconfirmed transactions stuck in mempool
- No chain forks detected

---

## Test Configuration

Create `devnet/e2e/test_config.json`:

```json
{
  "nodes": [
    {"rpc": "http://127.0.0.1:5005", "ws": "ws://127.0.0.1:5006", "metrics": "http://127.0.0.1:9090"},
    {"rpc": "http://127.0.0.1:5007", "ws": "ws://127.0.0.1:5008", "metrics": "http://127.0.0.1:9091"},
    {"rpc": "http://127.0.0.1:5009", "ws": "ws://127.0.0.1:5010", "metrics": "http://127.0.0.1:9092"},
    {"rpc": "http://127.0.0.1:5011", "ws": "ws://127.0.0.1:5012", "metrics": "http://127.0.0.1:9093"}
  ],
  "accounts": [
    {"address": "0xADDR1", "secret": "0xSECRET1", "name": "account1"},
    {"address": "0xADDR2", "secret": "0xSECRET2", "name": "account2"},
    {"address": "0xADDR3", "secret": "0xSECRET3", "name": "account3"},
    {"address": "0xADDR4", "secret": "0xSECRET4", "name": "account4"}
  ],
  "chain_id": 8886,
  "asset_id": 1
}
```

---

## Known Limitations

1. **Devnet genesis uses placeholder validator pubkeys** — Ed25519 consensus signing is simulated. For real consensus tests, generate real Ed25519 keypairs and update genesis.
2. **No TLS** — RPC is plain HTTP. Production testing requires TLS termination.
3. **No auth** — RPC endpoints are unauthenticated.
4. **Docker networking** — Nodes run on the same host. Network partition/partition tests require external tooling (e.g., `tc`, Docker network disconnect).
5. **Oracle prices** — `NoOpPriceFetcher` returns no prices. For oracle tests, configure `HttpPriceFetcher` with real endpoints.
6. **Bridge** — Light client bridge requires `light-client-bridge` feature and Ethereum RPC endpoint.
7. **Shielded proving** — Requires separate `call-prover` service running.

---

## Quick Reference

```bash
# Start
./devnet/scripts/start.sh

# Run all E2E tests
python3 devnet/e2e/e2e_test.py --config devnet/e2e/test_config.json

# Run stress test
python3 devnet/e2e/stress_test.py --duration 300 --tps 100 --config devnet/e2e/test_config.json

# Check status
./devnet/scripts/status.sh

# Stop
./devnet/scripts/stop.sh

# Full reset
./devnet/scripts/clean.sh && ./devnet/scripts/start.sh
```
