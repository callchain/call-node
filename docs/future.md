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

**Status:** ~~Deferred~~ **In Progress — Core Theorems Complete** (2026-05-10)

**Completed:**

1. **Lean 4 theorem prover integration** (`formal_verification/lean/`)
   - Mathlib4 (`ZMod BN254_P`) integrated for field arithmetic
   - `Fr.lean`: Custom field structure eliminated; only 1 mathematical axiom remains (`Nat.Prime BN254_P`)
   - All 6 previous axioms proven from `Field` instance

2. **Circuit formalization + proofs** (zero `sorry`, `lake build` passes)
   - `DepositCircuit.lean`: Completeness and soundness theorems proven
   - `TransferCircuit.lean`: Completeness and soundness theorems proven (N=2, M=2)
   - `WithdrawCircuit.lean`: Completeness and soundness theorems proven

3. **Lean ↔ Rust R1CS correspondence (Gap 1)** — Historical
   - The Lean models formalized the Groth16/R1CS implementation (pre-migration)
   - `export_r1cs.rs` and `verify_r1cs.rs` binaries were deleted during Halo2 migration
   - `R1CSCorrespondence.lean` documents the historical refinement relationship
   - **Formal verification for Halo2 PLONKish circuits is future work**

4. **Implementation alignment** (pre-migration)
   - Transfer circuit export was N=1,M=1 → **N=2,M=2** (matching Lean model)
   - Withdraw circuit included `spending_key` witness + spending-rights constraint (matching Lean)

**Post-migration status:**

The Lean 4 formalization modeled the Groth16/R1CS implementation. After migrating to Halo2:
- The R1CS structural correspondence tools no longer exist
- New formal verification for Halo2 circuits (PLONKish arithmetization) is **future work**
- The existing Lean proofs remain as a mathematical model of the historical implementation

| Gap | Status | Description |
|-----|--------|-------------|
| 1. Lean ↔ Rust correspondence (R1CS) | ✅ Complete (historical) | Modeled the Groth16 implementation; no longer applicable |
| 2. Halo2 circuit formalization | ⏳ Future work | PLONKish constraints, custom gates, permutation arguments |
| 3. Range check full expansion | ✅ Closed | Halo2 uses `halo2_gadgets` range check (production-proven in Orchard) |

**When to revisit:**
- Close Gap 3: Prove in Lean that the expanded boolean/packing constraints are equivalent to `value < 2^128`
- Third-party audit should review the completeness/soundness proofs and structural correspondence claims

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
