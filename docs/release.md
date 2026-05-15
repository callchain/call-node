# Callchain Release and Operations Guide

**Version**: 0.1.0-testnet
**Last updated**: 2026-04-20

---

## Release Process

### Versioning

Callchain follows [SemVer](https://semver.org/):
- **MAJOR**: Breaking protocol changes, hard forks
- **MINOR**: New features, backward-compatible protocol additions
- **PATCH**: Bug fixes, security patches

### Release Checklist

```
1. Update CHANGELOG.md
   - Move [Unreleased] items to new version section
   - Add release date

2. Update version in Cargo.toml workspace
   sed -i 's/^version = ".*"/version = "X.Y.Z"/' Cargo.toml

3. Run full test suite
   cargo test --workspace

4. Run clippy and formatting
   cargo clippy --workspace -- -D warnings
   cargo fmt -- --check

5. Build release artifacts
   cargo build --release
   docker build -t callchain:vX.Y.Z .

5.1. Sign release binaries
   # Generate SHA-256 checksums
   sha256sum target/release/callchaind > callchaind-vX.Y.Z.sha256
   # Sign with release GPG key (security@callchain.org)
   gpg --armor --detach-sign callchaind-vX.Y.Z.sha256
   # Verify signature
   gpg --verify callchaind-vX.Y.Z.sha256.asc callchaind-vX.Y.Z.sha256

6. Create git tag
   git tag -a vX.Y.Z -m "Release vX.Y.Z"
   git push origin vX.Y.Z

7. Create GitHub Release
   - Attach binary artifacts
   - Attach Docker image digest
   - Link to CHANGELOG

8. Announce in #releases Discord/Slack
```

### Release Branches

| Branch | Purpose |
|---|---|
| `main` | Stable, tagged releases only |
| `dev` | Active development, merges to `main` via PR |
| `release/vX.Y` | Release candidates, cherry-picks from `dev` |
| `hotfix/*` | Critical security fixes, fast-track to `main` |

---

## Key Management

### Supported Key Storage Backends

Callchain supports three signer backends via the `Signer` trait:

| Backend | Feature Flag | Use Case | Security |
|---|---|---|---|
| **Local** (plaintext) | default | Devnet, CI, local testing | Private key in memory only |
| **HashiCorp Vault** | `hashi-vault` | Production validators | Key in Vault transit engine; audit log |

### Configuration

```toml
# config.toml — Local key (devnet only)
[keys]
validator_key = "0x..."

# config.toml — HashiCorp Vault (production)
[keys]
vault_addr = "https://vault.example.com:8200"
vault_token = "hvs.XXXXXXXX"
vault_key_name = "callchain-validator"
```

### Boot-Time Key Loading

The boot sequence (`boot.rs`) loads keys in priority order:

```
1. Vault key_name   → HashiVaultSigner (if hashi-vault feature enabled)
2. Keystore path    → LocalSigner from encrypted keystore
3. Plaintext key    → LocalSigner (warns in production)
```

### Threshold Signing (M-of-N)

**Current status**: Not yet implemented. The `Signer` trait is single-party. Threshold signing is on the roadmap.

**Recommended approach** for production:
- Use **HashiCorp Vault with Shamir seal unseal** (M-of-N operators to unseal Vault)
- For consensus-level threshold signing, integrate `commonware-cryptography` threshold Ed25519 when available

**Future implementation plan**:
```rust
// ThresholdSigner: M-of-N Ed25519 threshold signatures
pub struct ThresholdSigner {
    share_id: u32,
    total_shares: u32,
    threshold: u32, // M
    key_share: [u8; 32],
}

impl Signer for ThresholdSigner {
    fn sign(&self, msg: &[u8; 32]) -> Result<Signature, SignerError> {
        // Generate partial signature
        // Combine with other validators' partial signatures
        // Return aggregated signature when threshold reached
    }
}
```

### Key Rotation Protocol

**Goal**: Rotate validator signing keys without downtime or consensus disruption.

**Process**:

```
Phase 1: Prepare new key
- Generate new key in HSM/KMS
- Register new public key with validator set (via governance proposal)
- Wait for timelock (7 days)

Phase 2: Activate new key
- Update node config to use new key
- Restart node (graceful, uses BFT timeout to hand over)
- Old key remains valid for 1 epoch to handle in-flight consensus messages

Phase 3: Revoke old key
- Submit governance proposal to remove old pubkey
- Wait for timelock
- Old key permanently rejected
```

**Emergency rotation** (key compromise):
1. Any validator initiates `GovernanceEmergencyPause`
2. Compromised key is immediately removed from validator set via emergency proposal
3. All validators rotate keys within 24 hours
4. Resume operations via `GovernanceEmergencyResume`

### Key Security Checklist

| Check | Frequency | Tool |
|---|---|---|
| No plaintext keys in production | Every release | `grep -r "validator_key =" config/` |
| Vault token has minimal scope | Quarterly | Vault policy review |
| Key access logs reviewed | Weekly | CloudTrail / Vault audit log |
| Key rotation performed | Annually | Scheduled maintenance window |

---

## Genesis Configuration

### Mainnet Genesis (Gap 4)

Production deployment requires a finalized genesis configuration that all validators agree on. The current repository only contains testnet/devnet genesis files.

**Required genesis fields** (`chainspec/mainnet.json`):

```json
{
  "chain_id": 8888,
  "network_name": "Callchain Mainnet",
  "genesis_time": "2026-06-01T00:00:00Z",
  "block_time_millis": 250,
  "initial_validators": [
    {
      "address": "0x...",
      "public_key": "0x...",
      "stake": "1000000000000000000000000"
    }
  ],
  "initial_balances": {
    "0x...": "1000000000000000000000000000"
  },
  "governance": {
    "timelock_period_blocks": 2419200,
    "proposal_deposit": "10000000000000000000000",
    "min_validator_stake": "1000000000000000000000000"
  },
  "bridge": {
    "challenge_period_blocks": 2419200,
    "min_validator_signatures": 14,
    "total_validators": 21
  },
  "shielded": {
    "merkle_tree_depth": 32,
    "nullifier_set_initial_capacity": 1048576
  }
}
```

**Genesis creation checklist**:

1. **Validator set** — Collect public keys and initial stakes from genesis validators (minimum 7, recommended 21)
2. **Token distribution** — Define initial balances for team, investors, community treasury, ecosystem fund
3. **Governance parameters** — Set timelock, proposal deposit, quorum thresholds via governance proposal before launch
4. **Chain ID** — Register at [chainlist.org](https://chainlist.org) to avoid collisions
5. **Genesis hash** — All validators must verify `sha256sum chainspec/mainnet.json` matches before first block

**Testnet genesis** (`chainspec/testnet.json`) should mirror mainnet structure with lower stakes and shorter timelocks for rapid iteration.

---

## Security Audit

### Third-Party Audit (Gap 3)

Before mainnet deployment, the codebase must undergo a comprehensive security audit by a reputable firm. No production deployment should proceed without a published audit report.

**Recommended audit firms** (in no particular order):
- Trail of Bits
- OpenZeppelin
- Zellic
- OtterSec
- Spearbit

**Audit scope** (priority order):

1. **Protocol layer** — precompile execution balance calculations, fee logic, allowance enforcement, batch transfer correctness
2. **Bridge security** — Signature threshold enforcement (`min_validator_signatures`), challenge period logic, deposit finalization, `revoke_pending_external_deposit`
3. **ZK circuits** — Halo2 circuit constraints, universal parameters, nullifier uniqueness, note commitment soundness
4. **Governance** — Proposal lifecycle, voting power calculation, timelock enforcement, emergency pause/resume safety
5. **Consensus** — Block execution ordering, state root computation, slashing conditions, fork choice rules
6. **Cryptography** — Signature verification (Ed25519, secp256k1, BLS), keystore encryption, key derivation

**Audit readiness checklist**:

| Prerequisite | Status | Notes |
|---|---|---|
| All Phase 1 test gaps closed | In progress | See `docs/testing.md` |
| Fuzz tests running for 7+ days | Not started | Target: 100M+ iterations |
| Internal security review complete | Not started | Protocol team + external advisor |
| Documentation accurate and current | In progress | All spec sections validated |
| No critical or high open issues | Not started | Blocker for audit kickoff |

**Post-audit process**:
1. Receive draft report, fix findings within agreed timeline
2. Publish final report in `docs/audit/YYYY-MM-firm-name.pdf`
3. Create public issue tracker for each finding with severity label
4. Re-audit fixes if critical/high findings were found

---

## Performance Baselines

### Benchmark Suite (Gap 11)

Establish measurable performance baselines before testnet launch. Targets must be validated on production-equivalent hardware.

**Recommended benchmark suite** (using `criterion.rs`):

```bash
# Hot path benchmarks
cargo bench -p call-precompile --bench precompile_execute
cargo bench -p call-consensus --bench block_production
cargo bench -p call-crypto --bench signature_verify
cargo bench -p call-storage --bench mdbx_read_write
cargo bench -p call-shielded --bench proof_generate
cargo bench -p call-light-client --bench mpt_verify
```

**Target baselines** (single validator, AWS c6i.2xlarge):

| Metric | Target | Measurement |
|---|---|---|
| Block production time | < 250ms | `criterion` median over 1000 blocks |
| Precompile execution throughput | > 5,000 tx/s | Single-block max capacity |
| Signature verification (Ed25519) | > 50,000 sigs/s | Batch verify 1,000 signatures |
| MPT proof verification | < 5ms | Single receipt proof |
| ZK proof generation (deposit) | < 30s | `real-prover` feature enabled |
| State root computation | < 50ms | Full balance tree hash |
| MDBX write latency (p99) | < 10ms | Commit 1,000 KV pairs |

**E2E stress test targets**:

| Scenario | Target | Duration |
|---|---|---|
| Sustained throughput | 1,000 TPS | 1 hour |
| Burst throughput | 10,000 TPS | 30 seconds |
| Multi-sender stress | 100 concurrent senders | 10 minutes |
| Rapid block production | 500 blocks | < 3 minutes |
| Memory stability | < 2GB RSS growth | 1 hour sustained load |

**Real-world validation requirements**:
- Minimum 4 validators across 3 geographic regions
- Network latency: 50-200ms between validators
- Packet loss simulation: 0.1%, 1%, 5%
- Hardware heterogeneity: mix of AWS, GCP, bare metal

---

## Deployment

### Docker Deployment

```bash
# Pull release image
docker pull ghcr.io/callchain/callchaind:v0.1.0-testnet

# Run with config
docker run -d \
  --name callchain-node \
  -v /etc/callchain:/config \
  -v /var/lib/callchain:/data \
  -p 8545:8545 \
  -p 8546:8546 \
  -p 30303:30303 \
  ghcr.io/callchain/callchaind:v0.1.0-testnet \
  --config /config/config.toml
```

### Systemd Service

```ini
# /etc/systemd/system/callchaind.service
[Unit]
Description=Callchain Node
After=network.target

[Service]
Type=simple
User=callchain
Group=callchain
ExecStart=/usr/local/bin/callchaind --config /etc/callchain/config.toml
Restart=always
RestartSec=10
Environment="RUST_LOG=info,callchain=debug"
Environment="CALL_KEYSTORE_PASS_FILE=/etc/callchain/keystore.pass"

[Install]
WantedBy=multi-user.target
```

### Monitoring

| Metric | Alert Condition | Action |
|---|---|---|
| `callchain_blocks_produced_total` | Flat for > 60s | Check consensus health |
| `callchain_p2p_peers_connected` | < min_healthy_peers | Check network connectivity |
| `callchain_mempool_size` | > 10,000 | Check block production |
| `callchain_bridge_pending_count` | Growing unbounded | Check bridge finalization |
| `mdbx_db_size_bytes` | > 80% disk | Trigger prune or expand storage |

### Canary Deployment

Before rolling out a release to all validators, deploy to a small canary subset:

```
1. Select 5-10% of validator nodes for canary
2. Deploy new version to canary nodes
3. Monitor for 4 hours:
   - Block production rate (should match non-canary)
   - Error rate in logs
   - Memory and CPU usage
   - P2P peer connectivity
4. If all healthy, proceed to full rollout
5. If issues detected, halt rollout and investigate
```

**Canary gating criteria**:

| Check | Threshold | Action if failed |
|---|---|---|
| Block production rate | > 95% of target | Pause, investigate logs |
| Consensus round time | < 500ms p99 | Pause, check network |
| Memory growth | < 10% vs baseline | Pause, check for leaks |
| Error rate | < 0.1% of log lines | Pause, check errors |

### Rollback Procedure

If a release causes consensus halts, state corruption, or critical bugs:

```
1. Identify the issue via monitoring / validator reports
2. Stop callchaind on affected nodes: systemctl stop callchaind
3. Restore previous binary from backup:
   cp /usr/local/bin/callchaind.backup /usr/local/bin/callchaind
4. If database schema changed, restore database from pre-upgrade snapshot:
   # Snapshot taken before upgrade
   rm -rf /var/lib/callchain/mdbx
   tar xzf /backup/callchain-pre-upgrade.tar.gz -C /var/lib/callchain
5. Restart node: systemctl start callchaind
6. Verify reconnection to peers and block sync
7. File post-mortem issue with rollback tag
```

**Pre-upgrade checklist** (prevents rollback pain):

- [ ] Take database snapshot before upgrade
- [ ] Back up current binary as `callchaind.backup`
- [ ] Back up config files
- [ ] Announce maintenance window to validators
- [ ] Have >= 2/3 validators coordinate upgrade timing

---

## Compliance Data Sync

### Environment Configuration

```bash
# Optional: URL for sanctioned address list
export CALL_COMPLIANCE_DATA_URL="https://api.example.com/compliance/sanctioned"
```

The node will fetch this URL every 5 minutes and update the compliance blacklist in EVM storage under the compliance precompile (`0x205`).

**Expected response format**:
```json
["0x1234...", "0xabcd...", "0xef01..."]
```

---

## Bug Bounty Program (Gap 12)

Launch a public bug bounty program before testnet goes live to incentivize white-hat security research.

**Recommended platform**: [Immunefi](https://immunefi.com) or [HackerOne](https://hackerone.com)

**Bounty tiers**:

| Severity | Reward (USD) | Examples |
|---|---|---|
| Critical | $50,000 - $250,000 | Theft of funds, consensus halt, invalid state transition, ZK proof bypass |
| High | $10,000 - $50,000 | DoS on validator set, bridge bypass without 14/21 sigs, governance takeover |
| Medium | $2,000 - $10,000 | RPC DoS, mempool spam bypass, compliance engine bypass |
| Low | $500 - $2,000 | Information disclosure, configuration issues, documentation errors |

**Scope**:
- `crates/protocol/` — Precompile execution, balance logic, fee calculation
- `crates/bridge/` — Deposit validation, signature threshold, challenge period
- `crates/consensus/` — Block production, BFT rounds, fork choice
- `crates/shielded/` — ZK circuits, nullifier tracking, note commitments
- `crates/governance/` — Proposal lifecycle, voting, timelock
- `crates/rpc/` — Authentication, rate limiting, input validation
- Smart contracts deployed on Callchain EVM (if any)

**Out of scope**:
- Frontend/UI bugs
- Social engineering
- Physical attacks
- Bugs in dependencies without exploitable impact on Callchain

**Rules**:
1. No testing on mainnet without explicit written approval
2. Testnet exploitation is allowed if funds are returned
3. Provide detailed PoC and remediation suggestion
4. Allow 90 days for fix before public disclosure (coordinated disclosure)
5. No bounty for issues already reported or in audit report

**Contact**: `security@callchain.org` (GPG key in `SECURITY.md`)

---

## Testnet Validation

### Light Client Real-Network Testing (Gap 13)

The light client is unit-tested against mock MPT proofs but has not been validated against a live Ethereum node.

**Validation plan**:

1. **Connect to real Ethereum RPC** (Alchemy/Infura/mainnet node)
   ```bash
   export ETH_RPC_URL="https://eth-mainnet.g.alchemy.com/v2/..."
   export CALL_LIGHT_CLIENT_START_BLOCK=21000000
   ```

2. **Verify header chain submission** for 1,000+ consecutive blocks
   - Check that all submitted headers pass PoW/PoS validation
   - Verify difficulty/TD matches mainnet

3. **Verify receipt proofs** for real bridge deposits
   - Query `eth_getTransactionReceipt` for deposit transactions
   - Verify MPT proof against submitted header's receipt root
   - Test with both legacy and EIP-2718 typed receipts

4. **Reorg handling test**
   - Monitor for chain reorgs > 6 blocks deep
   - Verify orphaned headers are correctly removed and new canonical chain is followed

5. **Run for 7 days** against mainnet without intervention
   - Log all failures, compare state with reference implementation

### Fork Upgrade Heterogeneous Testing (Gap 14)

Height-activated upgrades have only been tested in simulation with identical node versions.

**Validation plan**:

1. **Deploy mixed-version testnet**
   - 50% nodes running current version (v0.1.0)
   - 50% nodes running previous version (v0.0.9)
   - Run for 48 hours to establish baseline

2. **Schedule upgrade at known height** (e.g., height 10,000)
   - Only upgraded nodes should apply new rules after height 10,000
   - Non-upgraded nodes should reject blocks with new precompiles

3. **Verify backward compatibility**
   - Old nodes can still sync pre-upgrade blocks
   - Old nodes gracefully disconnect from peers running new protocol version

4. **Verify forward compatibility**
   - Upgraded nodes accept blocks from old nodes before upgrade height
   - No consensus split if > 2/3 validators upgrade before activation height

5. **Governance-triggered upgrade test**
   - Submit upgrade proposal via governance
   - Vote and queue with timelock
   - Verify `ForkManager` persists scheduled upgrade across restart
   - Verify `check_upgrades_at_height` handles multiple upgrades at same height correctly

**Success criteria**:
- Zero consensus halts during upgrade window
- All nodes reach same final height within 100 blocks post-upgrade
- No invalid state transitions observed

---

## Emergency Procedures

See [docs/runbooks/](runbooks/):
- [chain-halt.md](runbooks/chain-halt.md) — Consensus stop diagnosis and recovery
- [state-corruption.md](runbooks/state-corruption.md) — Database corruption recovery
- [mass-offline.md](runbooks/mass-offline.md) — Mass validator offline response
