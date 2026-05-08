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
