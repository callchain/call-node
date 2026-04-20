# Powers of Tau Ceremony for Callchain Shielded Pool

## Problem

The shielded pool uses Groth16 (zk-SNARKs), which requires a **Common Reference String (CRS)** generated from secret randomness ("toxic waste"). If anyone knows the toxic waste, they can forge proofs — minting shielded tokens they don't own, double-spending, etc.

The current code uses `Groth16::circuit_specific_setup()` which generates this locally. **This is only safe for development/testing.**

## Solution

A **Powers of Tau** multi-party ceremony where many participants each contribute secret randomness and destroy it. The final CRS is secure as long as **at least one participant** is honest.

## Quick Start

```bash
cd pot_ceremony

# Option A: Download the Perpetual Powers of Tau (RECOMMENDED — 5 minutes)
./download_pot.sh 25    # 2^25 = 33M constraints, ~2GB

# Option B: Run your own ceremony (for organizations that want their own)
./run_ceremony.sh 100 25  # 100 participants, 2^25 constraints
```

Then derive circuit-specific keys (no trust assumptions — anyone can run this):

```bash
# First, export R1CS from arkworks circuits
cargo run --release --features real-prover --bin export_r1cs

# Then derive Phase 2 keys
./phase2_derive.sh ./pot_transcript/pot_final.ptau
```

## Directory Structure

```
pot_ceremony/
├── README.md                  # This file
├── download_pot.sh            # Option A: Download Perpetual PoT
├── run_ceremony.sh            # Option B: Run your own ceremony
├── phase2_derive.sh           # Derive circuit-specific keys (always needed)
├── r1cs/                      # (generated) R1CS constraint files
│   ├── deposit.r1cs
│   ├── transfer.r1cs
│   └── withdraw.r1cs
├── pot_transcript/            # (generated) Universal CRS
│   └── pot_final.ptau
└── circuit_keys/              # (generated) Circuit-specific keys
    ├── deposit_vk.json
    ├── deposit_final.zkey
    ├── Verifier_deposit.sol
    ├── transfer_vk.json
    ├── transfer_final.zkey
    ├── Verifier_transfer.sol
    ├── withdraw_vk.json
    ├── withdraw_final.zkey
    └── Verifier_withdraw.sol
```

## Code Location

The actual Rust code lives in the `crates/shielded/` crate:

- **`crates/shielded/src/ceremony.rs`** — Production key loading (`ProductionKeys`, `GenesisKeyHashes`, hash verification)
- **`crates/shielded/src/bin/export_r1cs.rs`** — Binary to export R1CS files from arkworks circuits
- **`crates/shielded/src/prover.rs`** — `RealProver::from_production_keys()` and related constructors

## Option A: Download Perpetual Powers of Tau (Recommended)

The [Perpetual Powers of Tau](https://github.com/privacy-scaling-explorations/perpetualpowersoftau) ceremony has 1000+ participants covering up to 2^28 constraints. This is the same CRS used by zkSync, Aztec, and many other production systems.

```bash
# Download 2^25 (~2GB) — sufficient for all Callchain circuits
./download_pot.sh 25
```

| POT Size | Constraints | File Size | Use Case |
|----------|-------------|-----------|----------|
| 2^22 | 4M | ~256MB | Quick testing |
| 2^25 | 33M | ~2GB | **Recommended for Callchain** |
| 2^28 | 268M | ~128GB | Maximum headroom |

Callchain circuit sizes:
- Deposit: ~350 constraints
- Withdraw: ~3,551 constraints
- Transfer: ~7,800 constraints

## Option B: Run Your Own Ceremony

```bash
# Run 100-participant ceremony with 2^25 max constraints
./run_ceremony.sh 100 25
```

In production, each participant should:
1. Run their contribution on their own machine
2. Securely delete their entropy (`rm -f /dev/shm/*`, memory wipe)
3. Pass the `.ptau` file to the next participant
4. Never keep a copy of the intermediate file

The final step applies a **random beacon** (e.g., a future Bitcoin block hash) to prevent the last participant from manipulating the outcome.

## Phase 2: Circuit-Specific Keys

After obtaining the universal CRS, derive keys for each circuit:

```bash
# 1. Export R1CS from arkworks
cargo run --release --features real-prover --bin export_r1cs

# 2. Derive keys
./phase2_derive.sh ./pot_transcript/pot_final.ptau
```

This generates:
- `*_final.zkey` — Proving + verifying key pair (use on clients to generate proofs)
- `*_vk.json` — Verifying key only (embed in nodes for verification)
- `Verifier_*.sol` — Solidity verifier contracts (for EVM on-chain verification)

## Integrating with call-shielded

### 1. Copy verifying keys to the shielded crate

```bash
mkdir -p crates/shielded/keys
cp circuit_keys/transfer_vk.bin crates/shielded/keys/
cp circuit_keys/deposit_vk.bin crates/shielded/keys/
cp circuit_keys/withdraw_vk.bin crates/shielded/keys/
```

### 2. Embed VK hashes in genesis config

```rust
// In crates/chainspec/src/genesis.rs
pub struct ShieldedGenesis {
    pub transfer_vk_hash: "abc123...", // SHA-256 of transfer_vk.bin
    pub deposit_vk_hash:  "def456...",
    pub withdraw_vk_hash: "789abc...",
}
```

### 3. Load keys at node startup

```rust
// In crates/shielded/src/prover.rs
impl RealProver {
    /// Production constructor — loads ceremony-derived verifying keys.
    pub fn from_production(
        keys_dir: &Path,
        genesis_hashes: &GenesisKeyHashes,
    ) -> Result<Self, KeyLoadError> {
        let keys = ProductionKeys::load_with_verification(keys_dir, genesis_hashes)?;
        Ok(Self {
            verifying_keys: keys.vk,
            proving_keys: keys.pk,
        })
    }
}
```

### 4. Feature flag for dev vs prod

```toml
# crates/shielded/Cargo.toml
[features]
default = []
real-prover = []          # Enables arkworks + Groth16
production-keys = []      # Use ceremony-derived keys (not circuit_specific_setup)
```

```rust
#[cfg(not(feature = "production-keys"))]
fn setup_keys() -> Keys {
    // Dev: generate keys on-the-fly (unsafe, but fine for testing)
    let (pk, vk) = Groth16::circuit_specific_setup(&circuit, &mut rng).unwrap();
    Keys { pk, vk }
}

#[cfg(feature = "production-keys")]
fn setup_keys() -> Keys {
    // Prod: load from ceremony output
    ProductionKeys::load("/var/lib/callchain/shielded_keys").unwrap()
}
```

## Verifying the Ceremony

After downloading or running a ceremony, verify the transcript:

```bash
snarkjs powersoftau verify pot_transcript/pot_final.ptau
```

This checks:
- All participant contributions are valid
- The beacon was applied correctly
- The transcript is internally consistent

After Phase 2, verify each circuit key:

```bash
snarkjs zkey verify circuit_keys/transfer.r1cs pot_transcript/pot_final.ptau circuit_keys/transfer_final.zkey
```

## Timeline Estimate

| Step | Effort | Notes |
|------|--------|-------|
| Download Perpetual PoT | 5 min | 2GB download |
| Export R1CS from arkworks | 1-2 days | Wire up circuit builders to `export_r1cs.rs` |
| Phase 2 key derivation | 10 min | Run locally |
| Copy VKs + embed hashes | 2-4 hours | Integrate ceremony keys into shielded crate |
| Test with production keys | 1 day | Verify proofs against ceremony-derived VKs |
| **Total** | **~2-3 days** | Assuming R1CS export is straightforward |

---

## Production Readiness Checklist

Before mainnet deployment, complete these steps to switch from dev CRS to production ceremony-derived keys.

### Step 1: Prerequisites

```bash
# Install snarkjs (requires Node.js)
npm install -g snarkjs

# Ensure Rust toolchain is up to date
rustup update
```

### Step 2: Download Perpetual Powers of Tau

```bash
cd PoT_ceremony
./download_pot.sh 25   # ~2GB download, 2^25 = 33M constraints
```

### Step 3: Export R1CS from arkworks circuits

```bash
cd ..
cargo run --release --features real-prover --bin export_r1cs
# Outputs: r1cs/deposit.r1cs, r1cs/withdraw.r1cs, r1cs/transfer.r1cs
```

### Step 4: Derive circuit-specific keys

```bash
cd PoT_ceremony
./phase2_derive.sh ./pot_transcript/pot_final.ptau
# Outputs: circuit_keys/*_final.zkey, circuit_keys/*_vk.json, circuit_keys/Verifier_*.sol
```

### Step 5: Copy verifying keys to the node

```bash
mkdir -p /var/lib/callchain/shielded_keys
cp circuit_keys/deposit_vk.bin /var/lib/callchain/shielded_keys/
cp circuit_keys/withdraw_vk.bin /var/lib/callchain/shielded_keys/
cp circuit_keys/transfer_vk.bin /var/lib/callchain/shielded_keys/

# Optional: copy proving keys for client-side proof generation
cp circuit_keys/deposit_pk.bin /var/lib/callchain/shielded_keys/
cp circuit_keys/withdraw_pk.bin /var/lib/callchain/shielded_keys/
cp circuit_keys/transfer_pk.bin /var/lib/callchain/shielded_keys/
```

### Step 6: Embed VK hashes in genesis config

Add to `chainspec/mainnet.json`:

```json
{
  "shielded": {
    "deposit_vk_hash":  "<sha256 of deposit_vk.bin>",
    "withdraw_vk_hash": "<sha256 of withdraw_vk.bin>",
    "transfer_vk_hash": "<sha256 of transfer_vk.bin>"
  }
}
```

Compute hashes:
```bash
sha256sum /var/lib/callchain/shielded_keys/*_vk.bin
```

### Step 7: Build with production keys

```bash
# Production build — uses ceremony-derived keys instead of circuit_specific_setup
cargo build --release --features production-keys -p call-node
```

### Step 8: Verify

```bash
# Run a testnet node and check logs for "shielded prover ready"
./target/release/calld --config /etc/callchain/config.toml
```

Expected log output:
```
INFO initializing shielded prover
INFO shielded prover ready
```

If production keys are missing, `RealProver::global()` falls back to dev setup with a warning:
```
WARNING: production ZK keys not found at /var/lib/callchain/shielded_keys
```

---

## References

- [Perpetual Powers of Tau](https://github.com/privacy-scaling-explorations/perpetualpowersoftau)
- [snarkjs](https://github.com/iden3/snarkjs)
- [Zcash Sapling MPC Ceremony](https://z.cash/technology/zcash-parameters/)
- [Groth16 Paper](https://eprint.iacr.org/2016/260)
- [ark-groth16 docs](https://docs.rs/ark-groth16/latest/ark_groth16/)
