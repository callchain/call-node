# Security Policy

## Supported Versions

| Version | Supported |
|---------|-----------|
| main    | Yes |
| v0.1.x  | Yes |
| < v0.1  | No |

## Reporting a Vulnerability

If you discover a security vulnerability in call-node, please report it responsibly.
**Do not open a public issue.**

- **GitHub Security Advisory**: https://github.com/callchain/call-node/security/advisories/new
- **Email**: security@callchain.cc
- **Include**: A detailed description of the vulnerability, steps to reproduce, and potential impact.

We will respond within **48 hours** and work with you to resolve the issue.

## Response Timeline

| Severity | Acknowledgment | Fix Target | Public Disclosure |
|----------|---------------|------------|-------------------|
| Critical | 24 hours | 72 hours | 90 days after fix |
| High | 48 hours | 1 week | 90 days after fix |
| Medium | 1 week | 2 weeks | 90 days after fix |
| Low | 2 weeks | Next release | Next release |

## Responsible Disclosure

- Report vulnerabilities privately to our security team.
- Allow us time to investigate and patch before public disclosure.
- We appreciate coordinated disclosure and will credit reporters where appropriate.

## Scope

In-scope:
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

Out-of-scope:
- Issues caused by dependencies without exploitable impact
- Social engineering or infrastructure attacks
- Documentation-only issues

## Security Measures in Place

- **Automated scanning**: `cargo audit`, `cargo deny`, `cargo geiger`, Semgrep rules
- **Continuous fuzzing**: 6 fuzz targets running on every PR and daily
- **Property-based testing**: `proptest` invariants for balances, nonces, rate limiting
- **Formal verification**: Kani model checker proofs for critical arithmetic invariants
- **CI enforcement**: Security checks run on every push/PR via GitHub Actions

## Past Reviews

| Date | Type | Findings | Status |
|------|------|----------|--------|
| 2026-05 | Automated tooling + fuzzing | — | In progress |
| 2026-06 | Community review | — | Planned |
| 2026-07 | Competitive audit | — | Planned |
