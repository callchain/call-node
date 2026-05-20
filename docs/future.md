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

**Status:** ~~Deferred~~ **Infrastructure Implemented** (2026-05-10)

**Completed:**

1. **`ProverRegistry`** (`crates/shielded/src/key_registry.rs`)
   - `RwLock<HashMap<KeyVersion, VersionedKeys>>` replaces `OnceLock`
   - Runtime key registration without restart
   - Monotonic version enforcement (prevents downgrade attacks)
   - `sunset_older_than(Duration)` cleanup for expired versions

2. **`ZkProof.key_version`** field
   - Proofs are tagged with the key version used to generate them
   - Validators look up the correct VK via `Halo2Prover::for_version()`
   - Backward-compatible: missing/0 defaults to genesis keys

3. **`Halo2Prover::global()` versioned key management**
   - Boot-time generation of universal params and circuit keys as version 0
   - Proving server picks up current keys automatically

**Completed (2026-05-10):**

4. **Governance proposal type for `ProverKeyRotation`** (`crates/governance/src/types.rs`, `precompile.rs`)
   - Proposal payload: new VK hashes, ceremony attestation, activation block height
   - Precompile stores rotation metadata in EVM storage on execution

5. **Auto-pickup in `LightClientService`** (`crates/node/src/light_client_service.rs`)
   - `check_prover_key_rotation()` polls governance storage at each epoch boundary
   - Reads `prover_key_rotation/pending` flag and `version` from EVM state
   - Calls `try_register_prover_keys(version)` to load keys from `/var/lib/callchain/shielded_keys_v{version}`
   - Tracks `last_applied_key_version` to avoid duplicate registration
   - No-op when `production-keys` feature is not enabled

**Status:** ~~Deferred~~ **Fully Implemented**

**When to revisit:** Before mainnet shielded pool launch for operational testing
of the full governance → node → proof lifecycle.

---

## Shielded Circuit Formal Verification

**Status:** **Deferred — Lean artifacts deleted during Halo2 migration** (2026-05-15)

**History:**

A Lean 4 formalization of the Groth16/R1CS shielded circuits was previously developed in `formal_verification/lean/`:
- Mathlib4 (`ZMod BN254_P`) integrated for field arithmetic
- `DepositCircuit.lean`, `TransferCircuit.lean`, `WithdrawCircuit.lean`: completeness and soundness theorems stated
- `docs/formal_verification/spec.md`: mathematical specification of BN254 Fr, Poseidon hash, R1CS satisfaction

**Post-migration status:**

The Lean 4 formalization modeled the pre-Halo2 Groth16/R1CS implementation. During the Halo2 migration:
- `formal_verification/lean/` was deleted (no longer applicable to PLONKish arithmetization)
- `docs/formal_verification/spec.md` was deleted (R1CS-specific, no longer relevant)
- The R1CS structural correspondence tools (`export_r1cs.rs`, `verify_r1cs.rs`) were removed

New formal verification for Halo2 circuits (PLONKish arithmetization, custom gates, permutation arguments) is **future work** and would require a ground-up effort.

| Gap | Status | Description |
|-----|--------|-------------|
| 1. Lean ↔ Rust correspondence (R1CS) | ❌ Deleted | Historical Groth16 formalization; artifacts removed |
| 2. Halo2 circuit formalization | ⏳ Future work | PLONKish constraints, custom gates, permutation arguments |
| 3. Range check full expansion | ✅ Closed | Halo2 uses `halo2_gadgets` range check (production-proven in Orchard) |

**When to revisit:**
- Third-party audit should review Halo2 circuit constraints directly (no theorem-prover proofs currently exist)

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
