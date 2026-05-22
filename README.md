# Callchain Node (call-node)

A high-performance Layer 1 blockchain node with unified EVM execution and native protocol precompiles.

[![CI](https://github.com/callchain/call-node/actions/workflows/ci.yml/badge.svg)](https://github.com/callchain/call-node/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](LICENSE)

## Quick Start

```bash
# Build release binary
cargo build --release

# Run a validator node (requires key)
./target/release/calld --validator --validator-key <HEX_KEY>

# Run a solo validator (single-node, no BFT consensus)
./target/release/calld --validator --validator-key <HEX_KEY> --solo

# Run a full node (non-validator)
./target/release/calld
```

## Architecture

Callchain features a unified EVM execution layer with a single consensus validator set. All transactions execute within the EVM, with protocol-level operations accessed via native precompiles at fixed addresses (`0x101`–`0x209`).

```
                    Callchain L1
              Simplex BFT Consensus (Single Validator Set)
                         │
                         ▼
                    EVM Contract
                 (Unified Execution Layer)
                         │
          ┌──────────────┼──────────────┐
          ▼              ▼              ▼
   Protocol State    ERC-20 Storage   Precompiles
   (Native Balance   (Contract        (0x101-0x209)
    Mapping)          Independent
                      Balances)
          │              │
          └──────────┬───┘
                     ▼
            Internal Bridge
           (Switch Precompile 0x207)
           Escrow or Mint/Burn per Asset
```

## Features

- **Unified EVM execution** — All transactions execute in EVM; protocol ops via precompiles
- **Simplex BFT consensus** — Single validator set securing all state
- **Asset-native balances** — Protocol-level stablecoin balances with deterministic execution
- **Open asset issuance** — Permissionless token creation on-chain
- **EVM compatibility** — Full Ethereum compatibility via revm
- **Shielded transactions** — Halo2 ZK proofs for private transfers (precompile `0x202`)
- **Agent framework** — AI agent sub-account delegation (precompile `0x209`)
- **Bridge support** — Cross-chain asset transfers with challenge periods
- **Light client** — Ethereum beacon chain verification via BLS signatures
- **Governance** — On-chain proposal lifecycle with timelock and emergency pause
- **Observability** — Prometheus metrics, OpenTelemetry tracing, structured logging, alerting
- **WebSocket RPC** — Real-time subscriptions for blocks, events, and payments
- **TLS/HTTPS RPC** — Production-ready encrypted JSON-RPC endpoints
- **Rate limiting** — Per-IP request throttling with configurable RPS windows
- **Compliance engine** — On-chain blacklist and issuer-configurable policies

## Workspace Crates

| Crate | Description |
|-------|-------------|
| `call-primitives` | Core types: Address, Hash, TxHash, BlockHash |
| `call-crypto` | Cryptographic primitives: keccak256, secp256k1, ed25519, BLS |
| `call-serialization` | Binary encoding and decoding (postcard) |
| `call-storage` | reth-db (MDBX) persistence layer |
| `call-protocol` | Balance engine, asset registry, compliance, fees |
| `call-evm` | EVM execution via revm |
| `call-consensus` | Simplex BFT consensus, block production, slashing |
| `call-network` | P2P networking via commonware-p2p |
| `call-mempool` | Mempool, transaction validation, eviction |
| `call-rpc` | JSON-RPC server (HTTP + WebSocket), rate limiting, TLS |
| `call-bridge` | Cross-chain bridge state, deposits, challenges |
| `call-shielded` | Halo2 ZK circuits, shielded transaction proofs |
| `call-agent` | AI agent registration, delegation, batch payments |
| `call-precompile` | EVM precompiled contracts (`0x101`–`0x209`) |
| `call-governance` | Proposal lifecycle, voting, timelock, emergency pause |
| `call-light-client` | Ethereum beacon chain light client verification |
| `call-oracle` | Price feed oracle with P2P aggregation |
| `call-chainspec` | Genesis configuration, chain parameters |
| `call-validator` | Validator staking, set management, key rotation |
| `call-compliance` | Sanction list, compliance data sync |
| `call-asset` | Asset registration, metadata |
| `call-switch` | Internal bridge (escrow / mint-burn) |
| `call-node` | Node application, CLI, boot sequence, telemetry |

## Development

```bash
# Run all tests (single thread avoids MDBX lock contention in test mode)
cargo test --workspace -- --test-threads=1

# Run clippy
cargo clippy --workspace -- -D warnings

# Format code
cargo fmt --all

# Build release binary
cargo build --release

# Run benchmarks
cargo bench --workspace

# Run fuzz targets
cargo fuzz --workspace
```

## CLI

```bash
# Start node with config file
calld --config /etc/callchain/config.toml

# Start validator with keystore (production)
calld --validator --validator-keystore /etc/callchain/validator.key --validator-keystore-pass-file /etc/callchain/keystore.pass

# Start with HashiCorp Vault signing
calld --validator --vault-addr https://vault.example.com:8200 --vault-token $VAULT_TOKEN --vault-key-name callchain-validator

# Start with custom RPC and P2P addresses
calld --http-addr 0.0.0.0:8545 --ws-addr 0.0.0.0:8546 --p2p-listen-addr 0.0.0.0:51235

# Start with TLS
calld --tls-cert-path /etc/callchain/cert.pem --tls-key-path /etc/callchain/key.pem

# Enable rate limiting
calld --rate-limit-rps 100 --rate-limit-window-secs 60

# Wallet commands
calld wallet generate-keys
calld wallet balance --address <ADDR> --rpc-url http://127.0.0.1:8545
calld wallet send --from-key <KEY> --to <ADDR> --amount 100 --asset-id 1 --nonce 0 --rpc-url http://127.0.0.1:8545
calld wallet server-info --rpc-url http://127.0.0.1:8545
calld wallet mempool --rpc-url http://127.0.0.1:8545
```

### Key CLI Arguments

| Argument | Default | Description |
|----------|---------|-------------|
| `--validator` | false | Run as validator (requires key) |
| `--solo` | false | Single-node validator without BFT |
| `--validator-key` | — | Hex-encoded consensus key (devnet only) |
| `--validator-keystore` | — | Path to encrypted keystore |
| `--vault-addr` | — | HashiCorp Vault URL |
| `--vault-key-name` | — | Vault transit key name |
| `--p2p-listen-addr` | `0.0.0.0:51235` | P2P listen address |
| `--p2p-bootstrap-peers` | — | Comma-separated peers |
| `--http-addr` | `127.0.0.1:8545` | HTTP RPC listen address |
| `--ws-addr` | `127.0.0.1:8546` | WebSocket RPC listen address |
| `--metrics-addr` | `0.0.0.0:9090` | Prometheus metrics endpoint |
| `--data-dir` | `~/.callchain` | Chain data directory |
| `--db-cache-size` | 1024 | DB cache size in MB |
| `--archive` | false | Keep all history (disable pruning) |
| `--log-level` | `info` | Log level (trace/debug/info/warn/error) |
| `--log-format` | `text` | Log format (text/json) |
| `--rate-limit-rps` | — | Per-IP max requests per window |
| `--rate-limit-window-secs` | 60 | Rate limit window |
| `--tls-cert-path` | — | TLS certificate (PEM) |
| `--tls-key-path` | — | TLS private key (PEM) |
| `--config` | — | TOML config file path |

## RPC

The node exposes standard JSON-RPC over HTTP (default port `8545`) and WebSocket (default port `8546`):

```bash
# Server info
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"server_info","id":1}'

# Account balance
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"call_protocolBalance","params":[1,"0x..."],"id":1}'

# Eth block number
curl -X POST http://localhost:8545 -H "Content-Type: application/json" \
  -d '{"jsonrpc":"2.0","method":"eth_blockNumber","id":1}'
```

## Documentation

See [`docs/`](docs/) for full documentation including architecture specs, developer guides, and operational runbooks.

## License

MIT — see [LICENSE](LICENSE) for details.
