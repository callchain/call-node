# Callchain Documentation

This directory contains the complete documentation for the Callchain node.

---

## Overview

Callchain is a Layer 1 blockchain built on a unified EVM execution architecture. Every transaction — whether a simple payment, governance vote, oracle price submission, or cross-chain bridge operation — executes inside the EVM via precompiled contracts (`0x101`–`0x209`). Consensus is driven by Commonware Simplex BFT, with a validator set elected through DPoS staking of the native CALL token.

**Key features:**

- **Ethereum-compatible RPC** — 47 `eth_*` methods, full blockTag history, Merkle proofs, Filter API, WebSocket subscriptions
- **Unified EVM execution** — No dual ledger; protocol state changes happen inside EVM precompiles
- **Cross-chain bridge** — Validator multi-sig deposits with challenge periods, plus light-client MPT proof path
- **Decentralized oracle** — Ed25519-signed price submissions, median aggregation, outlier detection
- **Shielded transactions** — Halo2 ZK proofs for private deposits, transfers, and withdrawals
- **On-chain governance** — Proposal types for parameter changes, validator key rotation, emergency pause
- **Compliance engine** — Per-asset sanctions lists and issuer policies

The documents below cover protocol design, operational guides, API references, and security research.

| Document | What you'll learn |
|----------|-----------------|
| [`spec.md`](spec.md) | Complete protocol specification, architecture, and design principles |
| [`CALL.md`](CALL.md) | The CALL token and native asset design |

## How To Use

| Document | What you'll learn |
|----------|-----------------|
| [`how-to/for-developer.md`](how-to/for-developer.md) | Developer workflow: build, test, local devnet, contributing |
| [`how-to/for-operator.md`](how-to/for-operator.md) | Node operator deployment: Docker, systemd, monitoring, backup, upgrade |
| [`how-to/for-validator.md`](how-to/for-validator.md) | Validator operations: staking, oracle, bridge attestation, key rotation, slashing |
| [`how-to/for-bridge.md`](how-to/for-bridge.md) | Bridge operations: deposits, withdrawals, challenges, fraud proofs |
| [`how-to/for-light-client.md`](how-to/for-light-client.md) | Light client operations: checkpoint, header sync, MPT proofs, reorg handling |

## Architecture & Design

| Document | Description |
|----------|-------------|
| [`spec.md`](spec.md) | Protocol specification: consensus, execution, storage, networking |
| [`protocol.md`](protocol.md) | Protocol payment layer: balances, fees, replay protection, compliance |
| [`consensus.md`](consensus.md) | Simplex BFT consensus, block production, fork management, slashing |
| [`evm.md`](evm.md) | EVM execution layer via revm |
| [`storage.md`](storage.md) | MDBX persistence layer, pruning, snapshots, migrations |
| [`network.md`](network.md) | P2P networking, gossip, peer management, ban enforcement |
| [`mempool.md`](mempool.md) | Transaction mempool, validation, eviction under pressure |
| [`transaction.md`](transaction.md) | Transaction formats, serialization, lifecycle |
| [`reth.md`](reth.md) | Reth integration and compatibility notes |

## Subsystems & Precompiles

| Document | Description |
|----------|-------------|
| [`precompile.md`](precompile.md) | All EVM precompiles (`0x101`–`0x209`) |
| [`bridge.md`](bridge.md) | Cross-chain bridge: deposits, challenges, signature thresholds |
| [`shielded.md`](shielded.md) | Halo2 ZK circuits, shielded transaction precompile (`0x202`) |
| [`agent.md`](agent.md) | AI agent framework: registration, delegation, batch payments (`0x209`) |
| [`governance.md`](governance.md) | On-chain governance: proposals, voting, timelock, emergency pause |
| [`light-client.md`](light-client.md) | Ethereum beacon chain light client verification |
| [`oracle.md`](oracle.md) | Price feed oracle with P2P aggregation |
| [`compliance.md`](compliance.md) | Compliance engine, sanctions list, issuer policies |
| [`switch.md`](switch.md) | Internal bridge (escrow vs mint-burn per asset) |
| [`asset.md`](asset.md) | Asset registration, open issuance, metadata |

## RPC & APIs

| Document | Description |
|----------|-------------|
| [`rpc.md`](rpc.md) | JSON-RPC server: HTTP, WebSocket, subscriptions, methods |
| [`eth_rpc.md`](eth_rpc.md) | Ethereum-compatible RPC methods (`eth_*`) |

## Observability & Operations

| Document | Description |
|----------|-------------|
| [`observability.md`](observability.md) | Prometheus metrics, OpenTelemetry tracing, logging, alerts, health checks |
| [`release.md`](release.md) | Release process, key management, genesis, deployment, rollback |
| [`validator_staking.md`](validator_staking.md) | Validator staking mechanics |
| [`validator_key_rotation.md`](validator_key_rotation.md) | Key rotation protocol and emergency procedures |
| [`prover_key_rotation.md`](prover_key_rotation.md) | ZK prover key rotation |

## Testing & Quality

| Document | Description |
|----------|-------------|
| [`testing.md`](testing.md) | Test strategy, coverage, fuzzing, benchmarks, E2E tests |
| [`audit.md`](audit.md) | Security audit scope, readiness checklist, post-audit process |

## Security & Cryptography

| Document | Description |
|----------|-------------|
| [`security.md`](security.md) | Security model, threat analysis, mitigations |
| [`halo2.md`](halo2.md) | Halo2 circuit design and constraints |
| [`zk.md`](zk.md) | Zero-knowledge proof system overview |
| [`bls_consensus_verification.md`](bls_consensus_verification.md) | BLS signature verification in consensus |

## Research & Future

| Document | Description |
|----------|-------------|
| [`future.md`](future.md) | Roadmap and planned features |
| [`vitalik-zk-payment.md`](vitalik-zk-payment.md) | Vitalik's ZK payment design analysis |
| [`seismic.md`](seismic.md) | Seismic / encrypted memory research |
| [`upgrade.md`](upgrade.md) | Protocol upgrade mechanism |
| [`unready.md`](unready.md) | Remaining blockers before mainnet / testnet launch |
| [`genesis.md`](genesis.md) | Genesis configuration requirements and creation guide |
| [`rfcs/threshold-signing.md`](rfcs/threshold-signing.md) | Threshold signature scheme for validators (RFC) |

## Runbooks

Operational procedures for incident response:

| Document | Scenario |
|----------|----------|
| [`runbooks/chain-halt.md`](runbooks/chain-halt.md) | Consensus stop diagnosis and recovery |
| [`runbooks/state-corruption.md`](runbooks/state-corruption.md) | Database corruption recovery |
| [`runbooks/mass-offline.md`](runbooks/mass-offline.md) | Mass validator offline response |

---

**Last updated**: 2026-05-22
