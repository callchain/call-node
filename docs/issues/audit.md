# Issue #1: No Formal Security Audit — Open Source Alternative

> **Severity**: Critical (mainnet blocker)
> **Scope**: Entire codebase
> **Status**: No third-party review of consensus, cryptography, or economic incentives
> **Solution**: Community-driven + tooling-heavy security assurance program

---

## Overview

As an open-source project, Callchain will not engage a paid audit firm. Instead, this document outlines a rigorous, transparent, and community-driven security assurance program that achieves comparable coverage through automated tooling, fuzzing, formal verification, and incentivized community review.

---

## Phase 1: Automated Security Scanning (Week 1–2)

Set up CI to run continuously on every PR.

### GitHub Actions Workflow

```yaml
# .github/workflows/security.yml
name: Security Audit
on: [push, pull_request]
jobs:
  audit:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      # Known vulnerability scanning
      - run: cargo install cargo-audit
      - run: cargo audit

      # Dependency license/policy check
      - run: cargo install cargo-deny
      - run: cargo deny check advisories licenses

      # Static analysis for common patterns
      - run: cargo install cargo-semver-checks
      - run: cargo semver-checks check-release

      # Unsafe code detection
      - run: |
          echo "Unsafe blocks found:"
          grep -r "unsafe" --include="*.rs" crates/ || echo "None"

      # Secret scanning
      - uses: trufflesecurity/trufflehog@main
        with:
          path: ./
          base: main
```

### Required Tools

| Tool | Purpose | Install |
|------|---------|---------|
| `cargo-audit` | Scan dependencies for known CVEs | `cargo install cargo-audit` |
| `cargo-deny` | Ban vulnerable crates, check licenses | `cargo install cargo-deny` |
| `cargo-semver-checks` | Detect breaking API changes | `cargo install cargo-semver-checks` |
| `cargo-geiger` | Count unsafe code | `cargo install cargo-geiger` |
| `semgrep` | Custom security rule scanning | `brew install semgrep` |

### Semgrep Rules (Custom)

Create `.semgrep/callchain-security.yml`:

```yaml
rules:
  - id: unchecked-arithmetic
    pattern: $X + $Y
    languages: [rust]
    message: "Use checked_add/checked_sub for balance arithmetic"
    severity: ERROR
    paths:
      include:
        - crates/precompile/src/*.rs
        - crates/protocol/src/*.rs

  - id: unsafe-storage-access
    pattern: unsafe { ... }
    languages: [rust]
    message: "Avoid unsafe blocks in storage access; use StorageRef"
    severity: WARNING
    paths:
      include:
        - crates/precompile/src/*.rs
        - crates/storage/src/*.rs

  - id: todo-in-consensus
    pattern: TODO | FIXME | HACK | XXX
    languages: [rust]
    message: "Unresolved TODO in consensus code"
    severity: WARNING
    paths:
      include:
        - crates/consensus/src/*.rs
```

---

## Phase 2: Fuzzing + Property-Based Testing (Week 2–4)

Add fuzz targets for all critical attack surfaces.

### Fuzz Target: Transaction RLP Decode

```rust
// fuzz/fuzz_targets/tx_decode.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = call_primitives::Transaction::decode_rlp(data);
});
```

### Fuzz Target: Precompile Dispatch

```rust
// fuzz/fuzz_targets/precompile_dispatch.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut precompile = AssetPrecompile::new();
    let _ = precompile.call(data, Address::ZERO, &mock_storage());
});
```

### Fuzz Target: MPT Proof Verification

```rust
// fuzz/fuzz_targets/mpt_proof.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = call_light_client::verify_mpt_proof(data, &[], &[0u8; 32]);
});
```

### Fuzz Target: Balance Arithmetic

```rust
// fuzz/fuzz_targets/balance_arithmetic.rs
#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 48 {
        let balance = u128::from_le_bytes(data[0..16].try_into().unwrap());
        let amount = u128::from_le_bytes(data[16..32].try_into().unwrap());
        let fee = u128::from_le_bytes(data[32..48].try_into().unwrap());
        let _ = balance.checked_sub(amount).and_then(|b| b.checked_sub(fee));
    }
});
```

### Complete Fuzz Target List

| Target | What It Tests | Run Command |
|--------|---------------|-------------|
| `tx_rlp_decode` | Malformed tx injection | `cargo fuzz run tx_rlp_decode` |
| `precompile_dispatch` | Invalid selector crashes | `cargo fuzz run precompile_dispatch` |
| `balance_arithmetic` | Overflow in transfer/mint/burn | `cargo fuzz run balance_arithmetic` |
| `mpt_proof_verify` | Fake proof acceptance | `cargo fuzz run mpt_proof_verify` |
| `signature_recovery` | Invalid sig handling | `cargo fuzz run signature_recovery` |
| `block_header_validate` | Malicious header acceptance | `cargo fuzz run block_header_validate` |

### Property Tests (proptest)

```rust
#[test]
fn prop_balance_never_negative() {
    proptest!(|(balance: u128, amount: u128| {
        let result = checked_sub(balance, amount);
        if let Some(new_balance) = result {
            prop_assert!(new_balance <= balance);
        }
    });
}

#[test]
fn prop_nullifier_never_reused() {
    proptest!(|(nullifier: [u8; 32])| {
        let mut set = NullifierSet::new();
        prop_assert!(set.insert(nullifier));
        prop_assert!(!set.insert(nullifier));
    });
}

#[test]
fn prop_transfer_preserves_total() {
    proptest!(|(sender_bal: u128, recipient_bal: u128, amount: u128| {
        let mut state = MockState::new();
        state.set_balance(1, SENDER, sender_bal);
        state.set_balance(1, RECIPIENT, recipient_bal);

        let old_total = sender_bal + recipient_bal;

        if state.transfer(1, SENDER, RECIPIENT, amount).is_ok() {
            let new_total = state.get_balance(1, SENDER) + state.get_balance(1, RECIPIENT);
            prop_assert_eq!(old_total, new_total);
        }
    });
}
```

### Fuzzing Infrastructure

Run fuzzers 24/7 on GitHub Actions or a dedicated runner:

```yaml
# .github/workflows/fuzz.yml
name: Continuous Fuzzing
on:
  schedule:
    - cron: '0 */6 * * *'  # Every 6 hours
jobs:
  fuzz:
    runs-on: ubuntu-latest
    timeout-minutes: 360
    steps:
      - uses: actions/checkout@v4
      - run: cargo install cargo-fuzz
      - run: cd fuzz && cargo fuzz run tx_rlp_decode --max-total-time=3600
      - run: cd fuzz && cargo fuzz run precompile_dispatch --max-total-time=3600
      - run: cd fuzz && cargo fuzz run balance_arithmetic --max-total-time=3600
```

---

## Phase 3: Community Security Review (Week 4–8)

Launch a public **"Security Review Period"** with token incentives.

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
- `crates/precompile/` — Balance arithmetic, access control
- `crates/bridge/` — Challenge period, fraud proofs
- `crates/shielded/` — Nullifier uniqueness, note soundness
- `crates/governance/` — Voting power, timelock bypasses
- `crates/protocol/` — Transaction validation, fee logic
- `crates/storage/` — State root computation, pruning safety
- `crates/network/` — P2P authentication, message validation
- `crates/rpc/` — Input validation, DoS vectors

## Rules

1. Provide PoC exploit or detailed vulnerability report
2. Allow 90 days for fix before public disclosure
3. No testing on mainnet without approval (testnet only)
4. First valid report per issue wins
5. Issues caused by dependencies without exploitable impact are out of scope

## Submit

Open a **private** GitHub Security Advisory or email security@callchain.org
```

### Promotion Channels

- Post to /r/rust, /r/ethdev, /r/crypto
- Tweet from project account
- Share in Discord #security channel
- Reach out to independent security researchers specializing in Rust/blockchain
- Publish on Immunefi (even without full bug bounty program)

---

## Phase 4: Lightweight Formal Verification

Use freely available tools on the most critical components.

### Kani Model Checker

```bash
cargo install --locked kani-verifier
cargo kani --crate call-protocol
```

### Verified Properties

```rust
// crates/protocol/src/balances.rs
#[cfg(kani)]
#[kani::proof]
fn verify_transfer_preserves_total_supply() {
    let sender = kani::any::<Address>();
    let recipient = kani::any::<Address>();
    let amount = kani::any::<u128>();

    let mut balances = Balances::new();
    balances.set_balance(1, sender, 1000);
    balances.set_balance(1, recipient, 500);

    let old_total = balances.total_supply(1);

    if balances.transfer(1, sender, recipient, amount).is_ok() {
        let new_total = balances.total_supply(1);
        assert_eq!(old_total, new_total);
    }
}
```

| Property | Component | What It Proves |
|----------|-----------|----------------|
| `transfer_preserves_total_supply` | `balances.rs` | No inflation bug |
| `mint_increases_total_supply` | `balances.rs` | Supply accounting correct |
| `burn_decreases_total_supply` | `balances.rs` | Supply accounting correct |
| `allowance_never_exceeds_balance` | `allowances.rs` | No over-approval |
| `nonce_always_increments` | `transaction.rs` | No replay possible |
| `slashing_reduces_stake` | `validator.rs` | Penalty applied correctly |

---

## Phase 5: Competitive Audit (Code4rena Style)

Host a **community audit competition** as a higher-stakes follow-up to Phase 3.

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

### What Participants Do

1. Register via GitHub issue
2. Review code for 2 weeks
3. Submit findings as private GitHub Security Advisories
4. Judges validate and score
5. Payout in CALL tokens after fixes land

**Cost**: $50K–$100K in tokens (vs $250K+ for a firm)
**Benefit**: 50–200 researchers reviewing code simultaneously

---

## Phase 6: Continuous Security Monitoring

Add to CI permanently.

### Daily Security Workflow

```yaml
# .github/workflows/continuous-security.yml
name: Continuous Security
on:
  schedule:
    - cron: '0 0 * * *'  # Daily
jobs:
  scan:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4

      # Dependency vulnerabilities
      - run: cargo audit

      # Fuzz regression (10 minutes per target)
      - run: |
          cargo install cargo-fuzz
          cd fuzz
          cargo fuzz run tx_rlp_decode --max-total-time=600
          cargo fuzz run precompile_dispatch --max-total-time=600
          cargo fuzz run balance_arithmetic --max-total-time=600

      # Kani regression
      - run: cargo kani --crate call-protocol

      # Semgrep
      - run: semgrep --config .semgrep/ --error
```

---

## Phase 7: Documentation & Transparency

### SECURITY.md

Create `SECURITY.md` at repo root:

```markdown
# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| main    | Yes |
| v0.1.x  | Yes |
| < v0.1  | No |

## Reporting

Report vulnerabilities privately:
- GitHub Security Advisory: https://github.com/callchain/call-node/security/advisories/new
- Email: security@callchain.org

## Response Timeline

| Severity | Acknowledgment | Fix Target | Public Disclosure |
|----------|---------------|------------|-------------------|
| Critical | 24 hours | 72 hours | 90 days after fix |
| High | 48 hours | 1 week | 90 days after fix |
| Medium | 1 week | 2 weeks | 90 days after fix |
| Low | 2 weeks | Next release | Next release |

## Bug Bounty

See [docs/bug-bounty.md](bug-bounty.md) for rewards and scope.

## Past Audits

| Date | Type | Findings | Status |
|------|------|----------|--------|
| 2026-05 | Automated tooling + fuzzing | — | In progress |
| 2026-06 | Community review | — | Planned |
| 2026-07 | Competitive audit | — | Planned |
```

---

## Budget Comparison

| Approach | Cost | Timeline | Effectiveness |
|----------|------|----------|---------------|
| Firm audit | $250K–$400K | 8–14 weeks | 5/5 |
| **Community review + fuzzing** | $50K–$100K (tokens) | 6–10 weeks | 4/5 |
| **Competitive audit (Code4rena)** | $50K–$100K (tokens) | 4–6 weeks | 4/5 |
| Tooling only (no incentives) | $0 (compute) | 2–4 weeks | 3/5 |

---

## Recommended Hybrid Path

| Week | Activity | Deliverable |
|------|----------|-------------|
| 1–2 | Set up automated tooling | CI security workflow, cargo-deny config, Semgrep rules |
| 2–4 | Implement fuzz targets + property tests | 6+ fuzz targets, proptest invariants, corpus seeds |
| 4–6 | Kani formal verification | Verified properties for balances, nonces, allowances |
| 6–8 | Launch community security review | Public program, researcher engagement, first reports |
| 8–10 | Host competitive audit competition | Prize pool, judging, payouts |
| 10+ | Continuous fuzzing in CI, bug bounty live | Daily scans, ongoing rewards |

**Total cost**: ~$100K in CALL tokens + compute
**Timeline**: 10 weeks to mainnet-ready confidence
**Deliverable**: Public security report + fixed findings + running fuzz suite

---

## Exit Criteria (When Is This Issue Resolved?)

This issue is resolved when ALL of the following are true:

1. [ ] `cargo audit` and `cargo deny` run in CI on every PR
2. [ ] 6+ fuzz targets run continuously with >100M iterations each
3. [ ] Kani verifies at least 6 critical properties in CI
4. [ ] Community security review completed with >=10 valid findings addressed
5. [ ] Competitive audit completed with all Critical/High findings fixed
6. [ ] `SECURITY.md` published with response timeline
7. [ ] Bug bounty program live with funded reward pool
8. [ ] Public security report published in `docs/audit/`
