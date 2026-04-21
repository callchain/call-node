# Callchain Production Readiness Checklist

This document tracks remaining production gaps derived from the 11 module audits in `docs/`.
Fixed items have been removed. Status: Phase 0/2/5 complete.

---

## Phase 0: Security — **COMPLETE**

All four signature/cryptography gaps are resolved.

---

## Phase 1: Make State Persistent (Storage)

| Priority | Action | Files |
|----------|--------|-------|
| **P1** | Implement WAL / crash recovery | `crates/storage/src/db.rs` |
| **P1** | Persist receipts to MDBX (table defined, no save/load helpers) | `crates/storage/src/reth_db.rs` |
| **P1** | Persist agent state to MDBX | `crates/agent/src/registry.rs`, `storage/src/reth_db.rs` |
| **P1** | Persist ForkManager / rollback state to MDBX | `crates/consensus/src/fork.rs`, `storage/src/reth_db.rs` |
| **P1** | Persist shielded nullifier set to MDBX | `crates/shielded/src/lib.rs`, `storage/src/reth_db.rs` |
| **P1** | Persist validator set to MDBX | `crates/consensus/src/validator.rs`, `storage/src/reth_db.rs` |

**Why now:** Every other system (bridge, agent, shielded, consensus) depends on durable state.

---

## Phase 2: RPC Security — **COMPLETE**

Rate limiting, TLS, governance refactored to tx submission — all resolved.

---

## Phase 3: Make Consensus Economically Safe

| Priority | Action | Files |
|----------|--------|-------|
| **P3** | Gossip scheduled upgrades across the network | `crates/network/src/gossip.rs`, `consensus/src/fork.rs` |

---

## Phase 4: Fix the Broken Subsystems

| Priority | Action | Files |
|----------|--------|-------|
| **P4** | **Light Client:** Check bridge event signature hash (`topics[0]`) | `crates/light-client/src/ethereum.rs` |
| **P4** | **Shielded:** Persist nullifier set to MDBX | `crates/shielded/src/lib.rs`, `storage/src/reth_db.rs` |

---

## Phase 5: Harden Operations — **COMPLETE**

Health checks, alerting, metrics, histograms, audit log — all wired.

---

## Phase 6: Test Coverage

| Priority | Action |
|----------|--------|
| **P6** | Property-based tests for tx decoding, MPT proofs, instruction parsing (`proptest`) |
| **P6** | Byzantine consensus tests: network partitions, equivocation, delayed messages |
| **P6** | Signature verification negative tests for every authenticated endpoint |
| **P6** | MDBX integration tests: concurrent writes, crash recovery, compaction |
| **P6** | Load test: sustained 1000 TPS for 1 hour + memory profiling |
| **P6** | Coverage reporting (`cargo llvm-cov` in CI) |

---

## Summary

| Phase | Status | Remaining |
|-------|--------|-----------|
| 0 — Security | ✅ Complete | 0 |
| 1 — Storage | 🟡 Partial | 6 gaps |
| 2 — RPC Security | ✅ Complete | 0 |
| 3 — Consensus | 🟡 Partial | 1 gap |
| 4 — Subsystems | 🟡 Partial | 2 gaps |
| 5 — Operations | ✅ Complete | 0 |
| 6 — Testing | 🟡 Partial | 6 gaps |
