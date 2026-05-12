# Callchain Testing Gaps — Tracking Document

**Updated**: 2026-05-11
**Source**: `docs/testing.md` — extracted unresolved gaps for focused tracking.

---

## How to Use This Document

This file tracks only **open (unresolved) testing gaps**. When a gap is closed:
1. Move it to the "Recently Closed" section at the bottom
2. Reference the commit / PR that resolved it
3. Update `docs/testing.md` accordingly

---

## Critical (Block Mainnet)

> **Note**: RPC authentication / authorization (JWT, API keys) is intentionally out of scope. Callchain nodes are expected to run behind a reverse proxy or VPN where auth is handled at the infrastructure layer.

| # | Gap | Crate | What to Test |
|---|-----|-------|--------------|
| 1 | ~~RPC rate limiting~~ | ~~`call-rpc`~~ | ~~Per-client request throttling, burst handling, ban-after-excess~~ |
| 2 | ~~Concurrent DB access / crash recovery~~ | ~~`call-storage`~~ | ~~Simultaneous writers, WAL replay after kill -9, corruption detection~~ |
| 3 | ~~Light client beacon BLS consensus~~ | ~~`call-light-client`~~ | ~~Mock beacon API → `fetch_light_client_finality_update` → `apply_light_client_update` → `set_finalized_block`~~ |
| 4 | ~~Oracle signature negative tests~~ | ~~`call-oracle`~~ | ~~Submit with invalid 64-byte signature → must fail~~ |

---

## High (Block Public Testnet)

| # | Gap | Crate | What to Test |
|---|-----|-------|--------------|
| ~~5~~ | ~~`eth_getLogs` at 1M+ blocks~~ | ~~`call-rpc`~~ | ~~`test_eth_get_logs_performance_100k_blocks` (CI) + `test_eth_get_logs_performance_1m_blocks` (`#[ignore]` stress)~~ |
| ~~6~~ | ~~Receipt DB persistence~~ | ~~`call-node`~~ | ~~`state_persist.rs`: 8 receipt tests (roundtrip, multi-block, field integrity, empty, overwrite, node restart, empty restart, incremental) + `test_e2e_state_persistence_restart`~~ |
| ~~7~~ | ~~Agent tx in block production~~ | ~~`call-node`~~ | ~~`test_agent_e2e.rs`: `test_agent_lifecycle_in_block` (register→grant→pay→batchPay→revokeBalance→revokeAgent) + `test_agent_non_owner_rejected`~~ |
| ~~8~~ | ~~Agent domain real verification~~ | ~~`call-agent`~~ | ~~`domain_verification.rs`: DNS TXT (`hickory-resolver`) + HTTP (`ureq`) with `domain-verify` feature; 2 no-feature tests + 5 `#[ignore]` real-network tests~~ |
| ~~9~~ | ~~BatchTransfer permission bypass~~ | ~~`call-asset`~~ | ~~`test_asset_precompile_batch_transfer_all_allowed`, `test_asset_precompile_batch_transfer_blocked_recipient_fails`, `test_asset_precompile_batch_transfer_first_allowed_second_blocked`~~ |
| ~~10~~ | ~~Slashing economic penalty~~ | ~~`call-consensus`~~ | ~~Double-sign detected → stake reduced → validator removed from set~~ |
| ~~11~~ | ~~ForkManager persistence~~ | ~~`call-consensus`~~ | ~~Serialize to DB, restart, scheduled upgrades retained~~ |
| ~~12~~ | ~~Multi-upgrade same height~~ | ~~`call-consensus`~~ | ~~Two upgrades at height H → both applied (verified by `test_check_upgrades_applies_all_at_same_height`)~~ |
| ~~13~~ | ~~Snapshot production + sig verify~~ | ~~`call-storage`~~ | ~~Trigger snapshot, verify cryptographic signatures (not just count)~~ |
| ~~14~~ | ~~Fast sync incremental catch-up~~ | ~~`call-node`~~ | ~~Snapshot at block N, head at N+10K, verify catch-up completes~~ |

---

## Medium (Ongoing)

| # | Gap | Crate | What to Test |
|---|-----|-------|--------------|
| ~~15~~ | ~~Agent `credit()` u128 overflow~~ | ~~`call-agent`~~ | ~~`grant_balance` with `u128::MAX` + 1 → must not wrap~~ |
| ~~16~~ | ~~EIP-2718 typed receipt parsing~~ | ~~`call-light-client`~~ | ~~Type 0x01 (EIP-2930) and Type 0x02 (EIP-1559) receipt proofs~~ |
| ~~17~~ | ~~Light client reorg handling~~ | ~~`call-light-client`~~ | ~~Feed orphaned headers, verify rollback and resync~~ |
| ~~18~~ | ~~WebSocket lag handling~~ | ~~`call-rpc`~~ | ~~Slow subscriber → verify lag notification or silent drop behavior~~ |
| ~~19~~ | ~~Compliance report symbol accuracy~~ | ~~`call-node`~~ | ~~Asset ID 2 mapped to correct symbol, not hardcoded "CALL"~~ |
| ~~20~~ | ~~Log rotation under load~~ | ~~`call-node`~~ | ~~Rapid 1000 logs/sec, disk-full simulation~~ |
| ~~21~~ | ~~P2P ban enforcement~~ | ~~`call-network`~~ | ~~`crates/network/src/gossip.rs`: 3 tests (rate limit → auto-ban, banned peer reconnect rejected, ban expiry allows reconnect)~~ |
| ~~22~~ | ~~Mempool eviction under pressure~~ | ~~`call-mempool`~~ | ~~`crates/protocol/src/security.rs`: 3 tests (just-inserted never evicted, no false evictions under pressure, eviction count exact)~~ |
| ~~23~~ | ~~Light client bridge deposit E2E~~ | ~~`call-bridge`~~ | ~~`crates/bridge/src/external/deposit.rs`: 6 tests (rejects unverified block, accepts verified block, rejects not-allowed asset, rejects duplicate header, rejects daily limit, EVM storage state)~~ |
| ~~24~~ | ~~Bridge MPT proof verification~~ | ~~`call-bridge`~~ | ~~Tx inclusion proof + receipt proof against real Ethereum header~~ |
| ~~25~~ | ~~Beacon sync background task~~ | ~~`call-node`~~ | ~~`start_beacon_sync_task` tick → fetch → BLS verify → `is_consensus_verified`~~ |
| ~~26~~ | ~~OpenTelemetry span in hot path~~ | ~~`call-node`~~ | ~~Block production triggers `record_block_span`, span emitted to collector~~ |
| ~~27~~ | ~~FileLogLayer high-volume rotation~~ | ~~`call-node`~~ | ~~Background task handles sustained 10K logs/sec without drop~~ |

---

## Infrastructure (Meta)

| # | Gap | Tool / Approach |
|---|-----|-----------------|
| ~~28~~ | ~~CI/CD pipeline~~ | ~~GitHub Actions: `cargo test --workspace` on PR, nightly full suite~~ |
| ~~29~~ | ~~Code coverage~~ | ~~`cargo tarpaulin` or `cargo llvm-cov`, gate PRs at >70%~~ |
| ~~30~~ | ~~Benchmark suite~~ | ~~`criterion.rs`: `crates/light-client/benches/mpt_verify.rs` (5 benches) + `crates/node/benches/block_production.rs` (1 bench, 3 params)~~ |
| ~~31~~ | ~~Property-based testing~~ | ~~`proptest`: light-client (4 props: RLP no-panic, header hash roundtrip, MPT no-panic, empty proof, deterministic) + precompile dispatch (5 props: zero-ops base, monotonic sloads/sstores, no underflow, additive)~~ |
| ~~32~~ | ~~Long-running testnet~~ | ~~`crates/node/tests/test_fork_upgrade.rs`: `test_mixed_validator_versions_consensus` (different protocol versions produce/verify blocks without split) + existing soak tests~~ |
| ~~33~~ | ~~Mutation testing~~ | ~~`.mutants.toml` (profile for key crates, excludes slow/external) + CI `mutants` job (manual trigger, artifact upload)~~ |

---

## Recently Closed

| # | Gap | Resolution | Commit |
|---|-----|-----------|--------|
| — | TLS/HTTPS tests | `tls_integration.rs` (3 tests) | prior |
| — | CORS tests | `cors_integration.rs` (6 tests) | prior |
| — | MDBX integration | `test_mdbx_integration.rs` (35 tests) | prior |
| — | Network partition | `test_network_partition.rs` (7 tests) | prior |
| — | Byzantine faults | `test_byzantine_faults.rs` (4 tests) | prior |
| — | Audit log integrity | Merkle root, file roundtrip, append-only | `06c579b` |
| — | Structured logging | JSON/text, rotation, config defaults | `06c579b` |
| — | Compliance CSV export | CSV generation with dynamic timestamps | `06c579b` |
| — | Oracle E2E | Price submit/read via TestNode | prior |
| — | Light client sync | Ethereum sync committee header verify | prior |
| — | Soak test baseline | Sustained load E2E | prior |
| 1 | RPC rate limiting | `crates/rpc/src/rate_limit.rs` (7 tests) | `1cf0214` |
| 2 | Concurrent DB access / crash recovery | `crates/storage/src/reth_db.rs` (+3 tests: same-key race, reopen persist, WAL checkpoint) | `713aa29` |
| 3 | Light client beacon BLS consensus | `crates/light-client/src/tests/mod.rs` (+3 tests: full flow, invalid sig, insufficient participation) + `bls_sign_beacon` | `12a8ccb` |
| 4 | Oracle signature negative tests | `crates/oracle/src/tests.rs` (+1 test: wrong validator_id; existing: wrong key, tampered price/block, all-zeros, random bytes) | current |
| 5 | `eth_getLogs` at 1M+ blocks | `crates/rpc/src/tests.rs` (+2 tests: `test_eth_get_logs_performance_100k_blocks`, `test_eth_get_logs_performance_1m_blocks` `[ignore]`) | current |
| 6 | Receipt DB persistence | `crates/node/src/state_persist.rs` (8 receipt tests + `test_e2e_state_persistence_restart`) | prior (already existed) |
| 7 | Agent tx in block production | `crates/node/tests/test_agent_e2e.rs` (2 tests: full lifecycle + non-owner rejection) | current |
| 8 | Agent domain real verification | `crates/agent/src/domain_verification.rs` (7 tests: 2 no-feature + 5 `#[ignore]` network) | current |
| 9 | BatchTransfer permission bypass | `crates/asset/src/precompile.rs` (3 tests: all allowed, blocked fails, first allowed second blocked) | current |
| 10 | Slashing economic penalty | `crates/consensus/src/simplex.rs` (6 tests: double-sign stake→0, offline slash, oracle slash, cumulative, subset refresh, nonexistent validator) | `e644ef4` |
| 11 | ForkManager persistence | `crates/consensus/src/fork.rs` (5 tests: serde roundtrip, upgrades survive restart, rollback nonces survive, rollback history survive, partially applied upgrades) | current |
| 12 | Multi-upgrade same height | `crates/consensus/src/fork.rs` (`test_check_upgrades_applies_all_at_same_height`: both upgrades at height 100 applied) | prior (`c3d7d4e`) |
| 13 | Snapshot production + sig verify | `crates/storage/src/prune/pruner.rs` (6 tests: valid quorum, tampered data, below quorum, unknown validator, empty pubkeys fallback, invalid sig bytes) + 5 fast sync pipeline tests | `ee66f17` + `fa398ea` |
| 14 | Fast sync incremental catch-up | `crates/node/tests/test_fast_sync_e2e.rs` (3 tests: restore+catch-up, incremental_sync no-op, snapshot pipeline to disk) | current |
| 15 | Agent `credit()` u128 overflow | `crates/agent/src/lib.rs` (`test_grant_balance_u128_overflow_rejected`: `checked_add` prevents wrap, returns `BalanceOverflow`) | current |
| 16 | EIP-2718 typed receipt parsing | `crates/light-client/src/tests/mod.rs` (4 tests: Type 1, Type 2, Type 3, bridge event from typed receipt + empty rejection) | current |
| 17 | Light client reorg handling | `crates/light-client/src/tests/mod.rs` (3 tests: longer chain rollback, resubmit unwound headers, buffered headers after rollback) | current |
| 18 | WebSocket lag handling | `crates/rpc/src/tests.rs` (5 tests: lag event serializes, broadcast channel lag detected, full channel no panic, subscriber receives lag notification, ETH lag JSON format) | prior |
| 19 | Compliance report symbol accuracy | `crates/node/src/logging.rs` (3 tests: per-asset-id symbol mapping, asset filter excludes unrelated, amount delta computation) | current |
| 20 | Log rotation under load | `crates/node/src/logging.rs` (3 tests: rapid 10x rotation sequence, cleanup old logs, read-only dir failure) | current |
| 21 | P2P ban enforcement | `crates/network/src/gossip.rs` (3 tests: rate limit → auto-ban, reconnect rejected, ban expiry allows reconnect) | current |
| 22 | Mempool eviction under pressure | `crates/protocol/src/security.rs` (3 tests: just-inserted never evicted, no false evictions under pressure, exact eviction count) | current |
| 23 | Light client bridge deposit E2E | `crates/bridge/src/external/deposit.rs` (6 tests: unverified block, verified block, not-allowed asset, duplicate header, daily limit, EVM storage state) | current |
| 24 | Bridge MPT proof verification | `crates/light-client/src/tests/mod.rs` (3 tests: tx inclusion with branch node, receipt proof with extension+branch, missing tx rejected) | current |
| 25 | Beacon sync background task | `crates/node/src/tests.rs` (3 tests: task spawns with light client, returns early without light client, applies update + sets finalized block) | current |
| 26 | OpenTelemetry span in hot path | `crates/node/src/telemetry/tests.rs` (1 test: block/tx/p2p spans emitted to collector via CaptureExporter) | current |
| 27 | FileLogLayer high-volume rotation | `crates/node/src/logging.rs` (1 test: `test_file_log_layer_sustained_high_volume` — 10K entries pumped through channel, all written to file) | current |
| 28 | CI/CD pipeline | `.github/workflows/ci.yml` (fmt, clippy, deny, audit, test, build, coverage jobs) | prior |
| 29 | Code coverage | `.github/workflows/ci.yml` (coverage job with `cargo-llvm-cov` + Codecov upload) | prior |
| 30 | Benchmark suite | `crates/light-client/benches/mpt_verify.rs` (5 benches: leaf, ext+leaf, branch+leaf, deep, batch) + `crates/node/benches/block_production.rs` (block execution 1/10/100 tx) | current |
| 31 | Property-based testing | `crates/light-client/src/tests/mod.rs` (5 proptest invariants: RLP no-panic, header hash roundtrip, MPT no-panic, empty proof, deterministic) + `crates/precompile/src/dispatch.rs` (5 proptest invariants: zero-ops base, monotonic sloads/sstores, no underflow, additive) | current |
| 32 | Long-running testnet | `crates/node/tests/test_fork_upgrade.rs` (`test_mixed_validator_versions_consensus`: different protocol versions produce/verify blocks without split) + existing soak tests | current |
| 33 | Mutation testing | `.mutants.toml` (profile for key crates, excludes slow/external) + CI `mutants` job (manual trigger, artifact upload) | current |

---

## Stats

- **Total open**: 0 gaps
- **Recently closed**: 45 gaps
- **Target**: Close all critical + high before mainnet; medium + infra before public testnet
