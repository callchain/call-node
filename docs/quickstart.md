# Callchain Quickstart

Get from zero to your first transaction in under 5 minutes.

---

## Option 1: Docker Compose (Fastest)

### Prerequisites

- [Docker](https://docs.docker.com/get-docker/) & Docker Compose
- `curl` or any HTTP client

### Start a Local Devnet

```bash
git clone https://github.com/callchain/call-node.git
cd call-node

# Build the image
docker build -t callchain/calld:latest .

# Start a single-node devnet
docker compose up -d
```

The node exposes:
- HTTP RPC: `http://localhost:5005`
- WebSocket: `ws://localhost:5006`
- Metrics: `http://localhost:9090/metrics`

### Verify the Node is Running

```bash
curl -s -X POST http://localhost:5005 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}' | jq .
```

Expected: `"0x0"` (genesis block).

### Send Your First Transaction

Generate a funded test account using the devnet genesis:

```bash
# Query balance of a genesis-funded address (replace with actual genesis address)
curl -s -X POST http://localhost:5005 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_getBalance",
    "params": ["0x0000000000000000000000000000000000000000", "latest"],
    "id": 1
  }' | jq .
```

Transfer CALL tokens via `eth_sendRawTransaction` (requires signing with a private key):

```bash
# Use cast (foundry) or any EVM-compatible signer
cast send --rpc-url http://localhost:5005 \
  --private-key 0x... \
  --value 0.1ether \
  0x0000000000000000000000000000000000000001
```

> **Note:** Devnet genesis pre-funds several test accounts. See `example/genesis.example.json` for addresses and private keys.

---

## Option 2: Build from Source

### Prerequisites

- Rust 1.82+
- `clang`, `libssl-dev`, `pkg-config`
- ~8 GB RAM, ~50 GB disk

```bash
git clone https://github.com/callchain/call-node.git
cd call-node

# Build release binary (~10 min on modern hardware)
cargo build --release

# The binary is at target/release/calld
```

### Start the Node

```bash
# Copy example config and genesis
cp example/config.example.toml /tmp/callchain-config.toml
cp example/genesis.example.json /tmp/callchain-genesis.json

# Run
target/release/calld \
  --data-dir /tmp/callchain-data \
  --config /tmp/callchain-config.toml \
  --genesis-path /tmp/callchain-genesis.json \
  --log-level info
```

By default the node listens on:
- HTTP RPC: `http://localhost:8545`
- WebSocket: `ws://localhost:8546`
- P2P: `0.0.0.0:51235`

---

## Query the Chain

### Get Chain ID

```bash
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_chainId","id":1}' | jq .
```

### Get Latest Block

```bash
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_getBlockByNumber","params":["latest",false],"id":1}' | jq .
```

### Estimate Gas

```bash
curl -s -X POST http://localhost:8545 \
  -H "Content-Type: application/json" \
  -d '{
    "jsonrpc": "2.0",
    "method": "eth_estimateGas",
    "params": [{
      "from": "0x...",
      "to": "0x0000000000000000000000000000000000000201",
      "data": "0x..."
    }],
    "id": 1
  }' | jq .
```

---

## Next Steps

| What you want to do | Go to |
|---------------------|-------|
| Learn the full RPC surface | [`eth_rpc.md`](eth_rpc.md) |
| Understand precompiles (0x101–0x209) | [`precompile.md`](precompile.md) |
| Register an asset or transfer tokens | [`how-to/for-developer.md`](how-to/for-developer.md) |
| Run a production node | [`how-to/for-operator.md`](how-to/for-operator.md) |
| Run a validator | [`how-to/for-validator.md`](how-to/for-validator.md) |
| Build with the bridge | [`how-to/for-bridge.md`](how-to/for-bridge.md) |

---

## Troubleshooting

| Problem | Fix |
|---------|-----|
| Port 5005 already in use | Change port mapping in `docker-compose.yml` |
| "MDBX lock contention" | Ensure only one `calld` process per `--data-dir` |
| `cargo build` OOM | Use `cargo build --release -j2` to limit parallelism |
| No peers connected | Check bootstrap peers in config; devnet single-node has no peers |
