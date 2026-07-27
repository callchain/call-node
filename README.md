# Callchain Node (call-node)

A high-performance Layer 1 blockchain node with unified EVM execution and native protocol precompiles.

[![CI](https://github.com/callchain/call-node/actions/workflows/ci.yml/badge.svg)](https://github.com/callchain/call-node/actions/workflows/ci.yml)
[![License: AGPL v3](https://img.shields.io/badge/License-AGPL_3.0-blue.svg)](LICENSE)

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

## Features

- **Unified EVM execution** — All transactions execute in EVM; protocol ops via precompiles (`0x101`–`0x209`)
- **Simplex BFT consensus** — Single validator set, 250 ms block time, sub-second finality
- **Asset-native balances** — Protocol-level stablecoin balances with deterministic execution
- **Shielded transactions** — Halo2 ZK proofs for private transfers
- **Agent framework** — AI agent sub-account delegation
- **Bridge support** — Cross-chain asset transfers with challenge periods
- **Light client** — Ethereum beacon chain verification via BLS signatures
- **Governance** — On-chain proposal lifecycle with timelock and emergency pause
- **Observability** — Prometheus metrics, OpenTelemetry tracing, structured logging
- **Compliance engine** — On-chain blacklist and issuer-configurable policies

## Development

```bash
# Run all tests (single thread avoids MDBX lock contention in test mode)
cargo test --workspace -- --test-threads=1

# Run clippy
cargo clippy --workspace -- -D warnings

# Format code
cargo fmt --all

# Run benchmarks
cargo bench --workspace

# Run fuzz targets
cargo fuzz --workspace
```

## Documentation

- [`docs/quickstart.md`](docs/quickstart.md) — Zero to first transaction in 5 minutes
- [`docs/architecture.md`](docs/architecture.md) — Architecture overview and workspace crates
- [`docs/cli.md`](docs/cli.md) — `calld` CLI reference
- [`docs/spec.md`](docs/spec.md) — Complete protocol specification
- [`docs/roadmap.md`](docs/roadmap.md) — Blockers, deferred work, and optimization priorities
- [`docs/`](docs/) — Full documentation index

## License

GNU Affero General Public License v3.0 — see [LICENSE](LICENSE) for details.
