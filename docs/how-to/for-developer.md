# How-To: For Developers

## Prerequisites

- Rust 1.82 or later
- Cargo (comes with Rust)
- Git
- ~8GB RAM for full test suite
- ~2GB disk for build artifacts

## Clone and Build

```bash
git clone https://github.com/callchain/call-node.git
cd call-node

# Debug build
cargo build

# Release build (optimized)
cargo build --release

# The binary is at target/release/calld
```

## Running Tests

```bash
# All tests (single thread avoids MDBX lock contention)
cargo test --workspace -- --test-threads=1

# Specific crate
cargo test -p call-consensus -- --test-threads=1
cargo test -p call-rpc -- --test-threads=1
cargo test -p call-shielded --features nova-prover -- --test-threads=1

# With output
cargo test --workspace -- --test-threads=1 --nocapture
```

## Code Quality

```bash
# Clippy (treat warnings as errors in CI)
cargo clippy --workspace -- -D warnings

# Formatting
cargo fmt --all

# Check formatting without modifying
cargo fmt --all -- --check

# Audit dependencies
cargo audit
```

## Genesis Setup

Before starting a node, you need a genesis file to initialize chain state.

```bash
# Use the built-in example (devnet/testnet)
cp example/genesis.example.json ~/.callchain/genesis.json

# Or create your own following docs/genesis.md
# The genesis file defines initial validators, balances, assets, and consensus params
```

## Local Devnet (Single Node)

```bash
# Generate a validator key
calld wallet generate-keys
# Save the secret key output

# Run a solo validator (no BFT consensus needed for local dev)
calld --validator --validator-key <SECRET_KEY_HEX> --solo \
  --genesis-path ~/.callchain/genesis.json \
  --http-addr 127.0.0.1:8545 \
  --ws-addr 127.0.0.1:8546 \
  --metrics-addr 127.0.0.1:9090 \
  --log-level debug
```

The node will:
- Produce blocks every ~250ms
- Expose HTTP RPC on `8545`
- Expose WebSocket RPC on `8546`
- Expose Prometheus metrics on `9090`

## Local Devnet (Multi-Node)

For a 4-node BFT network on localhost, each node needs a distinct **P2P identity key**. The peer_id is the hex-encoded public key (64 hex chars) derived from the identity key.

```bash
# Generate identity keys for each node
calld wallet generate-keys  # Node 1 — save secret key and public key (peer_id)
calld wallet generate-keys  # Node 2
calld wallet generate-keys  # Node 3
calld wallet generate-keys  # Node 4

# Node 1 (bootstrap)
calld --validator --validator-key <KEY1> --identity-key <ID_SECRET_1> \
  --p2p-listen-addr 127.0.0.1:51235 \
  --http-addr 127.0.0.1:8545 \
  --data-dir ~/.callchain/node1 \
  --genesis-path ~/.callchain/genesis.json

# Check Node 1 logs for its peer_id (hex public key, 64 chars):
# grep "peer_id" ~/.callchain/node1/logs/callchain.log

# Node 2 — use Node 1's peer_id as bootstrap
calld --validator --validator-key <KEY2> --identity-key <ID_SECRET_2> \
  --p2p-listen-addr 127.0.0.1:51236 \
  --p2p-bootstrap-peers <PEER1_ID_HEX>@127.0.0.1:51235 \
  --http-addr 127.0.0.1:8546 \
  --data-dir ~/.callchain/node2 \
  --genesis-path ~/.callchain/genesis.json

# Node 3 & 4 follow the same pattern
```

**Tip:** If you omit `--identity-key`, the node generates one automatically and logs the peer_id on startup. Look for `peer_id` in the startup logs.

## Wallet Operations

```bash
# Check balance
calld wallet balance --address <ADDR> --asset-id 1 --rpc-url http://127.0.0.1:8545

# Send payment
calld wallet send \
  --from-key <SECRET_KEY> \
  --to <RECIPIENT_ADDR> \
  --amount 1000000 \
  --asset-id 1 \
  --nonce 0 \
  --rpc-url http://127.0.0.1:8545

# Server info
calld wallet server-info --rpc-url http://127.0.0.1:8545

# Mempool stats
calld wallet mempool --rpc-url http://127.0.0.1:8545
```

## RPC Examples

```bash
# Eth block number
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'

# Protocol balance
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_protocolBalance","params":[1,"0x..."],"id":1}'

# Server info
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"server_info","id":1}'
```

## Adding a New Crate

1. Create directory under `crates/<name>/`
2. Add `Cargo.toml` with `workspace = true`
3. Add to root `Cargo.toml` workspace members
4. Add to `workspace.dependencies` if other crates will depend on it
5. Run `cargo check --workspace`

## Running Benchmarks

```bash
# All benchmarks
cargo bench --workspace

# Specific benchmark
cargo bench -p call-precompile --bench precompile_execute
cargo bench -p call-consensus --bench block_production
cargo bench -p call-crypto --bench signature_verify
```

## Fuzz Testing

```bash
# Run a fuzz target
cargo fuzz run tx_rlp_decode
cargo fuzz run precompile_dispatch
cargo fuzz run balance_arithmetic
```

## Contributing

1. Fork and branch from `main`
2. Write tests for new code
3. Run `cargo clippy --workspace -- -D warnings`
4. Run `cargo test --workspace -- --test-threads=1`
5. Open a PR against `main`
