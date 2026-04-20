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
| **AWS KMS** | `aws-kms` | Production validators | Key never leaves AWS; IAM-controlled |
| **HashiCorp Vault** | `hashi-vault` | Production validators | Key in Vault transit engine; audit log |

### Configuration

```toml
# config.toml — Local key (devnet only)
[keys]
validator_key = "0x..."

# config.toml — AWS KMS (production)
[keys]
aws_kms_key_id = "alias/callchain-validator-mainnet"

# config.toml — HashiCorp Vault (production)
[keys]
vault_addr = "https://vault.example.com:8200"
vault_token = "hvs.XXXXXXXX"
vault_key_name = "callchain-validator"
```

### Boot-Time Key Loading

The boot sequence (`boot.rs`) loads keys in priority order:

```
1. AWS KMS key_id   → AwsKmsSigner (if aws-kms feature enabled)
2. Vault key_name   → HashiVaultSigner (if hashi-vault feature enabled)
3. Keystore path    → LocalSigner from encrypted keystore
4. Plaintext key    → LocalSigner (warns in production)
```

### Threshold Signing (M-of-N)

**Current status**: Not yet implemented. The `Signer` trait is single-party. Threshold signing is on the roadmap.

**Recommended approach** for production:
- Use **AWS KMS with key policies** requiring M-of-N IAM role approvals for key deletion
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
| KMS key policies restrict usage | Quarterly | AWS IAM audit |
| Vault token has minimal scope | Quarterly | Vault policy review |
| Key access logs reviewed | Weekly | CloudTrail / Vault audit log |
| Key rotation performed | Annually | Scheduled maintenance window |

---

## Deployment

### Docker Deployment

```bash
# Pull release image
docker pull ghcr.io/callchain/calld:v0.1.0-testnet

# Run with config
docker run -d \
  --name callchain-node \
  -v /etc/callchain:/config \
  -v /var/lib/callchain:/data \
  -p 8545:8545 \
  -p 8546:8546 \
  -p 30303:30303 \
  ghcr.io/callchain/calld:v0.1.0-testnet \
  --config /config/config.toml
```

### Systemd Service

```ini
# /etc/systemd/system/calld.service
[Unit]
Description=Callchain Node
After=network.target

[Service]
Type=simple
User=callchain
Group=callchain
ExecStart=/usr/local/bin/calld --config /etc/callchain/config.toml
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

---

## Compliance Data Sync

### Environment Configuration

```bash
# Optional: URL for sanctioned address list
export CALL_COMPLIANCE_DATA_URL="https://api.example.com/compliance/sanctioned"
```

The node will fetch this URL every 5 minutes and update the `ComplianceEngine` blacklist.

**Expected response format**:
```json
["0x1234...", "0xabcd...", "0xef01..."]
```

---

## Emergency Procedures

See [docs/runbooks/](runbooks/):
- [chain-halt.md](runbooks/chain-halt.md) — Consensus stop diagnosis and recovery
- [state-corruption.md](runbooks/state-corruption.md) — Database corruption recovery
- [mass-offline.md](runbooks/mass-offline.md) — Mass validator offline response
