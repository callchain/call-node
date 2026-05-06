# Callchain Node (call-node)

A high-performance Layer 1 blockchain node with dual execution domains: Protocol Payment Layer and EVM Contract Layer.

[![CI](https://github.com/callchain/call-node/actions/workflows/ci.yml/badge.svg)](https://github.com/callchain/call-node/actions/workflows/ci.yml)
[![codecov](https://codecov.io/gh/callchain/call-node/branch/main/graph/badge.svg)](https://codecov.io/gh/callchain/call-node)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

## Quick Start

```bash
# Build
cargo build --release

# Run a validator node
./target/release/calld --validation-seed <SEED>

# Run a full node
./target/release/calld

# Connect to a peer
./target/release/calld --peers 127.0.0.1:51235
```

## Architecture

Callchain features a dual-domain architecture with a single consensus validator set:

```
                    Callchain L1
              Simplex BFT Consensus
                         │
          ┌──────────────┴──────────────┐
          ▼                             ▼
   Protocol Payment              EVM Contract
     Layer                          Layer
          │                             │
          ▼                             ▼
   Protocol Balances              ERC-20 Storage
          │                             │
          └──────────┬──────────────────┘
                     ▼
            Internal Bridge
           (lock-and-release)
```

## Features

- **Dual execution domains** — Protocol payments and EVM smart contracts
- **Simplex BFT consensus** — Single validator set securing both domains
- **Asset-native balances** — Protocol-level stablecoin balances with deterministic execution
- **Open asset issuance** — Permissionless token creation on-chain
- **EVM compatibility** — Full Ethereum compatibility via revm
- **Shielded transactions** — Zero-knowledge proof support for private transfers
- **Agent framework** — AI agent support with native balance management
- **Bridge support** — Cross-chain asset transfer support
- **WebSocket RPC** — Real-time subscriptions for blocks and payments
- **Light client** — Resource-constrained verification with header + proof checking

## Workspace Crates

| Crate | Description |
|-------|-------------|
| `call-primitives` | Core types: Address, Hash, TxHash, BlockHash |
| `call-crypto` | Cryptographic primitives: keccak256, secp256k1, ed25519 |
| `call-serialization` | Binary encoding and decoding |
| `call-storage` | reth-db (MDBX) persistence layer |
| `call-protocol` | Balance, asset registry, compliance, fee engine |
| `call-evm` | EVM execution via revm |
| `call-consensus` | Simplex BFT consensus and block production |
| `call-network` | P2P networking via commonware-p2p |
| `call-mempool` | Mempool and transaction management |
| `call-rpc` | JSON-RPC server (HTTP + WebSocket) |
| `call-bridge` | Cross-chain bridge state management |
| `call-shielded` | ZK proofs and shielded transaction support |
| `call-agent` | AI agent registration and balance management |
| `call-precompiles` | EVM precompiled contracts |
| `call-node` | Node application, CLI, boot sequence |

## Development

```bash
# Run all tests
cargo test --workspace

# Run clippy
cargo clippy --workspace -- -D warnings

# Format code
cargo fmt --all

# Build release binary
cargo build --release
```

## CLI

```bash
# Start node with custom data directory
calld --data-dir /path/to/data

# Start node with specific peers
calld --peers 127.0.0.1:51235,127.0.0.1:51236

# Generate new keypair
calld wallet generate-keys

# Query balance
calld wallet balance --address <ADDR> --rpc-url http://127.0.0.1:5005

# Send payment
calld wallet send --from-key <KEY> --to <ADDR> --amount 100 --asset-id 1 --rpc-url http://127.0.0.1:5005
```

## RPC

The node exposes standard JSON-RPC over HTTP (default port 5005) and WebSocket (default port 5006):

```bash
# Server info
curl -X POST http://localhost:5005 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"server_info","id":1}'

# Account balance
curl -X POST http://localhost:5005 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_protocolBalance","params":[1,"0x..."],"id":1}'
```

## License

MIT — see [LICENSE](LICENSE) for details.
