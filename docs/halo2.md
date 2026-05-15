# Halo2 Recursive Proofs: Post-Migration Documentation

**Date**: 2026-05-15
**Context**: Callchain completed migration from Groth16/BN254 to Halo2/Pasta on 2026-05-15. This document documents the new architecture and explores future capabilities enabled by Halo2.

---

## Table of Contents

1. [What is Halo2](#1-what-is-halo2)
2. [What is Recursive Proof](#2-what-is-recursive-proof)
3. [User Payment Flow with Recursive Aggregation](#3-user-payment-flow-with-recursive-aggregation)
4. [Recursive vs Batch Verification](#4-recursive-vs-batch-verification)
5. [Technical Barriers to Implementation](#5-technical-barriers-to-implementation)
6. [Compliance Proof in Halo2](#6-compliance-proof-in-halo2)
7. [Conclusion and Recommendation](#7-conclusion-and-recommendation)

---

## 1. What is Halo2

Halo2 is a zero-knowledge proving system developed by zcash, built on the PLONK arithmetization with several key differences from Groth16:

| Property | Before (Groth16) | Current (Halo2) |
|----------|-----------------|-----------------|
| Arithmetization | R1CS (Rank-1 Constraint System) | PLONKish (Custom gates + lookup tables + permutation) |
| Trusted Setup | Per-circuit trusted setup required | No setup (IPA) |
| Proof Size | ~128B | ~5-10KB |
| Verification | ~3ms (BN254 pairing) | ~5-10ms (Halo2 IPA) |
| Recursive Composition | Difficult | Native support via folding |
| Lookup Tables | Not supported | Native support |
| Range Checks | Bit decomposition (~150 constraints) | Custom gate (~few constraints) |

### Why Halo2 Matters for Callchain

Halo2 enables entirely new capabilities beyond what Groth16 could provide:

- **Recursive proofs**: Verify a proof inside another proof, enabling aggregation
- **Lookup tables**: Efficient set membership proofs (KYC/whitelist)
- **No trusted setup** (IPA mode): Iterate circuits without re-running ceremonies
- **Custom gates**: Tailor constraint types to specific operations (e.g., Poseidon, range checks)

---

## 2. What is Recursive Proof

### Analogy: School Papers

Imagine a school with 1,000 students, each writing a paper:

- **Normal verification (independent)**: The principal reads all 1,000 papers. Cost = O(N).
- **Batch verification**: The principal hires 10 assistants, each reads 100 papers and reports. Cost still = O(N), just with parallelism.
- **Recursive verification (Halo2)**: Student A reads Student B's paper and writes a "summary proof" that B's paper is valid. Student C reads Student D's paper and writes another summary. Then Student E reads A's and C's summaries and writes a merged summary. Eventually, the principal reads **one** final summary and knows all 1,000 papers are valid. Cost = O(1).

### Definition

> A **recursive proof** is a proof that attests to the validity of **another proof**.

```
Proof_A: "Alice's transfer is valid"
Proof_B: "Proof_A is a valid proof"
Proof_C: "Proof_B is a valid proof"
...
RootProof: "All proofs in this batch are valid"
```

The key insight: **"Verifying a proof" is just a computation, and any computation can be put into a ZK circuit.**

---

## 3. User Payment Flow with Recursive Aggregation

### Phase 1: User Side (Unchanged)

```
Alice wants to send 100 CALL to Bob (shielded)

1. Wallet retrieves unspent notes from local storage
2. Generates output notes (for Bob + change for self)
3. Builds witness: spending_key, merkle_paths, notes...
4. Calls prover (local or call-prover HTTP service)
5. Receives TransferProof: ~200 bytes
   - public inputs: nullifiers[], commitments[], asset_id
   - proof: zk-SNARK proof data
6. Submits proof to mempool
```

**Alice's workload is identical to today.** Recursion does not add user burden.

### Phase 2: Aggregator Side (New)

```
Aggregator node (validator subset or dedicated prover cluster):

Every 2–5 seconds (or every N proofs collected):

┌─────────────────────────────────────────────────────────┐
│  1. Pull pending proofs from mempool                    │
│     [Proof_A, Proof_B, Proof_C, ...]                    │
│                                                         │
│  2. For each proof, "verify and wrap":                  │
│     Verify(Proof_A) → generate Proof_A'                 │
│     Verify(Proof_B) → generate Proof_B'                 │
│     ...                                                 │
│                                                         │
│  3. Recursively merge:                                  │
│     Merge(Proof_A', Proof_B') → Proof_AB                │
│     Merge(Proof_AB, Proof_C') → Proof_ABC               │
│     ...                                                 │
│                                                         │
│  4. Final RootProof                                     │
│     Size: ~1–2KB                                        │
│     Verification time: ~10ms                            │
└─────────────────────────────────────────────────────────┘
```

**Key**: Proof_A' is not a copy of Proof_A — it is a **new proof** whose statement is:

> *"I (the aggregator) have verified that there exists a valid TransferProof proving Alice's transfer is legitimate."*

### Phase 3: On-Chain Side

```
Block N:
├─ Regular EVM transactions: [tx1, tx2, ...]
├─ Shielded aggregation:
│   - RootProof: 1 (~1–2KB)
│   - Public inputs:
│     * All nullifiers (public)
│     * All commitments (public)
│     * asset_id (public)
│     * Aggregator signature/identity (anti-DoS)
└─ ...

Validator verifies block:
  1. Execute regular EVM transactions
  2. Verify RootProof (~10ms, one pairing check)
  3. Update nullifier set + Merkle tree
  4. Done
```

### Network Data Flow

```
User A                    User B                    User C
  │                        │                        │
  ▼                        ▼                        ▼
┌─────────┐          ┌─────────┐          ┌─────────┐
│ Transfer│          │ Transfer│          │ Transfer│
│ Proof   │          │ Proof   │          │ Proof   │
└────┬────┘          └────┬────┘          └────┬────┘
     │                    │                    │
     └────────────────────┼────────────────────┘
                          ▼
                 ┌─────────────────┐
                 │    Mempool      │
                 │ (shielded pool) │
                 └────────┬────────┘
                          │
                          ▼
                 ┌─────────────────┐
                 │   Aggregator    │
                 │  (recursive     │
                 │   folding)      │
                 └────────┬────────┘
                          │
                          ▼
                 ┌─────────────────┐
                 │  Single Proof   │
                 │  to chain       │
                 └─────────────────┘
```

---

## 4. Recursive vs Batch Verification

| Aspect | Batch Verification | Recursive SNARKs |
|--------|-------------------|------------------|
| **Principle** | Random linear combination of N verification equations | Put "verify proof A" into a circuit, generate proof B |
| **Verification Cost** | O(N) — scales with transaction count | O(1) — constant time, independent of tx count |
| **Proof Size** | O(1) — single proof | O(1) — single proof |
| **Intermediate Data** | None needed | Recursive intermediate proofs (aggregator generates) |
| **Latency** | No extra latency | Aggregator aggregation time (seconds to tens of seconds) |
| **Decentralization** | Anyone can batch | Aggregator can be centralized service or decentralized rotation |

### Performance Comparison for Callchain

| Metric | Current (Halo2, 50 tx/block) | Batch Halo2 | Recursive Halo2 |
|--------|------------------------------|-------------|-----------------|
| On-chain verification | ~250-500ms (50 × 5-10ms) | ~150-250ms | ~10ms |
| P2P proof propagation | ~250-500KB (50 × 5-10KB) | ~250-500KB | 1–2KB |
| Block time consumed | 100-200% | 60-100% | 4% |
| Aggregator workload | None | None | +30–120s per batch |

---

## 5. Technical Barriers to Implementation

### Barrier 1: Circuit Rewrite (Largest Workload)

Callchain's 3 circuits (Deposit/Withdraw/Transfer) were originally written in **arkworks R1CS** and have been rewritten to **Halo2 PLONKish**:

```rust
// Before: R1CS (Groth16)
let ivk_var = FpVar::new_witness(cs.clone(), || Ok(bytes_to_fr(&ivk)))?;
let nf_var = poseidon_hash_gadget(cs.clone(), &[tag, fvk_var, rho_var])?;
nf_var.enforce_equal(&computed_nf)?;
```

Halo2 uses **PLONKish** custom gates:

```rust
// Halo2: Custom gates + lookup + permutation
meta.create_gate("nullifier_check", |meta| {
    let ivk = meta.query_advice(ivk_col, Rotation::cur());
    let rho = meta.query_advice(rho_col, Rotation::cur());
    let nf = meta.query_instance(nf_col, Rotation::cur());
    vec![nf - poseidon_hash(ivk, rho)]
});
```

**Estimated effort**: 3 circuits × ~100–300 lines of constraint code ≈ **2–4 weeks** (with experience).

### Barrier 2: Recursive Verifier Circuit (Hardest Cryptographic Engineering)

Recursive proof requires **verifying a proof inside another circuit**:

```
Circuit A: "Alice's transfer is valid"
Circuit B: "Circuit A's proof is valid"
```

Circuit B must implement in constraints:
- Elliptic curve point operations (to check proof points)
- Pairing function (for pairing check)
- Hash-to-curve (for transcript generation)

These operations are simple in native code but **extremely expensive in circuit constraints**:
- One BN254 pairing: tens of thousands to hundreds of thousands of constraints
- Verifying a Halo2 proof inside a Halo2 circuit: requires implementing the Halo2 verifier algorithm

**Mitigation strategies**:
- **Nova/Supernova folding**: Instead of verifying a full proof, "fold" two instances into one. Verifier circuit is much smaller (~10k constraints).
- **Cycle of curves** (e.g., Pasta curves): Prove on one curve that verification on another curve succeeded. Avoids non-native field arithmetic.

### Barrier 3: Curve Choice and EVM Compatibility

| Curve | Halo2 Support | On-Chain Verification |
|-------|--------------|----------------------|
| Pasta (Vesta/Pallas) | Optimal | No precompile — nearly impossible |
| BN254 | Feasible | Yes — `ecPairing` precompile available |

**Trade-off**:
- If Callchain keeps **L1 native verification** (current `ShieldedPrecompile` verifies inside EVM), must use BN254.
- If willing to switch to **rollup mode** (validity proof submitted to Callchain), can use Pasta.

Using BN254 with Halo2 sacrifices some optimizations (especially cycle of curves), making recursive verifier circuits larger.

### Barrier 4: Rust Ecosystem Maturity

| Library | Status |
|---------|--------|
| `halo2` (zcash) | Stable but API changes frequently; documentation is sparse |
| `halo2_proofs` | Core library, production-grade (used by Orchard) |
| `halo2_gadgets` | Basic gadgets (Poseidon, SHA256) — sufficient |
| `halo2_recursive` / `halo2_folding` | Experimental, APIs unstable |
| `nova-snark` | Relatively mature; implements folding but not Halo2-specific |

Halo2 core (`halo2_proofs`, `halo2_gadgets`) is production-grade (used by Zcash Orchard). Recursive/folding libraries are **rapidly evolving** — production deployment of recursion carries some ecosystem maturity risk.

### Barrier 5: Proving Key and Ceremony Infrastructure

The Groth16 `production-keys` feature and Powers of Tau ceremony scripts have been removed. Halo2 IPA mode requires no ceremony.

Halo2 key management is different:
- **Before (Groth16)**: Per-circuit trusted setup (PK/VK bound to specific circuit)
- **Current (Halo2 IPA)**: No trusted setup; `Params::new(k)` generates universal parameters deterministically
- **Future (Halo2 KZG)**: Universal SRS serves all circuits; smaller proofs but needs structured reference string

**Effort**: No ceremony needed for IPA. Switching to KZG later would require a public SRS (e.g., Ethereum's KZG ceremony) but the same circuits work unchanged.

### Barrier 6: Performance Reality

Recursive proof is not free:

| Metric | Non-Recursive | Recursive Aggregation (50 tx) |
|--------|--------------|------------------------------|
| User proof generation | 1–3s | **Unchanged** (still 1–3s) |
| Aggregator workload | None | **+30–120s** (fold/aggregate 50 proofs) |
| On-chain verification | 150ms | **~10ms** |
| Proof size (P2P) | 10KB | **~1–2KB** |
| End-to-end latency | Block time 250ms | **+ aggregator delay 5–30s** |

**Trade-off**: On-chain verification is faster, but the aggregator becomes a new bottleneck and trust point.

---

## 6. Compliance Proof in Halo2

Halo2 is **better suited** for compliance proofs than Groth16 due to native lookup tables and efficient range checks.

### Why Halo2 Excels at Compliance

| Feature | Groth16 Cost | Halo2 Cost |
|---------|-------------|------------|
| Set membership (KYC/whitelist) | Merkle proof: ~3,200 constraints | Lookup table: **O(1), ~few constraints** |
| Range check (amount bounds) | Bit decomposition: ~150 constraints | Custom gate: **~few constraints** |
| Proof of fund age | Range check + Merkle: ~3,500 constraints | Range check + lookup: **~hundreds** |

### Compliance Proof Types Halo2 Enables

**Type 1: ZK Membership Proof (Proof of Innocence)**

```
Public:  whitelist_merkle_root, compliance_commitment
Private: source_address, merkle_path, transfer_witness

Constraints:
  - source_address ∈ whitelist_tree (via lookup or Merkle path)
  - transfer_witness satisfies all TransferCircuit constraints
  - public inputs (asset_id, value) are consistent between both circuits
```

Halo2 can combine `TransferCircuit` + `ComplianceCircuit` into a **single circuit**, proved in one shot.

**Type 2: Source Age Proof**

```
Public:  min_age_days (e.g., 90)
Private: deposit_block_height, current_block_height, note_path

Constraints:
  - current_block_height - deposit_block_height ≥ min_age_days
  - note exists in shielded Merkle tree at deposit_block_height
```

Halo2's native range check makes the age constraint almost free.

**Type 3: Selective Disclosure Circuit**

```
Public:  disclosed_amount, disclosed_asset_id, auditor_public_key
Private: full_note, viewing_key

Constraints:
  - note_commitment = Poseidon(value, asset_id, rcm, rho)
  - disclosed_amount == note.value
  - disclosed_asset_id == note.asset_id
  - auditor_can_decrypt(encrypted_note, auditor_pk) == true
  - nullifier derived correctly (proves spender authority)
```

Unlike giving the auditor the full `viewing_key` (which reveals all history), this circuit proves **only specific properties** about one note.

### Recursive + Compliance Combined

```
┌─────────────────┐   ┌─────────────────┐   ┌─────────────────┐
│ Transfer Proof  │   │ Compliance Proof│   │ Deposit Proof   │
│ (Halo2 circuit) │   │ (Halo2 circuit) │   │ (Halo2 circuit) │
└────────┬────────┘   └────────┬────────┘   └────────┬────────┘
         │                     │                     │
         └─────────────────────┼─────────────────────┘
                               │
                  ┌─────────────▼─────────────┐
                  │   Recursive Aggregator    │
                  │   (Halo2 folding)         │
                  │                           │
                  │   Verifies:               │
                  │   - transfer is valid     │
                  │   - compliance is valid   │
                  │   - deposit is valid      │
                  └─────────────┬─────────────┘
                                │
                  ┌─────────────▼─────────────┐
                  │    Single Final Proof     │
                  │    (~1–2KB)               │
                  └───────────────────────────┘
```

The chain verifies **one recursive proof** that simultaneously attests to:
- Transfer validity (no double spend, value conservation)
- Compliance (source is whitelisted / aged / etc.)
- Deposit validity (note format is correct)

---

## 7. Conclusion and Recommendation

### Summary of Barriers

| Barrier | Difficulty | Effort | Workaround Available? |
|---------|-----------|--------|----------------------|
| Circuit rewrite (R1CS → Plonkish) | Medium | 2–4 weeks | No — must do |
| Recursive verifier circuit | **High** | 4–8 weeks | Use Nova folding to simplify |
| Curve choice (BN254 vs Pasta) | Medium | Architecture decision | Use BN254, sacrifice some efficiency |
| Rust ecosystem immaturity | Medium | Ongoing | Wait 6–12 months for stability |
| Ceremony / SRS migration | Low | 1–2 weeks | No — must do |
| Aggregator infrastructure | Medium | 2–3 weeks | No — must build |

### Migration Completed

Callchain completed the Groth16 → Halo2 migration in May 2026. The migration was justified by:

1. **Elimination of trusted setup**: No more Powers of Tau ceremony burden
2. **Future-proofing**: Pasta curves + Halo2 enable recursive composition when tx volume grows
3. **Compliance readiness**: Native lookup tables for KYC/whitelist proofs
4. **Ecosystem maturity**: `halo2_proofs` and `halo2_gadgets` are production-grade (Orchard-proven)

**Trade-offs accepted**:
- Proof size increased from ~128B to ~5-10KB
- Verification time increased from ~3ms to ~5-10ms per proof
- No EVM native precompile — verification runs in native Rust precompile

### Current State

```
Completed: Halo2 IPA for deposit/transfer/withdraw circuits
  - 3 circuits rewritten from R1CS to PLONKish
  - Pasta Poseidon for commitments/nullifiers
  - Universal params via Params::new(k) — no ceremony
  - Key rotation via governance proposals

Future work (not yet implemented):
  - Recursive proof aggregation (Nova/Supernova folding)
  - Compliance lookup circuits
  - KZG commitment switch (smaller proofs, same circuits)
```

> **Bottom line**: Halo2 migration is complete. The system is ready for recursive aggregation and compliance proofs when requirements mature.
