# Callchain Roadmap

Central tracking document for production readiness blockers, deferred work, and
optimization priorities. Replaces the former `docs/future.md` (deferred items)
and `docs/unready.md` (blockers).

**Last updated:** 2026-07-23

---

## Table of Contents

- [1. Critical (Must Resolve Before Mainnet)](#1-critical-must-resolve-before-mainnet)
- [2. High (Must Resolve Before Public Testnet)](#2-high-must-resolve-before-public-testnet)
- [3. Medium (Should Resolve Before Launch)](#3-medium-should-resolve-before-launch)
- [4. Deferred Work Items](#4-deferred-work-items)
- [5. Performance Optimization](#5-performance-optimization)
- [6. Testing & Quality Assurance](#6-testing--quality-assurance)
- [7. Observability](#7-observability)
- [8. Developer Experience](#8-developer-experience)
- [9. Operations & Deployment](#9-operations--deployment)
- [10. Ecosystem & Protocol Layer](#10-ecosystem--protocol-layer)
- [11. Dependency Management](#11-dependency-management)
- [12. Recently Resolved](#12-recently-resolved)

---

## 1. Critical (Must Resolve Before Mainnet)

### 1.1 Formal Security Audit

- **Scope:** Entire codebase (consensus, cryptography, economic incentives)
- **Status:** Not started
- **Recommended firms:** Trail of Bits, NCC Group, Least Authority
- **Prerequisite:** Resolve all Critical/High blockers below before engaging an auditor
- **Notes:** The codebase has strong test coverage (~1,177 tests) and CI security
  tooling (Kani model checking, fuzz targets, cargo-audit, cargo-deny), but none
  of these substitute for third-party review.

### 1.2 Formal Verification for Shielded Circuits

- **Scope:** `crates/shielded` — Halo2 PLONKish circuits
- **Status:** Deferred (historical Groth16 Lean 4 formalization deleted during
  Halo2 migration — see [§4.3](#43-shielded-circuit-formal-verification))
- **Impact:** Halo2 circuits are tested but not formally proved — soundness and
  completeness proofs absent
- **When to revisit:** Third-party audit should review Halo2 circuit constraints
  directly; formal verification is a follow-up

---

## 2. High (Must Resolve Before Public Testnet)

### 2.1 MPT Proof Verification Against Live Ethereum

- **Scope:** `crates/light-client` — bridge MPT proof verification
- **Status:** Not started
- **Details:** Verification is behind the `light-client-bridge` feature flag.
  No tests validate transaction inclusion or receipt proof verification against
  real Ethereum headers.
- **Acceptance criteria:** Tests that fetch real Ethereum mainnet headers and
  verify inclusion proofs end-to-end.

### 2.2 Real-Network Light Client Tests

- **Scope:** `crates/light-client`
- **Status:** Not started
- **Details:** Missing 7+ day soak test against live Ethereum RPC for header
  chain submission, receipt proofs, and reorg handling.
- **Acceptance criteria:** Automated test that runs continuously against a live
  Ethereum node for 7+ days, reporting any failures.

---

## 3. Medium (Should Resolve Before Launch)

### 3.1 Testnet Environment

- **Scope:** Infrastructure
- **Status:** Not started
- **Details:** E2E tests run in-memory. No long-running testnet for soak testing
  or heterogeneous fork upgrade testing.
- **Recommended approach:** Deploy a persistent multi-validator testnet with
  monitoring, faucet, and block explorer.

---

## 4. Deferred Work Items

### 4.1 EthLightClient BLS Consensus Verification

- **Scope:** `crates/light-client`
- **Status:** Deferred
- **Context:** The Ethereum light client currently verifies headers via
  parent-hash chain only. It does not verify Ethereum consensus layer BLS
  aggregate signatures from the beacon chain sync committee.
- **Why deferred:** Parent-hash chain + `set_finalized_block()` checkpoint
  tracking is sufficient for bridge deposit validation on devnet and testnet.
  Full consensus verification requires integrating Ethereum beacon chain light
  client sync (Altair sync committees, ~512 validators per period), which is a
  significant scope increase.
- **When to revisit:** Before mainnet bridge launch, at which point the bridge
  must trustlessly verify Ethereum finality without an externally-set checkpoint.

### 4.2 MEV Protection

- **Scope:** `crates/protocol`, `crates/consensus`
- **Status:** Deferred
- **Context:** `crates/protocol` contains a commit-reveal library for sealed-bid
  submission, but it is not integrated into block production. Validators can
  inspect the mempool and reorder or front-run transactions for profit.
- **Why deferred:** MEV protection requires protocol-level changes (commit-reveal
  timing, encrypted mempool, fair ordering) that complicate the consensus-critical
  path. For devnet and testnet, the economic value at risk is low and
  operator-run validators are trusted.
- **Implementation plan:**
  1. Integrate commit-reveal into `BlockProducer` tx selection
  2. Add encrypted mempool layer (threshold encryption or time-lock puzzles)
  3. Fair ordering: FCFS within a block or deterministic shuffle
  4. Penalize validators that violate ordering rules

### 4.3 Shielded Circuit Formal Verification

- **Scope:** `crates/shielded`
- **Status:** Deferred — future work
- **History:** A Lean 4 formalization of the Groth16/R1CS shielded circuits was
  previously developed in `formal_verification/lean/`. It modeled the
  pre-Halo2 implementation and was deleted during the Halo2 migration.
- **Current state:**

  | Gap | Status | Description |
  |-----|--------|-------------|
  | Lean ↔ Rust correspondence (R1CS) | Deleted | Historical Groth16 formalization; artifacts removed |
  | Halo2 circuit formalization | Future work | PLONKish constraints, custom gates, permutation arguments |
  | Range check full expansion | Closed | Halo2 uses `halo2_gadgets` range check (production-proven in Orchard) |

- **When to revisit:** After third-party audit — let audit findings guide the
  required depth of formal verification.

### 4.4 Grafana Dashboards

- **Scope:** `docs/observability/grafana/`
- **Status:** Deferred
- **Context:** The node exposes a Prometheus-compatible `/metrics` endpoint on
  `:9090`. All consensus, mempool, P2P, storage, and latency metrics are
  already emitted. However, there are no pre-built Grafana JSON dashboard files
  checked into the repository.
- **Why deferred:** Grafana is a deployment-layer concern. The metrics schema is
  stable and self-describing. Operators can import metrics in a few minutes
  using Grafana's built-in Prometheus data source and query builder.
- **When to revisit:** Before public testnet launch.
- **Implementation plan:**
  1. Create `docs/observability/grafana/` directory with JSON dashboard exports
  2. Dashboards to include:
     - **Consensus Overview:** blocks produced/committed, rounds, timeouts,
       latency p50/p95/p99
     - **Mempool Health:** tx count, rejected rate, bridge pending, fee history
     - **P2P Network:** peer count, bytes sent/received, message latency
     - **Storage / Pruning:** traces/receipts/bodies/snapshots pruned
     - **Node Health:** uptime, last block age (for stall detection)
  3. Add Grafana provisioning YAML for automatic dashboard loading
  4. Document data source configuration in `docs/observability.md`

### 4.5 System Contracts Migration

- **Scope:** `crates/precompile`
- **Status:** Deferred — long-term architectural question
- **Context:** All protocol logic (staking, assets, governance, shielded pool,
  bridge, oracle) is currently implemented as Rust precompiles (0x201–0x209).
  There is no plan to migrate to Solidity system contracts.
- **Why deferred:** Rust precompiles are more auditable, gas-efficient, and
  integrate cleanly with the consensus layer. Solidity system contracts would
  require a full rewrite, new tooling, and additional audit surface. This is
  not a testnet blocker.
- **When to revisit:** Post-mainnet, if ecosystem demand for Solidity-level
  composability justifies the migration cost.
- **Implementation plan (if pursued):**
  1. Formalize the precompile ↔ Solidity interface mapping
  2. Implement each precompile as a delegating Solidity proxy
  3. Governance-driven migration with backward compatibility period
  4. Deprecate Rust precompiles once usage drops below threshold

---

## 5. Performance Optimization

### 5.1 MDBX Serialization

- **ADR reference:** [ADR-0001](adr/0001-use-mdbx-as-sole-storage.md)
- **Scope:** `crates/storage`
- **Current approach:** `serde_json` for all values
- **Opportunity:** Migrate to `postcard` + `reth-codecs` to reduce value size and
  (de)serialization cost. Accepted as a trade-off during initial development to
  avoid maintaining codec implementations for ~20 custom types.
- **Priority:** Medium — suitable as a post-mainnet optimization.

### 5.2 MDBX Tuning

| Parameter | Default | Production Recommended |
|-----------|---------|----------------------|
| `db_cache_size` | 1 GB | 25–50% of available RAM (max 70%) |
| `snapshot_retention_blocks` | 128 | 128 (full node), 1024+ (archive) |

### 5.3 SELFDESTRUCT Storage Cleanup

- **Scope:** `crates/evm/src/db.rs`
- **Known limitation:** MDBX has no efficient prefix/range deletion. Clearing
  an account's storage on `SELFDESTRUCT` currently iterates all slots for that
  address. This is noted as a production TODO.

### 5.4 Shielded Proof Generation

- **Current performance:** ~5–10 seconds per proof
- **Mitigation:** Run dedicated `call-prover` cluster for proof generation
- **Priority:** Address when shielded pool sees meaningful usage

### 5.5 Key Performance Metrics

| Metric | Current | Target | Alert Threshold |
|--------|---------|--------|-----------------|
| Block time | 250 ms | 250 ms | > 500 ms p99 |
| Practical sustained TPS | ~1,500 | — | — |
| Asset `transfer` TPS | ~5,400 | — | — |
| Shielded `transfer` TPS | ~200 | — | — |
| MDBX single-write throughput | ~50K writes/sec | — | — |
| P2P broadcast latency | ~10–50 ms per hop | — | — |
| Single-node RPC throughput | ~5,000 req/sec | — | Load balance across nodes |

---

## 6. Testing & Quality Assurance

### 6.1 Current State

- **~1,177 `#[test]` / `#[tokio::test]` annotations across ~100 files**
- **Unit tests:** Per-crate coverage for consensus (~179), protocol (~283),
  crypto (~40), EVM (~29), network (~81), storage (~46), RPC (~40),
  bridge (~41), shielded (~160), light-client (~50), agent (~106),
  governance (~51), oracle (~22), mempool (~46), node (~155), precompile (~58),
  chainspec (~25), serialization (~12), primitives (~20)
- **Integration tests:** Shielded flow, MDBX, P2P, light client sync,
  TLS/HTTPS, CORS
- **E2E tests:** Block production, node lifecycle, EVM compatibility, shielded
  E2E, malicious proposer, stress (1000 txs), fork upgrade, multi-node
  consensus, governance lifecycle, bridge E2E, light client, WebSocket (9
  channels), oracle, network partition, Byzantine faults, soak, chaos
- **Fuzzing:** 6 targets (`tx_rlp_decode`, `precompile_dispatch`,
  `balance_arithmetic`, `mpt_proof_verify`, `signature_recovery`,
  `block_header_validate`)
- **Mutation testing:** `.mutants.toml` profile configured, CI job available
  (manual trigger)
- **Property-based testing:** Proptest in light-client (5 invariants) and
  precompile dispatch (5 invariants)
- **Formal methods:** Kani model checking in CI
- **Benchmarks:** 7 criterion.rs benchmarks (precompile, block production,
  signature verify, MDBX read/write, proof generate, MPT verify, priority pool)

### 6.2 Remaining Gaps

| Gap | Priority | Details |
|-----|----------|---------|
| Real-network light client 7-day soak | High | Connect to live Ethereum RPC for extended header chain and receipt proof testing |
| Live Ethereum MPT proof tests | High | Validate tx inclusion and receipt proof verification against real mainnet headers |
| Long-running testnet | Medium | Persistent multi-validator testnet for soak and heterogeneous fork upgrade testing |
| Expanded fuzz targets | Medium | Extend beyond current 6 targets to cover more precompile dispatch, consensus message deserialization |

### 6.3 Recommended Test Infrastructure Improvements

- Run `--release` mode tests in CI (currently debug-only)
- Add benchmark regression alerts (fail CI if performance drops > 10%)
- Increase Kani model checking coverage
- Schedule regular mutation testing runs

---

## 7. Observability

### 7.1 Current State

- Prometheus `/metrics` endpoint on `:9090` (via `telemetry::server::start_metrics_server`)
- OpenTelemetry spans in hot paths: `record_block_span`, `record_tx_span`, `record_p2p_span`
  wired in `crates/node/src/block_producer.rs` and `bft_loop.rs`
- Structured logging (JSON/text format) with rotation
- OTel span emission verified by `CaptureExporter` in test
- Alerting thresholds documented in `docs/observability.md`

### 7.2 Gaps & Recommendations

| Gap | Recommendation |
|-----|---------------|
| Grafana dashboards | Implement per plan in [§4.4](#44-grafana-dashboards) |
| OTel span coverage | Add spans for precompile execution duration, MDBX read/write latency, RPC handler dispatch |
| Alerting rules | Configure Prometheus Alertmanager rules per thresholds in [`docs/benchmarks.md`](benchmarks.md) |

---

## 8. Developer Experience

### 8.1 CLI Polish

| Feature | Description |
|---------|-------------|
| `calld init` | Generate a config file interactively |
| `calld version` | Show full build info (git commit, build timestamp, rustc version) |
| `calld config validate` | Validate a config file without starting the node |

### 8.2 Crate-Level README

Each crate would benefit from a brief README covering its responsibilities and
key design decisions. Currently most crates lack this, leaving developers to
infer structure from code alone.

### 8.3 Changelog Standards

Adopt [Keep a Changelog](https://keepachangelog.com/) format for
[`CHANGELOG.md`](../CHANGELOG.md), categorizing changes as Added/Changed/
Deprecated/Removed/Fixed/Security.

---

## 9. Operations & Deployment

### 9.1 Docker

- Current `Dockerfile` uses multi-stage build
- Optimize further with distroless or debian-slim base image to reduce attack
  surface and image size

### 9.2 Production Readiness

| Area | Recommendation |
|------|---------------|
| Hot-reload config | Allow runtime updates to rate limits, allowlists, log levels without restart |
| Graceful shutdown | Ensure validators complete the current consensus round on SIGTERM before exiting |
| Snapshot service | Provide public snapshots for fast node bootstrapping (reduce sync from scratch) |
| Backup strategy | Document MDBX backup procedure (WAL archive + periodic full backup) |

### 9.3 Production Sizing

| Deployment | Validators | CPU | RAM | Disk | Network |
|------------|-----------|-----|-----|------|---------|
| Devnet / testnet | 1 | 4 cores | 16 GB | 500 GB SSD | 100 Mbps |
| Testnet with dApps | 3 | 8 cores | 32 GB | 1 TB NVMe | 1 Gbps |
| Mainnet | 21 | 16 cores | 64 GB | 2 TB NVMe | 10 Gbps |

---

## 10. Ecosystem & Protocol Layer

### 10.1 RPC Compatibility

The node exposes standard JSON-RPC over HTTP (`:8545`) and WebSocket (`:8546`).
As the ecosystem grows, consider:

- Support for additional `eth_*` methods to improve tool compatibility
- WalletConnect integration
- EIP-1193 provider standard
- Hardhat / Foundry templates for contract developers on Callchain

### 10.2 Precompile Expansion

Current precompile range: `0x101`–`0x209`. Future additions may include:

- BLS signature verification precompile (EIP-2537)
- Additional curve operations
- Ecosystem-driven precompiles as demand emerges

---

## 11. Dependency Management

Locked dependencies in `Cargo.toml` require regular attention:

| Dependency | Version | Notes |
|-----------|---------|-------|
| commonware-* | 2026.3.0 / 2026.4.0 | Simplex BFT consensus, P2P, crypto |
| reth-* | git rev `a550b7a` | Storage, EVM, RPC, trie |
| revm | 36.0.0 | EVM execution engine |
| alloy-* | 1.5.7 / 1.8.2 | Ethereum types and RPC |
| halo2_proofs | 0.3 | ZK proving |

**Recommended:** Monthly dependency upgrade cycle. Prioritize security patches.
Establish a "dependency upgrade PR" process (upgrade → run full test suite →
benchmark comparison).

---

## 12. Recently Resolved

The following items were completed during the 2026-04 to 2026-05 hardening pass:

### Governance

| Gap | Resolution |
|-----|------------|
| `apply_proposal` only logged for most proposal types | Now applies real internal state changes for all 10 proposal types |
| Governance state persistence | Persisted to MDBX; `executor` and `balance_source` rewired after load |
| ForkManager integration for ProtocolUpgrade | Wired via `NodeProposalExecutor` |

### Prover Key Rotation

| Component | Status |
|-----------|--------|
| `ProverRegistry` with `RwLock<HashMap<KeyVersion, VersionedKeys>>` | Implemented |
| `ZkProof.key_version` field | Implemented |
| `Halo2Prover::global()` versioned key management | Implemented |
| Governance proposal type for `ProverKeyRotation` | Implemented |
| Auto-pickup in `LightClientService` | Implemented |

### Security & Testing (34 items resolved)

A comprehensive documentation audit resolved all gaps in:

- TLS/HTTPS tests (`crates/rpc/tests/tls_integration.rs`)
- MDBX integration tests (`crates/storage/tests/mdbx_integration.rs`)
- Network partition tests (`crates/node/tests/test_network_partition.rs`)
- Byzantine faults tests (`test_byzantine_faults.rs`)
- Light client BLS consensus verification tests
- Oracle signature verification negative tests
- Database migration framework
- Receipt persistence (`crates/node/src/state_persist.rs`)
- Agent E2E tests (`test_agent_e2e.rs`)
- Upgrade persistence tests
- CI/CD pipeline (`.github/workflows/ci.yml`)
- OTel span wiring in production paths
- Slashing economic penalty tests
- Snapshot production/verification tests
- Fast sync incremental catch-up tests
- Rate limiting tests
- WebSocket lag handling tests
- CORS configuration tests
- Mempool eviction tests
- P2P ban enforcement tests
- `eth_getLogs` performance tests (100K / 1M blocks)
- Fuzz testing crate with 6 targets
- Benchmark suite with 7 criterion benchmarks
- Property-based testing (proptest)
- Mutation testing configuration

### Subsystem Documentation

The following subsystem docs claim "all gaps resolved" or "production ready"
for their *implementation scope*. These assessments are accurate for the code
itself but do not account for the cross-cutting integration, testing, and
infrastructure gaps tracked in this document:

- `docs/protocol.md`
- `docs/bridge.md`
- `docs/security.md`
- `docs/shielded.md`
- `docs/compliance.md`
- `docs/agent.md`
- `docs/upgrade.md`
- `docs/rpc.md`

---

## Priority Ordering (Recommended Execution Sequence)

Based on impact and dependency, the recommended order of work is:

1. **Third-party security audit** — one-time investment, prerequisite for mainnet
2. **MEV protection** — medium complexity, essential for credible neutrality
3. **Testnet deployment** — ongoing value, finds real-world bugs
4. **Grafana dashboards** — low effort, high ops value
5. **BLS consensus verification** — high effort, necessary for trustless bridge
6. **MDBX serialization optimization** — moderate effort, reduces storage/latency
7. **Shielded circuit formal verification** — high effort, defer to audit results
8. **System contracts migration** — long-term, only if ecosystem demands it
