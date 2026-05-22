# Callchain Architecture Decision Records (ADRs)

## What is an ADR?

An Architecture Decision Record (ADR) captures a significant architectural decision made during the development of Callchain, along with its context and consequences. ADRs are immutable once accepted: if a decision is later reversed or superseded, the original ADR is marked **Deprecated** and a new ADR is created to document the replacement.

## Why ADRs?

- **Preserve rationale**: Future maintainers can understand *why* a decision was made, not just *what* was done.
- **Onboard faster**: New contributors can read ADRs instead of reconstructing history from commit messages.
- **Resolve debates**: When the same question arises again, the ADR serves as the reference point.
- **Audit trail**: External auditors and partners can review the reasoning behind security-critical choices.

## How to Propose an ADR

1. **Draft**: Copy `adr-template.md` to `NNNN-<short-title>.md` using the next available number.
2. **Status**: Set status to `Proposed`.
3. **Review**: Open a PR or discussion thread. Link the ADR in the PR description.
4. **Accept or Reject**: After consensus, update the status to `Accepted` or `Rejected`. Merge the PR.
5. **Deprecate**: If later superseded, mark the ADR `Deprecated` and reference the replacement ADR.

## ADR Index

| Number | Title | Status |
|--------|-------|--------|
| [0001](0001-use-mdbx-as-sole-storage.md) | Use MDBX (reth-db) as Sole Persistence Backend | Accepted |
| [0002](0002-unified-evm-execution.md) | Unified EVM Execution via Precompiled Contracts | Accepted |
| [0003](0003-simplex-bft-consensus.md) | Commonware Simplex BFT with DPoS Validator Election | Accepted |

## Template

See [adr-template.md](adr-template.md).
