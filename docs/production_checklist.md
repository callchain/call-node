# Callchain Production Readiness Checklist

This document tracks remaining production gaps derived from the 11 module audits in `docs/`.

---

## Phase 0: Security — **COMPLETE**

Signature verification, cryptography, and key management resolved.

---

## Phase 1: Storage — **COMPLETE**

All persistence wired in `persist_state_to_db` / `load_state_from_db` (`crates/node/src/lib.rs`):
balances, EVM, bridge, shielded (incl. nullifiers), validators, agents (registry + balances + nonces), oracle, governance, compliance, consensus, receipts, fork state. Checkpoint-based crash recovery via `CallCheckpoint` table.

---

## Phase 2: RPC Security — **COMPLETE**

Rate limiting, TLS, governance refactored to tx submission.

---

## Phase 3: Consensus — **COMPLETE**

Upgrade gossip implemented via `UpgradeAnnouncement` in `call_network`, broadcast after `next_upgrade()` check, received and processed via `UPGRADE_CHANNEL`.

---

## Phase 4: Subsystems — **COMPLETE**

Light client bridge event signature verified with actual keccak256 hash. Shielded nullifier set persisted to MDBX.

---

## Phase 5: Operations — **COMPLETE**

Health checks, alerting, metrics, histograms, audit log — all wired.

---

## Phase 6: Test Coverage

| Priority | Action |
|----------|--------|
| **P6** | Byzantine consensus tests: network partitions, equivocation, delayed messages |
| **P6** | Load test: sustained 1000 TPS for 1 hour + memory profiling |

Fixed in this phase: signature negative tests (7), proptest roundtrip (2), MDBX integration tests (7), CI coverage job.

---

## Summary

| Phase | Status | Remaining |
|-------|--------|-----------|
| 0 — Security | ✅ Complete | 0 |
| 1 — Storage | ✅ Complete | 0 |
| 2 — RPC Security | ✅ Complete | 0 |
| 3 — Consensus | ✅ Complete | 0 |
| 4 — Subsystems | ✅ Complete | 0 |
| 5 — Operations | ✅ Complete | 0 |
| 6 — Testing | 🟡 Partial | 2 gaps |
