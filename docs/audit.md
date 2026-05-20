# Issue #1: No Formal Security Audit — Open Source Alternative

> **Severity**: Critical (mainnet blocker)
> **Scope**: Entire codebase
> **Status**: Zero-budget security program in progress — Phases 1–2–4–6–7 implemented, Phase 3–5 pending token budget
> **Solution**: Community-driven + tooling-heavy security assurance program

---

## Overview

As an open-source project, Callchain will not engage a paid audit firm. Instead, this document outlines a rigorous, transparent, and community-driven security assurance program that achieves comparable coverage through automated tooling, fuzzing, formal verification, and incentivized community review.

---

## Phase 1: Automated Security Scanning ✅ Implemented

Set up CI to run continuously on every PR.

### GitHub Actions Workflow

`.github/workflows/continuous-security.yml` runs daily and on every push:

```yaml
name: Continuous Security
on:
  schedule:
    - cron: '0 0 * * *'  # Daily
  push:
    branches: [main]
  pull_request:
    branches: [main]
```

Jobs:
- `audit` — `cargo audit` (known CVEs)
- `deny` — `cargo deny check advisories licenses bans`
- `geiger` — `cargo geiger` (unsafe code count)
- `semgrep` — `.semgrep/callchain-security.yml` (custom rules)
- `fuzz` — 3 targets × 10 min regression
- `kani` — `cargo kani --workspace`

### Required Tools

| Tool | Purpose | Status |
|------|---------|--------|
| `cargo-audit` | Scan dependencies for known CVEs | ✅ In CI |
| `cargo-deny` | Ban vulnerable crates, check licenses | ✅ In CI |
| `cargo-geiger` | Count unsafe code | ✅ In CI |
| `semgrep` | Custom security rule scanning | ✅ In CI |
| `cargo-fuzz` | Fuzz target runner | ✅ In CI |
| `kani-verifier` | Model checker | ✅ In CI |

### Semgrep Rules (Implemented)

`.semgrep/callchain-security.yml` — 6 rules covering:

- `unchecked-arithmetic` — ERROR: `$X + $Y` in balance/precompile code → use `checked_add`
- `unsafe-storage-access` — WARNING: `unsafe { ... }` in storage/precompile → use `StorageRef`
- `todo-in-consensus` — WARNING: `TODO | FIXME | HACK | XXX` in consensus/protocol code
- `panic-in-consensus` — WARNING: `panic!(...)` in consensus code → return `Result`
- `unwrap-in-balance` — WARNING: `.unwrap()` in balance/token code → propagate errors
- `hardcoded-secret` — ERROR: hardcoded passwords/secrets/tokens

---

## Phase 2: Fuzzing + Property-Based Testing ✅ Implemented

### Fuzz Targets (Implemented)

| Target | File | What It Tests | Status |
|--------|------|---------------|--------|
| `tx_rlp_decode` | `fuzz/fuzz_targets/tx_rlp_decode.rs` | Malformed ProtocolVersion RLP decode | ✅ |
| `precompile_dispatch` | `fuzz/fuzz_targets/precompile_dispatch.rs` | Invalid selector crashes in ValidatorPrecompile | ✅ |
| `balance_arithmetic` | `fuzz/fuzz_targets/balance_arithmetic.rs` | Overflow in transfer/mint/burn | ✅ |
| `mpt_proof_verify` | `fuzz/fuzz_targets/mpt_proof_verify.rs` | Fake proof acceptance | ✅ |
| `signature_recovery` | `fuzz/fuzz_targets/signature_recovery.rs` | Invalid sig handling in secp256k1 | ✅ |
| `block_header_validate` | `fuzz/fuzz_targets/block_header_validate.rs` | Malicious header acceptance | ✅ |

Run: `cd fuzz && cargo fuzz run <target>`

### Property Tests (Implemented)

`crates/asset/src/lib.rs` — 4 proptest invariants:

```rust
prop_transfer_preserves_total_balance   // sender + recipient before == after
prop_mint_increases_supply              // supply increases by exact amount
prop_burn_decreases_supply              // supply decreases by exact amount
prop_balance_never_negative             // checked_sub never produces negative
```

`crates/protocol/src/security.rs` — existing proptest invariants:

```rust
prop_replay_protector_stays_bounded     // len <= max_seen + 1
prop_rate_limiter_allows_at_most_max_per_window
```

---

## Phase 3: Community Security Review ⏳ Pending Budget

Launch a public **"Security Review Period"** with token incentives.

**Status**: Not started — requires CALL token budget for rewards.

### Program Announcement

```markdown
# Callchain Security Review Program

## Rewards

| Severity | Reward (CALL tokens) |
|----------|---------------------|
| Critical | 50,000 CALL |
| High     | 10,000 CALL |
| Medium   | 2,000 CALL |
| Low      | 500 CALL |

## Scope

- `crates/consensus/` — BFT safety, slashing correctness
- `crates/validator/` — Staking arithmetic, safety floor, churn limit
- `crates/asset/` — Balance transfers, mint/burn
- `crates/bridge/` — Challenge period, fraud proofs
- `crates/shielded/` — Nullifier uniqueness, note soundness
- `crates/governance/` — Voting power, timelock bypasses
- `crates/protocol/` — Transaction validation, fee logic, replay protection
- `crates/storage/` — State root computation, pruning safety
- `crates/network/` — P2P authentication, message validation
- `crates/rpc/` — Input validation, DoS vectors
- `crates/precompile/` — EVM precompile dispatch, gas accounting

## Rules

1. Provide PoC exploit or detailed vulnerability report
2. Allow 90 days for fix before public disclosure
3. No testing on mainnet without approval (testnet only)
4. First valid report per issue wins
5. Issues caused by dependencies without exploitable impact are out of scope

## Submit

Open a **private** GitHub Security Advisory or email security@callchain.cc
```

### Promotion Channels

- Post to /r/rust, /r/ethdev, /r/crypto
- Tweet from project account
- Share in Discord #security channel
- Reach out to independent security researchers specializing in Rust/blockchain
- Publish on Immunefi (even without full bug bounty program)

---

## Phase 4: Lightweight Formal Verification ✅ Implemented

Use freely available tools on the most critical components.

### Kani Model Checker ✅

```bash
cargo install --locked kani-verifier
cargo kani --crate call-protocol
```

### Verified Properties (Implemented)

`crates/protocol/src/kani_proofs.rs` — 7 proofs:

| Property | What It Proves |
|----------|----------------|
| `verify_transfer_preserves_total` | Transfer preserves sender + recipient total |
| `verify_mint_increases_supply` | Mint increases supply by exact amount |
| `verify_burn_decreases_supply` | Burn decreases supply by exact amount |
| `verify_balance_never_negative` | checked_sub never produces negative balance |
| `verify_nonce_increments` | Nonce always increments by exactly 1 |
| `verify_slash_reduces_stake` | Slashing reduces stake or leaves it at zero |
| `verify_allowance_decrease_exact` | Allowance decrease is exact, never underflows |

Run: `cargo kani --crate call-protocol`

### Theorem-Prover Formal Verification (Shielded Circuits) ❌ Deleted

A previous Lean 4 formalization of the Groth16/R1CS shielded circuits existed in `formal_verification/lean/`, with a mathematical spec at `docs/formal_verification/spec.md`. Both were **deleted during the Halo2 migration** (commit `65035f0`) because they modeled the pre-migration R1CS arithmetization and are no longer applicable to the current PLONKish Halo2 circuits.

**Current status**:
- No theorem-prover formalization exists for the current Halo2 circuits
- Halo2 circuits are verified via constraint-level tests (132 tests with `real-prover` feature) and the Halo2 proving system's own soundness guarantees
- Third-party audit should review circuit constraints directly

**Future work**: Ground-up formal verification of Halo2 PLONKish constraints, custom gates, and permutation arguments would require a new effort distinct from the deleted Lean work.

---

## Phase 5: Competitive Audit (Code4rena Style) ⏳ Pending Budget

Host a **community audit competition** as a higher-stakes follow-up to Phase 3.

**Status**: Not started — requires $50K–$100K CALL token prize pool.

### Competition Format

```
Duration: 2 weeks
Platform: Self-hosted via GitHub + Discord
Prize pool: $50,000–$100,000 worth of CALL tokens

Judges: Core team + 2 independent Rust/security experts

Scoring:
- Critical: 10 points
- High: 5 points
- Medium: 2 points
- Low: 1 point

Payout: (warden_points / total_points) * prize_pool
```

---

## Phase 6: Continuous Security Monitoring ✅ Implemented

`.github/workflows/continuous-security.yml` runs daily:

- `cargo audit` — dependency CVE scan
- `cargo deny check advisories licenses bans` — policy enforcement
- `cargo geiger` — unsafe code tracking
- `semgrep --config .semgrep/ --error` — custom rule enforcement
- `cargo fuzz run <target> --max-total-time=600` — 10 min per target regression
- `cargo kani --workspace` — formal verification regression

---

## Phase 7: Documentation & Transparency ✅ Implemented

### SECURITY.md

`SECURITY.md` at repo root — includes:

- Supported versions table
- Reporting channels (GitHub Security Advisory + email)
- Response timeline (24h Critical → 2 weeks Low)
- Scope (11 crates) + out-of-scope items
- Security measures in place (scanning, fuzzing, Kani, CI)
- Past reviews table

---

## Budget Comparison

| Approach | Cost | Timeline | Effectiveness | Status |
|----------|------|----------|---------------|--------|
| Firm audit | $250K–$400K | 8–14 weeks | 5/5 | Not planned |
| **Community review + fuzzing** | $50K–$100K (tokens) | 6–10 weeks | 4/5 | Phase 3 pending |
| **Competitive audit (Code4rena)** | $50K–$100K (tokens) | 4–6 weeks | 4/5 | Phase 5 pending |
| **Tooling only (no incentives)** | $0 (compute) | 2–4 weeks | 3/5 | ✅ Done |

---

## Recommended Hybrid Path

| Week | Activity | Deliverable | Status |
|------|----------|-------------|--------|
| 1–2 | Set up automated tooling | CI security workflow, cargo-deny config, Semgrep rules | ✅ |
| 2–4 | Implement fuzz targets + property tests | 6 fuzz targets, proptest invariants, corpus seeds | ✅ |
| 4–6 | Kani formal verification | 7 verified properties for balances, nonces, allowances | ✅ |
| 6–8 | Launch community security review | Public program, researcher engagement, first reports | ⏳ |
| 8–10 | Host competitive audit competition | Prize pool, judging, payouts | ⏳ |
| 10+ | Continuous fuzzing in CI, bug bounty live | Daily scans, ongoing rewards | ✅ CI active, ⏳ bounty |

**Tooling cost**: $0 (GitHub Actions free tier)
**Incentive cost**: ~$100K in CALL tokens (Phases 3 + 5)
**Timeline**: Tooling done in 2 weeks; full program 10 weeks after budget approval
**Deliverable**: Public security report + fixed findings + running fuzz suite

---

## Exit Criteria (When Is This Issue Resolved?)

This issue is resolved when ALL of the following are true:

1. [x] `cargo audit` and `cargo deny` run in CI on every PR
2. [x] 6+ fuzz targets implemented and runnable (`fuzz/` directory)
3. [x] Kani verifies at least 6 critical properties in CI (`crates/protocol/src/kani_proofs.rs`)
4. [ ] Community security review completed with >=10 valid findings addressed
5. [ ] Competitive audit completed with all Critical/High findings fixed
6. [x] `SECURITY.md` published with response timeline
7. [ ] Bug bounty program live with funded reward pool
8. [ ] Public security report published in `docs/audit/`
