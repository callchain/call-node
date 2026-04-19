# Callchain Production Readiness Checklist

This document provides a phased roadmap to take Callchain from its current state to production deployment. It is derived from the 11 module audits in `docs/`.

---

## Phase 0: Stop the Bleeding (Security — 2–3 weeks)

These gaps allow arbitrary state manipulation with no cryptographic barriers.

| Priority | Action | Files |
|----------|--------|-------|
| **P0** | Wire secp256k1 signature verification into `execute_protocol_instructions` | `crates/protocol/src/transaction.rs`, `executor.rs` |
| **P0** | Implement multi-sig verification for `call_bridgeSubmitDeposit` | `crates/bridge/src/external.rs`, `rpc/src/callchain.rs` |
| **P0** | Fix `call_lightVerifyBlockHeader` — verify Ed25519 signatures instead of counting hex lengths | `crates/rpc/src/callchain.rs` |
| **P0** | Fix `verify_snapshot()` — cryptographically verify validator signatures | `crates/storage/src/prune.rs` |

**Why first:** Without these, anyone can forge transactions, prices, deposits, and snapshots.

---

## Phase 1: Make State Persistent (Storage — 3–4 weeks)

Currently everything is in-memory or JSON. A restart destroys receipts, nullifiers, agent state, upgrades, and consensus history.

| Priority | Action | Files |
|----------|--------|-------|
| **P1** | Complete MDBX integration — implement actual `DbCursor`/`DbTx` for all 34 tables | `crates/storage/src/db.rs`, `reth_db.rs`, `tables.rs` |
| **P1** | Replace JSON fallback with hard failure on MDBX init | `crates/storage/src/db.rs` |
| **P1** | Wire `PruneState` to delete from MDBX, not just in-memory BTreeMaps | `crates/storage/src/prune.rs` |
| **P1** | Implement WAL / crash recovery | `crates/storage/src/db.rs` |
| **P1** | Persist receipts, agent state, ForkManager, nullifiers, validator set to MDBX | Cross-crate |

**Why now:** Every other system (bridge, agent, shielded, consensus) depends on durable state.

---

## Phase 2: Close the RPC Attack Surface (2 weeks)

The RPC is the primary external interface and currently has no guards.

| Priority | Action | Files |
|----------|--------|-------|
| **P2** | Add JWT or API-key authentication to sensitive endpoints (governance, oracle, bridge, pause) | `crates/rpc/src/lib.rs`, `handlers.rs` |
| ~~P2~~ | ~~Add per-client rate limiting~~ | ~~`crates/rpc/src/lib.rs`~~ | **Done.** Per-IP sliding-window rate limiter at connection level (`RateLimiter`). Configurable via `rate_limit_rps` / `rate_limit_window_secs`. |
| ~~P2~~ | ~~Add TLS/HTTPS support~~ | ~~`crates/rpc/src/lib.rs`~~ | **Done.** Both HTTP and WS servers support TLS via `tokio-rustls`. Configured via `tls_cert_path` / `tls_key_path`. |
| ~~P2~~ | ~~Fix `call_governanceExecute` signature binding~~ | ~~`crates/rpc/src/callchain.rs`~~ | **Done.** Refactored to transaction submission; signature verified by `ProtocolTransaction::verify_signature()` against actual sender. |

---

## Phase 3: Make Consensus Economically Safe (2–3 weeks)

| Priority | Action | Files |
|----------|--------|-------|
| **P3** | Make slashing reduce stake and remove validators from the active set | `crates/protocol/src/security.rs`, `consensus/src/validator.rs` |
| **P3** | Persist ForkManager and rollback state to disk | `crates/consensus/src/fork.rs` |
| **P3** | Gossip scheduled upgrades across the network | `crates/network/src/gossip.rs`, `consensus/src/fork.rs` |
| **P3** | Actually execute emergency rollback (revert state to target height) | `crates/consensus/src/fork.rs`, `node/src/lib.rs` |
| **P3** | Add rollback replay protection (nonce/height bound) | `crates/consensus/src/fork.rs` |

---

## Phase 4: Fix the Broken Subsystems (3–4 weeks)

| Priority | Action | Files |
|----------|--------|-------|
| **P4** | **Light Client:** Fix receipt proof to use RLP-encoded index key instead of `&[]` | `crates/light-client/src/ethereum.rs` |
| **P4** | **Light Client:** Support EIP-2718 typed receipts | `crates/light-client/src/ethereum.rs` |
| **P4** | **Light Client:** Check bridge event signature hash (`topics[0]`) | `crates/light-client/src/ethereum.rs` |
| **P4** | **Agent:** Deduct from owner balance on `grant_funds()` | `crates/agent/src/balances.rs` |
| **P4** | **Agent:** Add overflow check on `credit()` | `crates/agent/src/balances.rs` |
| **P4** | **Agent:** Wire agent instructions into block production pipeline | `crates/node/src/lib.rs`, `payload-builder/src/builder.rs` |
| **P4** | **Agent:** Fix `execute_agent_call` — call general EVM, not `evm_call_bridge_mint` | `crates/agent/src/executor.rs` |
| **P4** | **Agent:** Implement real domain verification (DNS TXT / HTTP file) | `crates/agent/src/registry.rs` |
| **P4** | **Shielded:** Persist nullifier set to MDBX | `crates/shielded/src/lib.rs`, `storage/src/db.rs` |

---

## Phase 5: Harden Operations (2 weeks)

| Priority | Action | Files |
|----------|--------|-------|
| **P5** | Make `/health` check actual subsystems (DB writable, P2P connected, sync status) | `crates/node/src/telemetry.rs` |
| **P5** | Run alert evaluation in a background task with webhook/Slack integration | `crates/node/src/telemetry.rs` |
| **P5** | Wire all metrics into actual production paths (block production, tx execution, P2P) | Cross-crate |
| **P5** | Add latency histograms for block/tx/P2P | `crates/node/src/telemetry.rs` |
| **P5** | Integrate audit log append into block execution | `crates/node/src/logging.rs`, `protocol/src/executor.rs` |

---

## Phase 6: Test Coverage (Ongoing, 4+ weeks)

| Priority | Action |
|----------|--------|
| **P6** | Property-based tests for tx decoding, MPT proofs, instruction parsing (`proptest`) |
| **P6** | Byzantine consensus tests: network partitions, equivocation, delayed messages |
| **P6** | Signature verification negative tests for every authenticated endpoint |
| **P6** | MDBX integration tests: concurrent writes, crash recovery, compaction |
| **P6** | Load test: sustained 1000 TPS for 1 hour + memory profiling |
| **P6** | Set up CI/CD (`cargo test`, `cargo clippy`, coverage with `cargo llvm-cov`) |

---

## Recommended Order of Execution

```
Week 1–3:  Phase 0 (signature verification) + Phase 1 (MDBX persistence)
Week 4–5:  Phase 2 (RPC security) + Phase 3 (consensus slashing/rollback)
Week 6–9:  Phase 4 (light client + agent fixes)
Week 10–11: Phase 5 (observability/ops)
Week 12+: Phase 6 (testing + CI + testnet soak)
```

---

## The Single Most Important Principle

Every authenticated action must actually verify its signature.

Right now, the codebase has the *shape* of a secure system — signature fields, verification functions, challenge periods — but many critical paths skip verification entirely. Fix that first, or nothing else matters.
