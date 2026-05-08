# Callchain Production Readiness Summary

> Generated: 2026-05-07
> Based on: docs/unready.md, docs/testing.md, docs/security.md, docs/future.md

---

## Bottom Line

**Devnet / internal testnet:** Ready. Code runs, ~1,682 tests pass, blocks produce.

**Public testnet:** Not ready. Missing security hardening, performance validation, TLS/auth, and CI/CD.

**Mainnet:** Not ready. Missing formal audit, DB migration framework, light client BLS verification, and long-running testnet validation.

---

## Critical (Must Resolve Before Mainnet)

| # | Blocker | Scope | Why |
|---|---------|-------|-----|
| 1 | **No formal security audit** | Entire codebase | No third-party review of consensus, cryptography, or economic incentives |
| 2 | ~~**No TLS/HTTPS tests**~~ ✅ | `crates/rpc` | Integration tests added: handshake success, plain HTTP rejection, expired cert rejection (`crates/rpc/tests/tls_integration.rs`) |
| 3 | ~~**No auth/authz tests**~~ ❌ N/A | `crates/rpc` | Permissionless blockchain — all methods are state queries or signed-tx submission; no admin namespace exists to protect |
| 4 | ~~**No MDBX integration tests**~~ ✅ | `crates/storage` | 30 integration tests added covering all 22 tables: put/get/delete roundtrips, batch writes, iteration, sorted order, large values, overwrite, clear, and convenience helpers (`crates/storage/tests/mdbx_integration.rs`) |
| 5 | ~~**No concurrent DB / corruption recovery tests**~~ ✅ | `crates/storage`, `crates/node` | Concurrency tests added: same-key writes, same-table writes, read-during-write, batch atomicity, close-reopen durability. Crash recovery test added: checkpoint detected on restart, in-memory state reset, EVM state preserved |
| 6 | ~~**No network partition tests**~~ ✅ | `crates/network`, `crates/node/tests` | `PartitionableNetwork` + `PartitionSimulator` added. Phase 1: partition groups, drop rates, isolation/heal. Phase 2: block gossip stops across partitions and resumes after heal. Phase 3: equivocating proposer across partitions, withholding proposer round advance, malicious message flood, invalid block rejection (`crates/network/src/p2p/partitionable.rs`, `crates/node/tests/test_network_partition.rs`, `crates/node/tests/test_byzantine_faults.rs`) |
| 7 | ~~**No light client malicious fork tests**~~ ✅ | `crates/light-client` | Malicious fork tests added: reorg below finalized rejected, tampered block hash rejected, duplicate header rejected, before-anchor rejected, gap attack rejected, side-chain fork resolution, buffer overflow rejected, parent-hash chain break detected. Ethereum BLS consensus signature verification remains deferred (see #32) |
| 8 | ~~**No oracle signature negative tests**~~ ✅ | `crates/oracle` | Signature verification already wired (`ed25519_verify` in `OracleTracker::submit_price`). Negative tests added: wrong signing key, tampered price, tampered block number, all-zero signature, random bytes signature (`crates/oracle/src/tests.rs`) |
| 9 | ~~**No database migration framework**~~ ✅ | `crates/storage` | `Migration` trait + `MigrationRunner` added. Schema version tracked in `call_schema_version` table (u64 BE). Runner applies migrations in ascending version order, skips already-applied ones, bumps version after each success. Tests: fresh DB starts at v0, in-order application, idempotence (re-run skipped), failure leaves version unchanged, migration can write data (`crates/storage/src/db.rs`, `crates/storage/src/reth_db.rs`) |
| 10 | ~~**No formal verification for shielded circuits**~~ ⚠️ Partial | `crates/shielded` | True formal verification (theorem-prover proofs of completeness/soundness) remains future work. Added comprehensive constraint-level negative tests as pragmatic alternative: wrong commitment/asset_id/RCM (deposit), wrong nullifier/Merkle root/value/asset_id/spending key (transfer), wrong nullifier/Merkle root/value/asset_id (withdraw), plus constraint-count stability checks. All 132 shielded tests pass (`real-prover` feature) |

---

## High (Must Resolve Before Public Testnet)

| # | Blocker | Scope | Why |
|---|---------|-------|-----|
| 11 | ~~**No `eth_getLogs` performance tests**~~ ✅ | `crates/rpc` | Added `query_logs()` optimized path using `log_index` for address-filtered queries (O(1) lookup vs full scan). 8 tests added: basic, address filter, topic filter, combined filter, block range, large dataset performance (5K blocks/30K logs), empty range, log index consistency |
| 12 | ~~**No receipt persistence tests**~~ ✅ | `crates/node` | Receipts are persisted via `save_receipts()`/`load_receipts()` to MDBX with `CallReceipts` + `CallReceiptsByBlock` tables. 6 tests added: basic roundtrip, multi-block receipts, full field integrity (logs/topics/status/gas/fee/state_changes/memos), empty receipts, overwrite update, and full node restart recovery with `get_receipts_by_block` + `log_index` rebuild verification (`crates/node/src/state_persist.rs`, `crates/node/src/tests.rs`) |
| 13 | ~~**No agent block-production integration tests**~~ ✅ | `crates/consensus` | 2 integration tests added: `test_agent_register_in_block` verifies registerAgent precompile (0x209) in a real block with state persistence; `test_agent_grant_and_pay_in_block` verifies register→grant→pay sequence across 3 EVM transactions in one block, confirming agent balance and recipient balance. Also fixed `execute_block_transactions` storage slot merging bug where later txs overwriting same address lost earlier tx's storage slots (`crates/evm/src/block_executor.rs`, `crates/node/src/tests.rs`) |
| 14 | ~~**No slashing economic penalty tests**~~ ✅ | `crates/consensus` | 5 tests added verifying actual EVM storage stake reduction: `test_double_sign_stake_reduced_to_zero` (full stake burned, status inactive, dropped from qualified); `test_offline_slash_stake_reduced_in_storage` (partial reduction, dropped from qualified); `test_oracle_outlier_slash_stake_reduced_in_storage` (0.1% reduction); `test_cumulative_offline_slash_accumulates` (successive slashes computed on current stake); `test_offline_slash_to_zero_makes_inactive` (100% slash zeroes stake and sets status inactive) (`crates/consensus/src/simplex.rs`) |
| 15 | ~~**No upgrade persistence tests**~~ ✅ | `crates/node` | 4 tests added in `crates/node/src/state_persist.rs`: `test_fork_manager_persist_and_check_activation` (upgrade survives save/load and activates at scheduled height); `test_fork_manager_persist_multiple_upgrades` (3 upgrades at heights 50/100/200 all survive roundtrip and apply in order); `test_fork_manager_persist_upgrade_before_save_height` (upgrade scheduled before save point is not lost); `test_fork_manager_persist_no_upgrade_at_unscheduled_height` (no false positives at unscheduled heights) |
| 16 | ~~**No multi-upgrade-at-same-height tests**~~ ✅ | `crates/consensus` | `test_check_upgrades_applies_all_at_same_height` in `crates/consensus/src/fork.rs` verifies two upgrades scheduled at height 100 both apply: `current_version` advances to `1.2.0` (the last one), both entries are marked `applied=true`, and idempotency holds on re-check (`check_upgrades_at_height(101)` returns `None`) |
| 17 | ~~**No snapshot crypto verification**~~ ✅ | `crates/storage` | `verify_snapshot` already performs Ed25519 crypto verify; what was missing were tests. 6 tests added in `crates/storage/src/prune/pruner.rs`: `test_verify_snapshot_valid_quorum` (2/3 valid sigs pass); `test_verify_snapshot_tampered_data_rejected` (valid sigs on tampered snapshot fail); `test_verify_snapshot_below_quorum` (1/3 sigs rejected); `test_verify_snapshot_unknown_validator_ignored` (unknown validator sig doesn't count toward quorum); `test_verify_snapshot_empty_pubkeys_fallback` (count-only mode when no pubkeys); `test_verify_snapshot_invalid_signature_bytes_rejected` (tampered signature bytes rejected) |
| 18 | ~~**No fast sync catch-up**~~ ✅ | `crates/storage` | 6 tests added in `crates/storage/src/prune/pruner.rs`: `test_incremental_sync_no_op_returns_zero` (documents the no-op contract); `test_fast_sync_run_full_pipeline` (end-to-end: create signed snapshot, save to disk, run `FastSyncFlow::run()`, verify completion); `test_fast_sync_run_no_peers_fails` and `test_fast_sync_run_no_pubkeys_fails` (error paths); `test_fast_sync_restore_snapshot_mismatch_rejected` (tampered snapshot fails restore verification); `test_fast_sync_list_snapshots_sorted` (snapshot listing returns sorted heights) |
| 19 | ~~**No real-network light client tests**~~ ✅ | `crates/light-client` | 4 real-network E2E tests added in `crates/light-client/tests/eth_sync_e2e.rs` (gated behind `eth-sync` feature + `ETH_RPC_URL` env): `test_light_client_real_network_epoch_sync` (syncs 32 headers = one epoch); `test_light_client_real_network_parent_chain` (verifies parent-hash chain integrity across 16 real headers); `test_light_client_real_network_gap_sync` (every-other-header submission then gap fill, tests buffer/flush); `test_light_client_real_network_finalized_checkpoint` (beacon chain API fetch, gated on `BEACON_URL`). Existing `test_light_client_real_network_header_sync` already covered basic 5-header sync |
| 20 | **No CI/CD pipeline** | Repository | Tests run manually; no automated PR execution or coverage |

---

## Medium (Should Resolve Before Launch)

| # | Blocker | Scope | Why |
|---|---------|-------|-----|
| 21 | **OTel spans not called in production paths** | `crates/node` | `record_block_span()` etc. defined but unused in hot paths |
| 22 | **No Grafana dashboards** | `docs/observability.md` | No pre-built JSON dashboard files |
| 23 | **Structured error codes missing** | `crates/rpc` | Errors are strings; no machine-readable codes |
| 24 | **No WebSocket lag handling tests** | `crates/rpc` | Lagged subscribers silently drop events |
| 25 | **No CORS configuration tests** | `crates/rpc` | Default jsonrpsee CORS untested |
| 26 | **No mempool eviction under pressure tests** | `crates/mempool` | `ReplayProtector` evicts 25% but correctness untested |
| 27 | **No P2P ban enforcement tests** | `crates/network` | Ban duration defined but not verified in practice |
| 28 | **No property-based / fuzz testing** | Entire codebase | No `proptest`, `quickcheck`, or `cargo-fuzz` |
| 29 | **No benchmark suite** | Entire codebase | No `criterion.rs` for hot paths |
| 30 | **No testnet environment** | Infrastructure | E2E runs in-memory only; no soak testing |
| 31 | **Churn limit / safety floor unimplemented** | `crates/consensus` | `docs/validator_staking.md` describes mechanisms not yet in code |

---

## Future (Deferred, Not Blocking)

| # | Item | Scope | Notes |
|---|------|-------|-------|
| 32 | **EthLightClient BLS consensus verification** | `crates/light-client` | Parent-hash chain sufficient for devnet/testnet; beacon chain sync deferred |
| 33 | **Prover key rotation** | `crates/shielded` | Governance-driven ceremony; mainnet readiness only |
| 34 | **MEV protection** | `crates/protocol` | Commit-reveal library exists but not integrated into block production |
| 35 | **System contracts** | Long-term | Protocol logic in Rust precompiles; Solidity migration deferred |

---

## What's Working Well

- **~1,700 tests** across 101 files, spanning unit, integration, and E2E
- **EVM-only architecture**: all state in MDBX (`CallEvmAccounts` / `CallEvmStorage`)
- **Commonware Simplex BFT** consensus with VRF proposer rotation
- **Dynamic gas metering** (`base + sloads*50 + sstores*500`) via revm Journal
- **`StorageRef`** safe journal decomposition (replaced unsafe `JournalBackend`)
- **All precompiles wired** (`0x101` oracle, `0x103` bridge, `0x201` asset, `0x202` shielded, `0x203` governance, `0x204` validator, `0x205` compliance, `0x207` switch, `0x209` agent)
- **Shielded pool** with Groth16 proofs (deposit/transfer/withdraw)
- **Bridge** with optimistic challenge period (initiate/resolve/withdraw bond)
- **Governance** with timelock, auto-advance, and 10 proposal types
- **Fast sync** with Ed25519-signed state snapshots (production wired; cryptographic verification tests incomplete — see #17)
- **Light client** with MPT proof verification and header gossip

---

## Recommended Priority Order

1. **Before public testnet**: CI/CD (#20), TLS/auth tests (#2-3), MDBX integration (#4), oracle signature tests (#8), performance tests (#11-12), slashing tests (#14)
2. **Before mainnet audit kickoff**: All critical items (#1, #5-10), real-network light client soak (#19)
3. **Before mainnet launch**: Implement churn limit / safety floor / dynamic unbonding (#31), benchmark suite (#29), formal shielded verification (#10)
