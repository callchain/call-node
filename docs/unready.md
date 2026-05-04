# Production Readiness — Remaining Blockers

This document tracks the gap between the current codebase and production mainnet readiness. Individual subsystem docs may claim "all gaps resolved" for their scope; this is the cross-cutting tracker.

**Last updated:** 2026-04-23

---

## Critical (Must Resolve Before Mainnet)

| # | Blocker | Scope | Details |
|---|---------|-------|---------|
| 1 | **No TLS/HTTPS tests** | `crates/rpc` | RPC servers bind to plain HTTP. No tests verify TLS termination or certificate handling. Config exists but untested. |
| 2 | **No authentication/authorization tests** | `crates/rpc` | No API key, JWT, or IP allowlist tests. All RPC endpoints are effectively unprotected in tests and production. |
| 3 | **No rate limiting tests for RPC** | `crates/rpc` | `max_connections` caps concurrent connections but no tests verify per-client request throttling. |
| 4 | **No MDBX integration tests** | `crates/storage` | Storage crate has table descriptors but no actual MDBX read/write/delete integration tests. Production would run on JSON fallback if MDBX fails. |
| 5 | **No concurrent access/corruption recovery tests** | `crates/storage`, `crates/node` | No tests for concurrent DB writes, crash recovery, WAL behavior, or incomplete checkpoint detection. |
| 6 | **No network partition tests** | `crates/node/tests` | E2E tests use local harness. No tests for network partitions, Byzantine nodes, equivocation, or message delays. |
| 7 | **No light client consensus verification tests** | `crates/light-client` | Light client does not verify Ethereum BLS signatures. No tests for malicious fork feeding or consensus-layer attacks. |
| 8 | **No oracle signature verification negative tests** | `crates/oracle` | Oracle price submissions accept any 64-byte signature. No negative test exists with invalid signatures. |
| 9 | **No database migration framework** | `crates/storage` | No schema versioning. Changes to data layout require manual migration or full resync. |
| 10 | **No formal security audit** | Entire codebase | No third-party audit of consensus, cryptography, or economic incentives. |
| 11 | **No formal verification for shielded circuits** | `crates/shielded` | Groth16 circuits are tested but not formally verified. Soundness/completeness proofs absent. |

---

## High (Must Resolve Before Public Testnet)

| # | Blocker | Scope | Details |
|---|---------|-------|---------|
| 12 | **No `eth_getLogs` performance tests** | `crates/rpc` | Scans all receipts linearly (O(n)). No test validates behavior at 1M+ blocks. |
| 13 | **No receipt persistence tests** | `crates/node` | Receipts are in-memory only. No test verifies DB persistence or recovery across restarts. |
| 14 | **No agent integration with block production tests** | `crates/consensus` | Agent transactions exist as a library but full E2E coverage of agent tx in a real block is incomplete. |
| 15 | **No slashing economic penalty tests** | `crates/consensus` | Double-sign detection works but no test verifies stake reduction or validator removal economic effects. |
| 16 | **No upgrade persistence tests** | `crates/consensus` | `ForkManager` is persisted but no test verifies scheduled upgrades survive restart and activate correctly. |
| 17 | **No multi-upgrade-at-same-height tests** | `crates/consensus` | `check_upgrades_at_height` behavior with multiple upgrades at the same height is untested. |
| 18 | **No snapshot production/verification tests** | `crates/storage` | Snapshot production is wired but `verify_snapshot` only counts signatures, no cryptographic verification. |
| 19 | **No fast sync incremental catch-up tests** | `crates/storage` | `incremental_sync()` returns `Ok(0)` by design. No test verifies catch-up from snapshot to chain head. |
| 20 | **No MPT proof verification tests against live Ethereum** | `crates/light-client` | Bridge MPT proof verification is behind `light-client-bridge` feature flag. No tests validate tx inclusion or receipt proof verification against real Ethereum headers. |
| 21 | **No real-network light client tests** | `crates/light-client` | Missing 7+ day soak test against live Ethereum RPC for header chain submission, receipt proofs, and reorg handling. |
| 22 | **No CI/CD pipeline** | Repository | Tests are run manually. No automated test execution, coverage tracking, or benchmark suite on PRs. |

---

## Medium (Should Resolve Before Launch)

| # | Blocker | Scope | Details |
|---|---------|-------|---------|
| 23 | **OTel spans not called in production paths** | `crates/node` | `record_block_span()`, `record_tx_span()`, `record_p2p_span()` are defined and tested but not called in hot paths. |
| 24 | **No Grafana dashboards** | `docs/observability.md` | No pre-built JSON dashboard files for Grafana import. |
| 25 | **Structured error codes missing** | `crates/rpc` | Errors are string messages. No machine-readable error codes for alerting or automated response. |
| 26 | **No WebSocket lag handling tests** | `crates/rpc` | Lagged subscribers are not notified. No test verifies silent event dropping. |
| 27 | **No CORS configuration tests** | `crates/rpc` | Default jsonrpsee CORS policy untested. |
| 28 | **No mempool eviction under memory pressure tests** | `crates/mempool` | `ReplayProtector` evicts 25% when over limit but no test verifies correctness during eviction. |
| 29 | **No P2P ban enforcement tests** | `crates/network` | `NetworkLimits` defines ban duration but no test verifies peer banning works in practice. |
| 30 | **No property-based or fuzz testing** | Entire codebase | No `proptest`, `quickcheck`, or `cargo-fuzz` for transaction RLP decoding, MPT proof parsing, precompile call deserialization. |
| 31 | **No benchmark suite** | Entire codebase | No `criterion.rs` benchmarks for hot paths (MPT verification, proof generation, block production). |
| 32 | **No testnet environment** | Infrastructure | E2E tests run in-memory. No long-running testnet for soak testing or heterogeneous fork upgrade testing. |

---

## Governance — Recently Resolved

The following governance gaps were resolved in the 2026-04-23 hardening pass:

| Gap | Resolution |
|-----|------------|
| `apply_proposal` only logged for most proposal types | `apply_proposal` now applies real internal state changes for all 10 proposal types |
| Governance state persistence | Already persisted to MDBX; `executor` and `balance_source` rewired after load |
| ForkManager integration for ProtocolUpgrade | Already wired via `NodeProposalExecutor` |

See `docs/governance.md` for full details.

---

## Subsystem Docs With Overly Optimistic Assessments

The following docs claim "all gaps resolved" or "production ready" for their subsystem. While the implementations are solid, the cross-cutting testing and infrastructure gaps above mean the system as a whole is **not** production ready:

- `docs/protocol.md` — "All protocol components are production-ready. No remaining gaps."
- `docs/bridge.md` — "All documented gaps have been resolved."
- `docs/security.md` — All components marked "Production ready".
- `docs/shielded.md` — All components marked "Production ready".
- `docs/compliance.md` — All components marked "Ready".
- `docs/agent.md` — All components marked "Ready".
- `docs/upgrade.md` — All components marked "Ready".
- `docs/rpc.md` — All components marked "Ready".

These assessments are accurate for the **implementation scope** of each subsystem but do not account for the integration, testing, and infrastructure gaps tracked in this document.
