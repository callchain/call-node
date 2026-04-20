# Production Readiness Analysis

**Date**: 2026-04-17
**Commit**: e64e58e — `fix(rpc): fix get_receipts_by_block to actually filter by block number`
**Branch**: dev

## Summary

This is a well-architected testnet/devnet candidate. The codebase demonstrates strong engineering discipline — clean crate separation, comprehensive test coverage, strict linting, and thorough documentation. However, several critical gaps remain before mainnet deployment.

**Overall readiness**: Testnet-ready, not mainnet-ready.

---

## What's Done Well

- **Spec coverage**: All 26 spec sections validated, all tasks (P0-P23) marked complete
- **Test coverage**: 647+ unit/integration tests, 83 integration tests, E2E tests
- **Architecture**: Clean workspace crate separation (18 crates), Simplex BFT consensus, dual execution domains
- **ZK shielded**: Real arkworks Groth16 prover with 3 circuits (Deposit/Transfer/Withdraw), feature-flagged (`real-prover`)
- **Code quality**: Strict clippy rules (warn on `unwrap`, `expect`, `panic`, `unsafe_code`)
- **Documentation**: Detailed spec (~5K lines), task lists, ZK design docs, CHANGELOG, SECURITY policy
- **Docker**: Production-ready multi-stage build with non-root user
- **Telemetry**: Prometheus metrics, OpenTelemetry tracing, alert rules
- **Governance**: Dual-track voting (validators + token holders), proposal lifecycle, emergency pause
- **Security hardening**: Rate limiting, replay protection, double-sign slashing, MEV protection (PBS + commit-reveal)

---

## Gaps Before Production

### Critical

1. **No tagged release** — version is `0.1.0`, changelog is all `[Unreleased]`, no git releases
2. **ZK trusted setup** — ~~uses `circuit_specific_setup` (dev/dummy CRS). Production requires a Powers of Tau ceremony with MPC participants~~. **Partially fixed**: `PoT_ceremony/` has complete tooling (download Perpetual PoT, run custom ceremony, Phase 2 key derivation). `crates/shielded/src/ceremony.rs` implements `ProductionKeys::load_with_verification()` with genesis hash checking. `RealProver::global()` auto-switches to production keys when `production-keys` feature is enabled. `export_r1cs.rs` exports arkworks circuits to snarkjs format. Remaining: execute the ceremony, generate `circuit_keys/`, and enable `production-keys` in production builds.
3. **No third-party security audit** — no audit report from a reputable firm (Trail of Bits, OpenZeppelin, etc.). Given the financial nature of the system, this is a prerequisite.
4. **Mainnet genesis config absent** — no production genesis with real validator set, token distribution, initial parameters, and chain ID.

### High

5. **Bridge security model** — ~~external bridge relies on 14/21 validator signatures with no fraud proofs, challenge period, or light client verification~~. **Fixed**: challenge period is ~7 days (2,419,200 blocks at 250ms), `light-client-bridge` feature enabled by default, bridge deposits are automatically finalized during block production, and permissionless challenge revocation is available via `ChallengeBridgeDeposit` instruction with `revoke_pending_external_deposit`. Tests cover the challenge flow.
6. **Oracle system lacks real data sources** — price feeds are structural/mock. No Chainlink, Pyth, or other production oracle integration. Fee conversion and multi-currency payments depend on this.
7. **Key management** — ~~validator keys stored on-disk locally. No HSM/KMS integration, no threshold signature scheme, no key rotation protocol~~. **Fixed**: `AwsKmsSigner` and `HashiVaultSigner` backends are implemented behind feature flags. `docs/release.md` documents boot-time key loading priority, threshold signing approach (M-of-N via commonware-cryptography when available), and key rotation protocol with governance timelock. Threshold signing is not yet implemented.
8. **Network security** — ~~no TLS mentioned, no authenticated P2P handshakes documented, no infra-level DoS protection~~. **Fixed**: TLS/HTTPS + per-IP rate limiting added to RPC layer. `P2PDefense` is now wired into the P2P message receive loop enforcing per-peer rate limits (100 msg/sec) and max message size (10 MB). Infra-level DoS protection still needs documentation.

### Medium

9. **Compliance engine has no real data** — ~~OFAC blacklist, KYC registry, whitelist are in-memory structures with no real data source integration~~. **Fixed**: Background `compliance_data_sync` task fetches sanctioned address lists from a configurable URL (`CALL_COMPLIANCE_DATA_URL` env var) every 5 minutes and updates the `ComplianceEngine` blacklist. KYC/whitelist data sources still need integration.
10. **Governance timelock** — `TIMELOCK_PERIOD_BLOCKS` is 2,419,200 blocks (~7 days at 250ms block time). This is appropriate for production. The 1000-block value only appears in test configs.
11. **Stress testing unverified** — E2E stress tests claim 10K tx/sec targets but this needs real benchmarking on production hardware with realistic network conditions (latency, packet loss, geographic distribution).
12. **No bug bounty program** — SECURITY.md describes responsible disclosure but there's no mention of a bounty program (Immunefi, HackerOne) to incentivize white-hat research.

### Low

13. **Light client untested in real network** — the light client is implemented but needs testing against real node behavior, not just simulation.
14. **Fork management untested at scale** — height-activated upgrades work in simulation but haven't been tested with heterogeneous node versions on a live network.
15. **No disaster recovery runbook** — ~~no documented procedures for chain halt, state corruption, or mass validator offline events~~. **Fixed**: Three runbooks created under `docs/runbooks/`: `chain-halt.md` (diagnosis and recovery), `state-corruption.md` (MDBX snapshot restore and fast sync), `mass-offline.md` (emergency pause and validator recovery coordination).

---

## Risk Assessment by Component

| Component | Readiness | Risk if Deployed | Notes |
|-----------|-----------|-----------------|-------|
| Protocol payments | Medium-High | Moderate | Core logic implemented, needs audit |
| EVM layer | Medium | Moderate | Revm integration solid, ERC-20 template needs audit |
| Consensus (Simplex BFT) | Medium | High | Via commonware — battle-tested but not at this scale |
| ZK shielded | Medium | Moderate-High | Real prover with 3 circuits (Deposit/Transfer/Withdraw) implemented; `production-keys` feature auto-loads ceremony-derived keys; PoT ceremony tooling complete; remaining: execute ceremony and enable feature in production builds |
| External bridge | Medium-High | Moderate | Challenge period (~7 days), light-client verification enabled, auto-finalization in block production, permissionless `ChallengeBridgeDeposit` instruction for fraud proofs |
| Oracle system | Low | High | No real price feed integration |
| Governance | Medium | Moderate | Logic complete, timelock at ~7 days is production-appropriate |
| Mempool | High | Low | Well-structured with anti-spam measures |
| RPC layer | High | Low | JSON-RPC + WebSocket, good defaults |
| Node/CLI | High | Low | Well-structured, good defaults |
| Telemetry | High | Low | Prometheus + OpenTelemetry, alert rules defined |
| Light client | Medium | Low | Implemented, needs real-world testing |
| Network/P2P | Medium-High | Low-Moderate | Commonware-p2p with Noise auth; P2PDefense wired for per-peer rate limiting and message size caps |
| State storage | High | Low | reth-db (MDBX), prune strategy defined |

---

## Recommended Production Path

### Phase 1: Testnet (Current State + Minor Fixes)
- Tag a `v0.1.0-testnet` release
- Deploy with mock prover + mock oracles
- Invite external developers to build and test
- Run stress tests on real hardware with distributed validators

### Phase 2: Pre-Mainnet
- Commission a third-party security audit
- Run Powers of Tau ceremony for ZK circuits
- Integrate production oracle feeds (Chainlink/Pyth)
- ~~Implement HSM/KMS key management~~ → Enable HSM/KMS in production config (code implemented, feature-gated)
- Launch a bug bounty program (docs drafted in `docs/release.md`)
- Tune governance parameters (longer timelocks, higher deposits)

### Phase 3: Mainnet
- Deploy with audited code and production CRS
- Start with limited total value (gradual token release)
- Monitor for 30-90 days before full capacity
- ~~Establish incident response procedures and runbooks~~ → Runbooks created under `docs/runbooks/`

---

## Code Quality Metrics

| Metric | Value | Assessment |
|--------|-------|------------|
| Workspace crates | 18 | Well-separated concerns |
| Rust edition | 2021 | Current |
| MSRV | 1.82 | Reasonable |
| Lint strictness | High | Warns on unwrap, expect, panic, unsafe |
| Test count | 647+ | Good coverage |
| Integration tests | 83 | Cross-component coverage |
| E2E tests | Present | Deterministic runtime + network simulation |
| Dependencies | reth (git-locked), commonware, alloy, revm | Modern stack |
| License | MIT OR Apache-2.0 | Standard |
