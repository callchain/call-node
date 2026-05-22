# ADR-0003: Commonware Simplex BFT with DPoS Validator Election

- Status: Accepted
- Date: 2026-04-13
- Author(s): Callchain Core Team

## Context

Callchain requires a Byzantine-fault-tolerant consensus algorithm that can:

- Finalize blocks in sub-second latency.
- Scale to 100–216 validators without quadratic message blowup.
- Have a production-ready Rust implementation.
- Support validator subset rotation for liveness and censorship resistance.
- Integrate cleanly with the EVM execution layer (single state root, no dual ledger).

The team evaluated three families of BFT algorithms: Simplex (Commonware), Tendermint (Malachite), and HotStuff. The choice directly impacts message complexity, audit surface, and the latency ceiling at the target validator count.

## Decision

Use **Commonware Simplex BFT** (`commonware-consensus` crate) with the following parameters:

| Parameter | Value |
|-----------|-------|
| Block time | 250 ms |
| Epoch length | 100 blocks |
| Max validators | 216 |
| Per-epoch proposer subset | 21 |
| Min self-stake | 1,000,000 CALL |
| Unbonding period | 120,960 blocks (~8.4 h at 250 ms) |
| Offline slash rate | 0.1% per round (10 bps) |
| Churn limit quotient | 16 |
| Safety ratio | 4/3 (safety floor = ceil(21 * 4/3) = 28) |

### Proposer Selection

- **VRF-based subset selection**: At each epoch boundary, a deterministic 21-validator subset is selected from the qualified validator set using `keccak256(VRF_DOMAIN || seed || pubkey)` sortition. The seed is derived from the previous block hash and round number, making it unbiasable.
- **Round-robin proposer**: Within the subset, the proposer for a given round is selected as `subset[round % 21]`.
- **Epoch churn**: At epoch boundaries, validators whose unbonding period has elapsed are auto-exited, subject to a churn limit (`max(2, qualified / 16)`). Validators whose stake drops below the minimum are also auto-exited.

### Consensus-EVM Integration

- Validator stakes, statuses, and pubkeys are stored in EVM storage under the `VALIDATOR_ADDRESS` (`0x204`) precompile.
- `SimplexConsensus` reads validator state via `ProtocolStorage` accessors in `crates/consensus/src/exec/state_accessors.rs`.
- Block rewards, slashing, and oracle rewards are applied through the `CallchainBlockExecutor` trait, implemented by `EvmBlockExecutor`.
- The consensus state (`current_height`, `current_round`, `proposer_subset`, `last_block_hash`) is serialized to `PersistedConsensusState` and stored in MDBX via `CallConsensusState`.

### Block Structure

```rust
struct Block {
    header: BlockHeader,
    evm_txs: Vec<EvmTx>,
}

struct BlockHeader {
    parent_hash: BlockHash,
    height: u64,
    timestamp_millis: u64,
    state_root: Hash,        // EVM state root (single source of truth)
    proposer: ValidatorId,
    signature: Signature,
    version: ProtocolVersion,
    bls_aggregate_signature: Option<Vec<u8>>,
    bls_signer_bitmap: Vec<u8>,
}
```

### Slashing

- **Double-sign**: 100% of self-stake is slashed; validator is removed from the active set.
- **Offline**: 0.1% of current self-stake per round offline, applied cumulatively.
- **Oracle outlier**: 0.1% of self-stake.

## Consequences

### Positive

- **O(n) message complexity**: With 216 validators, Simplex produces ~216 messages per round. Tendermint's O(n²) would produce ~46,000 messages per round — a 200× difference that directly determines the latency ceiling.
- **Smallest audit surface**: Simplex has the lowest state-machine complexity among the evaluated options, reducing the risk of consensus bugs.
- **Production Rust implementation**: `commonware-consensus` is actively maintained and already used in production-like environments (e.g., Tempo chain).
- **Native subset rotation**: VRF-based subset selection is built into the design, improving censorship resistance and liveness without extra protocol layers.
- **Graceful degradation**: The consensus engine automatically pauses block production during network partitions and resumes immediately upon recovery.
- **Sub-second finality**: With a 250 ms block time and single-round confirmation, finality is achieved in ~500 ms (2 rounds).

### Negative / Trade-offs

- **Relatively new algorithm**: Simplex has less long-term battle testing than Tendermint (which powers Cosmos) or HotStuff (which powers several major chains). The team accepts this risk because the implementation is simpler and the Commonware team is responsive.
- **BLS aggregation is optional**: The block header includes `bls_aggregate_signature` and `bls_signer_bitmap` for light client support, but the core consensus path uses Ed25519 signatures. Full BLS aggregation is not yet wired into the critical path.
- **Validator count floor**: The protocol expects 100–216 validators. Below 100, the safety assumptions of the subset-selection model weaken. Above 216, the O(n) message count begins to stress network bandwidth at 250 ms block times.
- **Epoch churn latency**: Validators must wait up to 100 blocks (~25 s) plus the unbonding period (~8.4 h) before being removed. This is conservative for safety but may feel slow to operators.
- **Reward distribution is post-execution**: Block rewards and oracle rewards are currently applied after EVM transaction execution in `Block::execute`. The ADR-0002 note indicates these should eventually become system transactions inside the EVM block for a fully unified state root.

## Alternatives Considered

| Dimension | Simplex (Chosen) | Tendermint (Malachite) | HotStuff |
|-----------|------------------|------------------------|----------|
| Communication complexity | O(n) | O(n²) | O(n) |
| State machine complexity | Lowest | Medium | High |
| Rust implementation maturity | `commonware-consensus` ready | `malachite-bft` available | No mature open-source Rust impl |
| Subset rotation | Native | Requires extra impl | Native |
| Audit surface | Smallest | Medium | Large |
| Production validation | Tempo chain | Arc chain | PlasmaBFT (closed source) |

- **Tendermint / Malachite**: Rejected primarily due to O(n²) message complexity. At 216 validators, the message volume would be prohibitive for 250 ms block times.
- **HotStuff**: Rejected because there is no mature, open-source Rust implementation available. Reimplementing HotStuff from scratch would exceed the project's security-audit budget.
- **PBFT / pBFT variants**: Rejected because they lack native subset rotation and have higher message complexity than Simplex.

## References

- `crates/consensus/src/simplex.rs` — `SimplexConsensus`, epoch churn, block commit/validate
- `crates/consensus/src/proposer.rs` — `ConsensusParams`, VRF subset selection, proposer rotation
- `crates/consensus/src/block.rs` — `Block`, `BlockHeader`, `BlockExecutionResult`
- `crates/consensus/src/exec/state_accessors.rs` — EVM storage accessors for validator state
- `docs/spec.md` §2.1, §2.3, §2.4, §2.5 — consensus algorithm, validators, block structure, execution order
