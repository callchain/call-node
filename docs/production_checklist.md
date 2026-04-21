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

Tracked in `docs/testing.md` — 2 gaps remaining (byzantine consensus tests, load test).

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
