# Callchain Frequently Asked Questions (FAQ)

## General

### What is Callchain?

Callchain is a Layer 1 blockchain with unified EVM execution. Every protocol operation — payments, governance votes, oracle submissions, bridge deposits — executes inside the EVM via precompiled contracts (`0x101`–`0x209`). Consensus is Commonware Simplex BFT with DPoS validator election.

### What makes Callchain different from Ethereum?

- **Unified execution layer**: No dual ledger; protocol state lives in EVM storage slots under precompile addresses
- **Native precompiles**: Asset registry, staking, governance, oracle, bridge, and shielded transactions are all first-class EVM precompiles
- **Sub-400ms block time**: 250ms target with Simplex BFT finality
- **Built-in compliance**: Per-asset sanctions lists and issuer policies at the protocol level

### What is the native token?

**CALL** — 18 decimal places, used for gas, staking, governance deposits, and all protocol fees.

---

## Node Operations

### What hardware do I need?

| Node Type | CPU | RAM | Disk | Network |
|-----------|-----|-----|------|---------|
| Full node (pruned) | 4 cores | 16 GB | 500 GB SSD | 100 Mbps |
| Full node (recommended) | 8 cores | 32 GB | 1 TB NVMe | 1 Gbps |
| Archive node | 8+ cores | 64 GB | 2+ TB NVMe | 1 Gbps |
| Validator | 8 cores | 32 GB | 1 TB NVMe | 1 Gbps |

### How long does initial sync take?

- **Full node**: ~2–4 hours per 100K blocks (depends on block complexity and peer latency)
- **Fast sync (snapshot)**: ~5–15 minutes to validate a state snapshot and catch up remaining blocks
- **Archive node**: Significantly longer; all historical state must be replayed

### What is the difference between Full, Archive, and Light nodes?

| Mode | Blocks | Historical State | Use Case |
|------|--------|------------------|----------|
| Full | All | Recent 50K blocks + receipts | Standard RPC, dApp backend |
| Archive | All | All historical state | Block explorer, indexer |
| Light | Headers only | None | Wallet, mobile, edge |

Full and Archive nodes differ only in pruning configuration. A Full node can be converted to Archive only by resyncing with `--archive`.

### How fast does disk usage grow?

| Node Type | ~1 Month | ~6 Months | ~1 Year |
|-----------|----------|-----------|---------|
| Full (pruned) | ~50 GB | ~200 GB | ~350 GB |
| Archive | ~200 GB | ~1.2 TB | ~2.5 TB |

Actual growth depends on transaction volume. See [`how-to/for-operator.md`](how-to/for-operator.md) for pruning configuration.

### Can I run multiple nodes on the same machine?

Yes, but each node must use a **different `--data-dir`**. Never share an MDBX database between two running processes — it will cause "MDBX lock contention" errors.

---

## RPC and APIs

### What RPC methods are available?

- **47 standard `eth_*` methods** — full Ethereum JSON-RPC compatibility including historical state, Merkle proofs, Filter API, and WebSocket subscriptions
- **30+ `call_*` read-only methods** — native protocol queries (asset info, validator list, oracle prices, governance proposals)

See [`eth_rpc.md`](eth_rpc.md) for the per-method audit and [`rpc.md`](rpc.md) for the full endpoint list.

### Which network does MetaMask connect to?

Add Callchain as a custom network:

| Field | Value |
|-------|-------|
| Network Name | Callchain Testnet |
| RPC URL | `https://rpc.testnet.callchain.org` |
| Chain ID | (from genesis) |
| Currency Symbol | CALL |
| Block Explorer | `https://explorer.testnet.callchain.org` |

### Do you support `eth_sendTransaction`?

Yes, but only for accounts loaded in the node's local keystore. For external wallets (MetaMask, Ledger), use `eth_sendRawTransaction` with a signed RLP-encoded transaction.

### Are WebSocket subscriptions supported?

Yes — `eth_subscribe` supports:
- `newHeads` — new block headers
- `logs` — filtered log events
- `newPendingTransactions` — pending tx hashes

Callchain-specific subscriptions (`call_subscribe*`) support block announcements, payment events, bridge completions, and more. See [`rpc.md`](rpc.md).

---

## Transactions and Gas

### What is the block time?

Target **250ms** per block (~4 blocks/second). Block time is configurable per chain but fixed at the protocol level.

### What is the gas limit per block?

Fixed at `30,000,000` gas (`0x1c9c380`).

### How is gas priced?

Callchain uses EIP-1559-style pricing:

| Component | Value |
|-----------|-------|
| Base fee | Protocol-calculated per block |
| Minimum priority fee | 1 wei (`MIN_PRIORITY_FEE_PER_GAS`) |
| Suggested gas price | `base_fee + 1` wei |

### How do I send a protocol transaction (e.g., transfer, stake)?

All state changes go through `eth_sendRawTransaction` targeting a precompile address:

```solidity
// Example: transfer asset via precompile 0x201
// function selector: transfer(uint64,address,uint128)
to: 0x0000000000000000000000000000000000000201
data: 0xa9059cbb...  // ABI-encoded
```

See [`precompile.md`](precompile.md) for the full ABI reference.

### What happens if my transaction fails?

Failed transactions are included in the block and consume gas. The receipt contains:
- `status: 0` (failure)
- `revertReason`: decoded revert string (if applicable)

Common failure reasons:
- Insufficient balance for gas
- Invalid nonce
- Precompile revert (e.g., invalid asset ID, compliance check failure)

---

## Staking and Validation

### How do I become a validator?

1. Stake minimum 1,000,000 CALL via precompile `0x204`
2. Register an Ed25519 consensus key
3. Pass compliance check
4. Wait for next epoch boundary (every 100 blocks)

See [`how-to/for-validator.md`](how-to/for-validator.md).

### What is the unbonding period?

- **Testnet**: ~8.4 hours
- **Mainnet**: ~84 hours max

During unbonding, stake is locked but not earning rewards.

### What are the slashing conditions?

| Offense | Penalty |
|---------|---------|
| Double-sign | 100% of stake |
| Offline (missed rounds) | 0.1% per round |
| Oracle outlier (>5% from median) | 0.1% of stake |

---

## Shielded Transactions

### Do I need a prover service?

Yes. Shielded transactions require ZK proof generation, which is CPU-intensive (~5–10 seconds per proof). Options:

1. **Local proving**: Run `call-prover` alongside your node
2. **Remote proving**: Use a dedicated prover service
3. **CLI wallet**: `calld wallet shielded-transfer` handles proving internally

### What are the minimum and maximum amounts?

- **Minimum**: Protocol-enforced floor (check `shielded.md` for current value)
- **Maximum**: Limited by U128 balance range and Merkle tree capacity

### Can I view my shielded balance without exposing the private key?

Yes. Use a viewing key (`ivk`) to scan the shielded pool and decrypt notes belonging to your address. The viewing key cannot spend.

---

## Bridge

### How do bridge deposits work?

1. User deposits tokens into the Ethereum bridge contract
2. Validators observe the `BridgeDeposit` event
3. Validators sign the deposit attestation with secp256k1
4. Once 14+ signatures (2/3 of 21) are collected, the aggregator calls `externalDeposit()` on precompile `0x103`
5. Tokens are minted on Callchain

### What is the challenge period?

~14 days. During this window, anyone can challenge a deposit by posting a bond. If the challenge is valid, the deposit is reversed.

### Is there a daily deposit limit?

Yes — per-asset `daily_limit_per_asset` is enforced by the bridge precompile.

---

## Governance

### How do I submit a proposal?

Call `submitProposal(uint8,string,string,bytes)` on precompile `0x203` with a governance deposit. See [`governance.md`](governance.md) for proposal types.

### What is the timelock?

After a proposal passes voting, it enters a timelock before execution. This prevents rushed changes.

### Can the chain be paused?

Yes. Validators can trigger an emergency pause via `emergencyPause()` on precompile `0x203`. While paused, all state-mutating transactions are rejected.

---

## Troubleshooting

### "MDBX lock contention"

**Cause**: Two `calld` processes trying to open the same `--data-dir`.
**Fix**: Ensure each node uses a unique data directory. Check with `lsof | grep mdbx`.

### "No peers connected"

**Cause**: Bootstrap peers unreachable or peer ID mismatch.
**Fix**: Verify `bootstrap_peers` in config. Check network connectivity with `telnet bootstrap.callchain.org 51235`.

### "Consensus timeout"

**Cause**: Fewer than 2/3 validators are online.
**Fix**: Check validator health dashboards. If you're a validator, check your node logs for missed rounds.

### "Rate limit exceeded"

**Cause**: RPC request rate exceeds `rate_limit_rps`.
**Fix**: Increase `rate_limit_rps` in config, or whitelist your IP.

### High memory usage

**Cause**: Archive mode, large state, or memory leak.
**Fix**:
- Switch from archive to full mode (requires resync)
- Reduce `db_cache_size`
- Check for pending snapshots in `data_dir/snapshots/`

### Sync never completes

**Cause**: Out-of-sync peers, corrupt database, or network partition.
**Fix**:
1. Check `eth_syncing` — is `highestBlock` increasing?
2. Restart node to find new peers
3. If database is corrupt, wipe `mdbx/` and re-sync

---

## Getting Help

- **Documentation**: This `docs/` directory
- **Developer How-To**: [`how-to/for-developer.md`](how-to/for-developer.md)
- **Operator How-To**: [`how-to/for-operator.md`](how-to/for-operator.md)
- **Validator How-To**: [`how-to/for-validator.md`](how-to/for-validator.md)
- **GitHub Issues**: https://github.com/callchain/call-node/issues
