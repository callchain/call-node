# Changelog

All notable changes to this project will be documented in this file.

## [Unreleased]

### Added
- Dual execution domains: Protocol Payment Layer + EVM Contract Layer
- Simplex BFT consensus with single validator set
- P2P sync protocol with light client header verification
- JSON-RPC server with HTTP and WebSocket support
- WebSocket subscriptions for block announcements and payment events
- Shielded transaction support with ZK proof verification
- Agent framework for AI agents with native balance management
- CLI wallet: generate keys, query balances, send payments
- OpenTelemetry metrics and telemetry dashboard
- reth-db (MDBX) persistence with restart recovery
- Iterative batch sync with header verification
- Cross-chain bridge state management
- Compliance engine for asset-level restrictions
- Light client RPC endpoints for header and proof verification
- Incremental DB writes per block (full flush every 1000 blocks)
- Cargo-deny dependency checks in CI
