# Future Work

Items deferred from the 2026-05-06 system review. These are not blockers for
devnet/testnet but should be addressed before mainnet readiness.

---

## EthLightClient BLS Consensus Verification

**Context:** `crates/light-client` Ethereum light client currently verifies headers
via parent-hash chain only. It does not verify Ethereum consensus layer BLS
aggregate signatures from the beacon chain sync committee.

**Why deferred:** The parent-hash chain + `set_finalized_block()` checkpoint
tracking is sufficient for bridge deposit validation on devnet and testnet.
Full consensus verification requires integrating Ethereum beacon chain light
client sync (Altair sync committees, ~512 validators per period), which is a
significant scope increase.

**When to revisit:** Before mainnet bridge launch. At that point the bridge
must trustlessly verify Ethereum finality without relying on an externally-set
checkpoint.

---

## Prover Key Rotation

**Context:** `call-shielded::prover::RealProver` uses `OnceLock` global static
for proving/verification keys. There is no runtime mechanism to rotate keys
without restarting the prover service.

**Why deferred:** Key rotation requires a governance-driven ceremony
(coordinated trusted setup, new CRS distribution, verifying-key hash update in
code). This is a mainnet-readiness procedure, not a devnet/testnet concern.

**When to revisit:** Before mainnet shielded pool launch. Plan:
1. Governance proposal type for `ProverKeyRotation`
2. Ceremony coordination (offline MPC)
3. Service hot-reload of new verification key
4. Old key sunset period for in-flight proofs

---

## Shielded Circuit Formal Verification

**Context:** `crates/shielded` deposit, transfer, and withdraw circuits are
exercised by comprehensive R1CS constraint-level negative tests (132 tests,
`real-prover` feature). These prove the circuits reject invalid witnesses,
but they do not constitute a mathematical proof of completeness or soundness.

**What is missing:** A theorem-prover-level formal specification and proof
(e.g., in Coq, Isabelle/HOL, or a ZK-specific framework) that:
1. The R1CS constraint system exactly captures the intended relation
2. Every valid witness satisfies all constraints (completeness)
3. No invalid witness satisfies all constraints (soundness)
4. The Merkle tree gadget is collision-resistant under Poseidon
5. The nullifier derivation is a pseudo-random function

**Why deferred:** Formal verification of a non-trivial ZK circuit is
research-grade work requiring months of specialist effort. The pragmatic
constraint-level tests are sufficient for devnet and testnet where the
economic value at risk is low.

**When to revisit:** Before mainnet shielded pool launch. At that point a
third-party audit should include either:
- A full formal verification engagement, or
- A rigorous pen-and-paper security proof reviewed by domain experts

---

## Grafana Dashboards

**Context:** The node exposes a Prometheus-compatible `/metrics` endpoint on
`:9090` (via `telemetry::server::start_metrics_server`). All consensus,
mempool, P2P, storage, and latency metrics are already emitted. However, there
are no pre-built Grafana JSON dashboard files checked into the repository.

**Why deferred:** Grafana is a deployment-layer concern. The metrics schema is
stable and self-describing (Prometheus text format with `# HELP` and `# TYPE`
annotations). Operators can import metrics in a few minutes using Grafana's
built-in Prometheus data source and query builder. A curated dashboard is a
convenience, not a code correctness blocker.

**When to revisit:** Before public testnet launch, when operators will benefit
from a drop-in dashboard. Plan:
1. Create `docs/observability/grafana/` directory with JSON dashboard exports
2. Dashboards to include:
   - **Consensus Overview**: blocks produced/committed, rounds, timeouts, latency p50/p95/p99
   - **Mempool Health**: tx count, rejected rate, bridge pending, fee history
   - **P2P Network**: peer count, bytes sent/received, message latency
   - **Storage / Pruning**: traces/receipts/bodies/snapshots pruned
   - **Node Health**: uptime, last block age (for stall detection)
3. Add Grafana provisioning YAML for automatic dashboard loading
4. Document data source configuration in `docs/observability.md`

---

## MEV Protection

**Context:** `crates/protocol` contains a commit-reveal library for sealed-bid
submission, but it is not integrated into block production. Validators can
inspect the mempool and reorder or front-run transactions for profit.

**Why deferred:** MEV protection requires protocol-level changes (commit-reveal
timing, encrypted mempool, fair ordering) that complicate the consensus-critical
path. For devnet and testnet, the economic value at risk is low and operator-run
validators are trusted.

**When to revisit:** Before mainnet launch. Plan:
1. Integrate commit-reveal into `BlockProducer` tx selection
2. Add encrypted mempool layer (threshold encryption or time-lock puzzles)
3. Fair ordering: FCFS within a block or deterministic shuffle
4. Penalize validators that violate ordering rules

---

## System Contracts

**Context:** All protocol logic (staking, assets, governance, shielded pool,
bridge, oracle) is currently implemented as Rust precompiles (0x201–0x209).
There is no plan to migrate to Solidity system contracts.

**Why deferred:** Rust precompiles are more auditable, gas-efficient, and
integrate cleanly with the consensus layer. Solidity system contracts would
require a full rewrite, new tooling, and additional audit surface. This is a
long-term architectural question, not a testnet blocker.

**When to revisit:** Post-mainnet, if ecosystem demand for Solidity-level
composability justifies the migration cost. Plan:
1. Formalize the precompile ↔ Solidity interface mapping
2. Implement each precompile as a delegating Solidity proxy
3. Governance-driven migration with backward compatibility period
4. Deprecate Rust precompiles once usage drops below threshold
