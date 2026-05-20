# Production Readiness — Remaining Blockers

This document tracks the gap between the current codebase and production mainnet readiness. Individual subsystem docs may claim "all gaps resolved" for their scope; this is the cross-cutting tracker.

**Last updated:** 2026-05-20 (rate limit + Grafana audit complete)

---

## Critical (Must Resolve Before Mainnet)

| # | Blocker | Scope | Details |
|---|---------|-------|---------|
| 1 | **No formal security audit** | Entire codebase | No third-party audit of consensus, cryptography, or economic incentives. |
| 2 | **No formal verification for shielded circuits** | `crates/shielded` | Halo2 circuits are tested but not formally verified. Soundness/completeness proofs absent. |

---

## High (Must Resolve Before Public Testnet)

| # | Blocker | Scope | Details |
|---|---------|-------|---------|
| 4 | **No MPT proof verification tests against live Ethereum** | `crates/light-client` | Bridge MPT proof verification is behind `light-client-bridge` feature flag. No tests validate tx inclusion or receipt proof verification against real Ethereum headers. |
| 5 | **No real-network light client tests** | `crates/light-client` | Missing 7+ day soak test against live Ethereum RPC for header chain submission, receipt proofs, and reorg handling. |

---

## Medium (Should Resolve Before Launch)

| # | Blocker | Scope | Details |
|---|---------|-------|---------|
| 7 | **No testnet environment** | Infrastructure | E2E tests run in-memory. No long-running testnet for soak testing or heterogeneous fork upgrade testing. |

---

## Governance — Recently Resolved

The following governance gaps were resolved in the 2026-04-23 hardening pass:

| Gap | Resolution |
|-----|------------|
| `apply_proposal` only logged for most proposal types | `apply_proposal` now applies real internal state changes for all 10 proposal types |
| Governance state persistence | Already persisted to MDBX; `executor` and `balance_source` rewired after load |
| ForkManager integration for ProtocolUpgrade | Already wired via `NodeProposalExecutor` |

See `docs/governance.md` for full details.

---

## Recently Resolved (2026-05-20 Documentation Audit)

| # | Original Blocker | Resolution |
|---|-----------------|------------|
| 1 | No TLS/HTTPS tests | `crates/rpc/tests/tls_integration.rs` — TLS handshake, plain HTTP rejection, expired cert rejection |
| 2 | No MDBX integration tests | `crates/storage/tests/mdbx_integration.rs` — 30+ tests covering all 22 CallTables (put/get/delete, batch, iteration, concurrency, durability, 1MB values) |
| 3 | No concurrent access/corruption recovery tests | `crates/storage/tests/mdbx_integration.rs` — concurrent writes (10 threads), read-during-batch-write, batch atomicity, close-reopen durability |
| 4 | No network partition tests | `crates/node/tests/` — 16 tests: `test_network_partition.rs` (6), `test_byzantine_faults.rs` (4), `test_chaos.rs` (6) covering partitions, equivocation, message delays, node restarts |
| 5 | No light client consensus verification tests | `crates/light-client/src/tests/mod.rs` — BLS aggregate signature full flow (`test_apply_light_client_update_bls_consensus_full_flow`), invalid signature rejection, insufficient participation rejection, 7 malicious fork / reorg tests |
| 6 | No oracle signature verification negative tests | `crates/oracle/src/tests.rs` — 6 negative tests: wrong key, tampered price, tampered block, all-zeros signature, wrong validator ID, random bytes |
| 7 | No database migration framework | `crates/storage/src/db.rs` — full `Migration` trait + `MigrationRunner` with schema versioning; 5 tests (fresh DB, order, idempotent, failure rollback, data write) |
| 13 | Receipts in-memory only | `crates/node/src/state_persist.rs` — `save_receipts`/`load_receipts` with 5 persistence tests |
| 14 | No agent E2E tests | `crates/node/tests/test_agent_e2e.rs` — full lifecycle: register, grant, pay, batchPay, revoke |
| 16 | No upgrade persistence tests | `crates/consensus/src/fork.rs` — 6 persistence tests (serde roundtrip, upgrades survive restart, rollback nonces, history, partially applied) |
| 17 | No multi-upgrade-at-same-height tests | `crates/consensus/src/fork.rs` — `test_check_upgrades_applies_all_at_same_height` |
| 22 | No CI/CD pipeline | `.github/workflows/ci.yml` + `continuous-security.yml` — automated test execution, fuzz regression, Kani model checking |
| 23 | OTel spans not called in production paths | `crates/node/src/block_producer.rs` and `bft_loop.rs` — `record_block_span`, `record_tx_span`, `record_p2p_span` wired in hot paths |
| 4 (High) | No `eth_getLogs` performance tests | `crates/rpc/src/tests.rs` — `test_eth_get_logs_performance_large_dataset`, `test_eth_get_logs_performance_100k_blocks`, `test_eth_get_logs_performance_1m_blocks` |
| 5 (High) | No slashing economic penalty tests | `crates/consensus/src/simplex.rs` — 8+ tests: `handle_double_sign`, `handle_offline`, `handle_oracle_outlier` verifying stake reduction, validator removal, cumulative slashing |
| 6 (High) | No snapshot production/verification tests | `crates/storage/src/prune/pruner.rs` — `verify_snapshot` with Ed25519 cryptographic signature verification + 9 tests |
| 4 (High) | No fast sync incremental catch-up tests | `crates/node/tests/test_fast_sync_e2e.rs` — 3 tests: `test_fast_sync_restore_then_catch_up` (snapshot restore + 50 block catch-up), `test_fast_sync_pipeline_snapshot_to_disk` (save/list/load/restore), `test_incremental_sync_returns_zero_by_design` |
| 11 (Medium) | Structured error codes missing | `crates/rpc/src/handlers/helpers.rs` — `RpcErrorCode` enum with 9 codes (-32000 to -32010), `rpc_error()` builder, and typed helpers |
| 12 (Medium) | No WebSocket lag handling tests | `crates/rpc/src/tests.rs` — 4 tests: `test_ws_event_lagged_serializes`, `test_broadcast_channel_lag_detected`, `test_ws_subscriber_receives_lag_notification`, `test_ws_eth_lag_notification_json` |
| 13 (Medium) | No CORS configuration tests | `crates/rpc/tests/cors_integration.rs` — 6 tests: preflight allowed/denied, actual request allowed/denied, wildcard, empty origins localhost |
| 14 (Medium) | No mempool eviction under memory pressure tests | `crates/protocol/src/security.rs` — `test_replay_protector_evicts_25_percent_when_over_capacity`, `test_replay_protector_evicted_hash_can_be_re_inserted`, `test_replay_protector_sustained_pressure_eviction` |
| 15 (Medium) | No P2P ban enforcement tests | `crates/network/src/gossip.rs` — 8+ tests: `test_peer_state_ban`, `test_peer_state_ban_reason_preserved`, `test_peer_state_ban_expiry_auto_unban`, `test_gossip_manager_banned_peer_rejected`, `test_gossip_manager_ban_expiry_allows_messages`, `test_gossip_manager_rate_limit_triggers_auto_ban`, `test_gossip_manager_banned_peer_reconnect_rejected`, `test_gossip_manager_ban_expiry_allows_reconnect` |
| 1 (Critical) | No rate limiting tests for RPC | `crates/rpc/src/rate_limit.rs` — 7 tests: first request allowed, within limit, blocks over limit, window reset, per-IP isolation, concurrent access, cleanup purges stale |
| 7 (Medium) | No Grafana dashboards | `arc-node/deployments/monitoring/config-grafana/provisioning/dashboards-data/` — 9 JSON dashboards: default, reth, reth-arc-payload-build, reth-database, reth-discovery, reth-io-correlation, reth-mempool, reth-state-growth, version-panels |
| 30 | No property-based or fuzz testing | `fuzz/` crate with 6 targets (`tx_rlp_decode`, `precompile_dispatch`, `balance_arithmetic`, `mpt_proof_verify`, `signature_recovery`, `block_header_validate`) |
| 31 | No benchmark suite | 7 `criterion.rs` benchmarks (`precompile_execute`, `block_production`, `signature_verify`, `mdbx_read_write`, `proof_generate`, `mpt_verify`, `priority_pool`) |

---

## Subsystem Docs With Overly Optimistic Assessments

The following docs claim "all gaps resolved" or "production ready" for their subsystem. While the implementations are solid, the cross-cutting testing and infrastructure gaps above mean the system as a whole is **not** production ready:

- `docs/protocol.md` — "All protocol components are production-ready. No remaining gaps."
- `docs/bridge.md` — "All documented gaps have been resolved."
- `docs/security.md` — All components marked "Production ready".
- `docs/shielded.md` — All components marked "Production ready".
- `docs/compliance.md` — All components marked "Ready".
- `docs/agent.md` — All components marked "Ready".
- `docs/upgrade.md` — All components marked "Ready".
- `docs/rpc.md` — All components marked "Ready".

These assessments are accurate for the **implementation scope** of each subsystem but do not account for the integration, testing, and infrastructure gaps tracked in this document.
