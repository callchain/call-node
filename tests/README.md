# Callchain E2E & Integration Tests

This directory contains all end-to-end and integration tests for Callchain.

## Test Scripts

Run these scripts to execute the full test suites:

| Script | What it tests | Time |
|--------|---------------|------|
| `./run_inmemory_e2e.sh` | In-memory Rust E2E (no Docker, deterministic) | ~30s |
| `./run_single_node_e2e.sh` | Single-node devnet via Docker + Python RPC tests | ~2min |
| `./run_e2e.sh` | Full 6-node validator devnet via Docker + Python RPC tests | ~3min |
| `./run_network_integration.sh` | Real commonware-p2p with localhost TCP sockets | ~30s |

## In-Memory Rust E2E

Fastest tests using the `TestNode` harness — no networking, fully deterministic.

```bash
# Run all in-memory e2e tests
./run_inmemory_e2e.sh

# Or run individual test suites
cargo test -p call-node --test test_governance_e2e -- --nocapture
cargo test -p call-node --test test_shielded_e2e -- --nocapture
cargo test -p call-node --test test_bridge_e2e -- --nocapture
cargo test -p call-node --test test_light_client_e2e -- --nocapture
cargo test -p call-node --test test_websocket_e2e -- --nocapture
```

## Live Devnet E2E (Python)

These tests run against a live network via JSON-RPC. They require Python 3 and test accounts (`accounts.json`).

### Single-Node Devnet

```bash
# Full orchestration: start node, run tests, prompt to stop
./run_single_node_e2e.sh

# Or run manually:
../devnet/single/scripts/start.sh
cd tests && python3 test_basic.py && python3 test_stress.py && python3 test_transactions.py
```

### 6-Node Validator Devnet

```bash
# Full orchestration: start 4 validators + 2 full nodes, run tests, prompt to stop
./run_e2e.sh

# Or run manually:
../devnet/scripts/start.sh
cd tests && python3 test_basic.py && python3 test_stress.py && python3 test_transactions.py
```

### Python Test Files

| File | Tests |
|------|-------|
| `test_basic.py` | RPC layer, balance queries, tx submission, mempool propagation |
| `test_stress.py` | Throughput, latency, consistency under concurrent load |
| `test_transactions.py` | Asset registration, governance, shielded, oracle, bridge, compliance tx |

**Note:** There is a known signature verification mismatch between the RPC handler (EIP-191) and block execution (raw tx_hash). Transactions submitted via RPC are accepted into the mempool but may fail during block execution. Governance transactions use raw tx_hash and execute correctly.

## Network Integration Tests

Tests actual commonware-p2p peer connection, message passing, and PEX with real localhost TCP sockets.

```bash
./run_network_integration.sh

# Or directly:
cargo test -p call-network --test integration_test -- --nocapture
```

## Project Structure

```
tests/
├── run_inmemory_e2e.sh          # In-memory Rust E2E runner
├── run_single_node_e2e.sh       # Single-node devnet E2E runner
├── run_e2e.sh                   # 6-node devnet E2E runner
├── run_network_integration.sh   # Real P2P integration test runner
├── test_basic.py                # Python: basic RPC & balance tests
├── test_stress.py               # Python: load & throughput tests
├── test_transactions.py         # Python: transaction type tests
├── rpc_client.py                # Python RPC client library
├── signer.py                    # Python tx signing helpers
├── accounts.json                # Test account keys
└── README.md                    # This file

crates/node/tests/
├── e2e/
│   ├── mod.rs                   # E2E module root
│   └── harness.rs               # TestNode, NetworkSimulator, NodeBuilder
├── test_governance_e2e.rs
├── test_shielded_e2e.rs
├── test_bridge_e2e.rs
├── test_light_client_e2e.rs
└── test_websocket_e2e.rs

crates/network/tests/
└── integration_test.rs          # Real P2P integration tests
```
