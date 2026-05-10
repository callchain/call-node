//! Ethereum beacon chain types for Altair light client sync.
//!
//! Manually implements SSZ decoding for the minimal set of types needed to
//! verify sync committee aggregate signatures. SSZ (Simple Serialize) is a
//! fixed-offset serialization format used by the beacon chain consensus layer.
//!
//! # SSZ layout rules used here
//!
//! * `uint64` — 8 bytes little-endian
//! * `BytesN` — N bytes fixed
//! * `Vector[N, T]` — N * sizeof(T) bytes fixed
//! * `Bitvector[N]` — ceil(N/8) bytes, LSB-first bit order
//! * `Container` — fields concatenated in declaration order (all fixed-size)

use alloy_primitives::{keccak256, B256};
use call_crypto::{BlsPublicKey, BlsSignature};

// ── Constants ────────────────────────────────────────────────────────

/// Number of validators in a sync committee.
pub const SYNC_COMMITTEE_SIZE: usize = 512;

/// Domain type for sync committee signatures.
pub const DOMAIN_SYNC_COMMITTEE: [u8; 4] = [0x07, 0x00, 0x00, 0x00];

/// SSZ merkle branch depth for next sync committee inclusion proof.
pub const NEXT_SYNC_COMMITTEE_BRANCH_DEPTH: usize = 5;

/// SSZ merkle branch depth for finalized header inclusion proof.
pub const FINALIZED_BRANCH_DEPTH: usize = 6;

// ── BeaconBlockHeader ────────────────────────────────────────────────

/// Beacon block header (fixed-size SSZ container, 112 bytes).
///
/// ```text
/// slot: uint64          (8)
/// proposer_index: uint64 (8)
/// parent_root: Bytes32   (32)
/// state_root: Bytes32    (32)
/// body_root: Bytes32     (32)
/// ─────────────────────────
/// Total: 112 bytes
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BeaconBlockHeader {
    pub slot: u64,
    pub proposer_index: u64,
    pub parent_root: B256,
    pub state_root: B256,
    pub body_root: B256,
}

impl BeaconBlockHeader {
    pub const SSZ_SIZE: usize = 8 + 8 + 32 + 32 + 32;

    /// Decode from SSZ bytes.
    pub fn from_ssz(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SSZ_SIZE {
            return None;
        }
        Some(Self {
            slot: u64::from_le_bytes(data[0..8].try_into().ok()?),
            proposer_index: u64::from_le_bytes(data[8..16].try_into().ok()?),
            parent_root: B256::from_slice(&data[16..48]),
            state_root: B256::from_slice(&data[48..80]),
            body_root: B256::from_slice(&data[80..112]),
        })
    }

    /// Compute the SSZ hash_tree_root of this header.
    ///
    /// For a fixed-size container, hash_tree_root = hash(merkleize(pack(fields))).
    /// Since all fields fit in one chunk (112 < 128), we pad to 128 bytes and hash.
    pub fn hash_tree_root(&self) -> B256 {
        let mut chunks = vec![0u8; 128];
        chunks[0..8].copy_from_slice(&self.slot.to_le_bytes());
        chunks[8..16].copy_from_slice(&self.proposer_index.to_le_bytes());
        chunks[16..48].copy_from_slice(self.parent_root.as_slice());
        chunks[48..80].copy_from_slice(self.state_root.as_slice());
        chunks[80..112].copy_from_slice(self.body_root.as_slice());
        keccak256(&chunks)
    }
}

// ── SyncCommittee ────────────────────────────────────────────────────

/// A sync committee: 512 BLS pubkeys + aggregate pubkey.
///
/// ```text
/// pubkeys: Vector[512, Bytes48]  (24576)
/// aggregate_pubkey: Bytes48       (48)
/// ───────────────────────────────────
/// Total: 24624 bytes
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncCommittee {
    pub pubkeys: Vec<BlsPublicKey>,
    pub aggregate_pubkey: BlsPublicKey,
}

impl SyncCommittee {
    pub const SSZ_SIZE: usize = SYNC_COMMITTEE_SIZE * 48 + 48;

    /// Decode from SSZ bytes.
    pub fn from_ssz(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SSZ_SIZE {
            return None;
        }
        let mut pubkeys = Vec::with_capacity(SYNC_COMMITTEE_SIZE);
        for i in 0..SYNC_COMMITTEE_SIZE {
            let start = i * 48;
            let mut pk = [0u8; 48];
            pk.copy_from_slice(&data[start..start + 48]);
            pubkeys.push(BlsPublicKey(pk));
        }
        let mut agg = [0u8; 48];
        agg.copy_from_slice(&data[SYNC_COMMITTEE_SIZE * 48..SYNC_COMMITTEE_SIZE * 48 + 48]);
        Some(Self {
            pubkeys,
            aggregate_pubkey: BlsPublicKey(agg),
        })
    }

    /// Get pubkeys of validators whose bit is set in the participation bitmask.
    pub fn participant_pubkeys(&self, bits: &[u8]) -> Vec<BlsPublicKey> {
        let mut result = Vec::new();
        for (byte_idx, byte) in bits.iter().enumerate() {
            for bit_idx in 0..8 {
                let validator_idx = byte_idx * 8 + bit_idx;
                if validator_idx >= SYNC_COMMITTEE_SIZE {
                    break;
                }
                if byte & (1 << bit_idx) != 0 {
                    result.push(self.pubkeys[validator_idx]);
                }
            }
        }
        result
    }
}

// ── SyncAggregate ────────────────────────────────────────────────────

/// Sync committee aggregate signature + participation bitmask.
///
/// ```text
/// sync_committee_bits: Bitvector[512]  (64)
/// sync_committee_signature: Bytes96     (96)
/// ───────────────────────────────────────
/// Total: 160 bytes
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncAggregate {
    pub sync_committee_bits: [u8; 64],
    pub sync_committee_signature: BlsSignature,
}

impl SyncAggregate {
    pub const SSZ_SIZE: usize = 64 + 96;

    /// Decode from SSZ bytes.
    pub fn from_ssz(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SSZ_SIZE {
            return None;
        }
        let mut bits = [0u8; 64];
        bits.copy_from_slice(&data[0..64]);
        let mut sig = [0u8; 96];
        sig.copy_from_slice(&data[64..160]);
        Some(Self {
            sync_committee_bits: bits,
            sync_committee_signature: BlsSignature(sig),
        })
    }

    /// Count number of participating validators.
    pub fn participant_count(&self) -> usize {
        self.sync_committee_bits
            .iter()
            .map(|b| b.count_ones() as usize)
            .sum()
    }
}

// ── LightClientUpdate ────────────────────────────────────────────────

/// Altair light client update.
///
/// Contains everything needed to advance the light client to a newer
/// sync period, including the next sync committee and its BLS aggregate proof.
///
/// ```text
/// attested_header: BeaconBlockHeader                     (112)
/// next_sync_committee: SyncCommittee                     (24624)
/// next_sync_committee_branch: Vector[5, Bytes32]         (160)
/// finalized_header: BeaconBlockHeader                     (112)
/// finality_branch: Vector[6, Bytes32]                    (192)
/// sync_aggregate: SyncAggregate                          (160)
/// signature_slot: uint64                                  (8)
/// ─────────────────────────────────────────────────────────────
/// Total: 25368 bytes (all fixed-size)
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LightClientUpdate {
    pub attested_header: BeaconBlockHeader,
    pub next_sync_committee: SyncCommittee,
    pub next_sync_committee_branch: [B256; NEXT_SYNC_COMMITTEE_BRANCH_DEPTH],
    pub finalized_header: BeaconBlockHeader,
    pub finality_branch: [B256; FINALIZED_BRANCH_DEPTH],
    pub sync_aggregate: SyncAggregate,
    pub signature_slot: u64,
}

impl LightClientUpdate {
    pub const SSZ_SIZE: usize = BeaconBlockHeader::SSZ_SIZE
        + SyncCommittee::SSZ_SIZE
        + NEXT_SYNC_COMMITTEE_BRANCH_DEPTH * 32
        + BeaconBlockHeader::SSZ_SIZE
        + FINALIZED_BRANCH_DEPTH * 32
        + SyncAggregate::SSZ_SIZE
        + 8;

    /// Decode from SSZ bytes.
    pub fn from_ssz(data: &[u8]) -> Option<Self> {
        if data.len() < Self::SSZ_SIZE {
            return None;
        }
        let mut cursor = 0;

        let attested_header = BeaconBlockHeader::from_ssz(&data[cursor..])?;
        cursor += BeaconBlockHeader::SSZ_SIZE;

        let next_sync_committee = SyncCommittee::from_ssz(&data[cursor..])?;
        cursor += SyncCommittee::SSZ_SIZE;

        let mut next_sync_committee_branch = [B256::ZERO; NEXT_SYNC_COMMITTEE_BRANCH_DEPTH];
        for i in 0..NEXT_SYNC_COMMITTEE_BRANCH_DEPTH {
            next_sync_committee_branch[i] = B256::from_slice(&data[cursor..cursor + 32]);
            cursor += 32;
        }

        let finalized_header = BeaconBlockHeader::from_ssz(&data[cursor..])?;
        cursor += BeaconBlockHeader::SSZ_SIZE;

        let mut finality_branch = [B256::ZERO; FINALIZED_BRANCH_DEPTH];
        for i in 0..FINALIZED_BRANCH_DEPTH {
            finality_branch[i] = B256::from_slice(&data[cursor..cursor + 32]);
            cursor += 32;
        }

        let sync_aggregate = SyncAggregate::from_ssz(&data[cursor..])?;
        cursor += SyncAggregate::SSZ_SIZE;

        let signature_slot = u64::from_le_bytes(data[cursor..cursor + 8].try_into().ok()?);

        Some(Self {
            attested_header,
            next_sync_committee,
            next_sync_committee_branch,
            finalized_header,
            finality_branch,
            sync_aggregate,
            signature_slot,
        })
    }
}

// ── SigningData ──────────────────────────────────────────────────────

/// SSZ type used to compute the signing root for beacon chain BLS signatures.
///
/// ```text
/// object_root: Bytes32  (32)
/// domain: Bytes32       (32)
/// ───────────────────────
/// Total: 64 bytes
/// ```
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SigningData {
    pub object_root: B256,
    pub domain: [u8; 32],
}

impl SigningData {
    pub const SSZ_SIZE: usize = 32 + 32;

    /// Compute the SSZ hash_tree_root (just keccak256 for this 64-byte struct).
    pub fn hash_tree_root(&self) -> B256 {
        let mut data = [0u8; 64];
        data[0..32].copy_from_slice(self.object_root.as_slice());
        data[32..64].copy_from_slice(&self.domain);
        keccak256(&data)
    }
}

/// Compute the beacon chain domain for sync committee signatures.
///
/// `domain = hash(fork_version || genesis_validators_root)` truncated to 28 bytes,
/// then prefixed with `DOMAIN_SYNC_COMMITTEE`.
pub fn compute_domain_sync_committee(
    fork_version: [u8; 4],
    genesis_validators_root: B256,
) -> [u8; 32] {
    // ForkData: fork_version (4) + genesis_validators_root (32) = 36 bytes
    let mut fork_data = [0u8; 36];
    fork_data[0..4].copy_from_slice(&fork_version);
    fork_data[4..36].copy_from_slice(genesis_validators_root.as_slice());
    let fork_data_root = keccak256(&fork_data);

    let mut domain = [0u8; 32];
    domain[0..4].copy_from_slice(&DOMAIN_SYNC_COMMITTEE);
    domain[4..32].copy_from_slice(&fork_data_root.as_slice()[0..28]);
    domain
}

/// Compute the sync committee signing root.
///
/// This is the message that the sync committee's BLS aggregate signature
/// attests to: `hash_tree_root(SigningData { object_root: header.hash_tree_root(), domain })`.
pub fn compute_sync_committee_signing_root(
    header: &BeaconBlockHeader,
    fork_version: [u8; 4],
    genesis_validators_root: B256,
) -> B256 {
    let domain = compute_domain_sync_committee(fork_version, genesis_validators_root);
    let signing_data = SigningData {
        object_root: header.hash_tree_root(),
        domain,
    };
    signing_data.hash_tree_root()
}

// ── Sync Period Math ─────────────────────────────────────────────────

/// Compute the sync committee period for a given beacon slot.
///
/// Each period is 256 epochs, each epoch is 32 slots.
pub fn sync_period(slot: u64) -> u64 {
    const SLOTS_PER_EPOCH: u64 = 32;
    const EPOCHS_PER_SYNC_COMMITTEE_PERIOD: u64 = 256;
    const SLOTS_PER_SYNC_PERIOD: u64 = SLOTS_PER_EPOCH * EPOCHS_PER_SYNC_COMMITTEE_PERIOD; // 8192
    slot / SLOTS_PER_SYNC_PERIOD
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sync_period_math() {
        assert_eq!(sync_period(0), 0);
        assert_eq!(sync_period(8191), 0);
        assert_eq!(sync_period(8192), 1);
        assert_eq!(sync_period(16383), 1);
        assert_eq!(sync_period(16384), 2);
    }

    #[test]
    fn test_beacon_block_header_roundtrip() {
        let header = BeaconBlockHeader {
            slot: 42,
            proposer_index: 7,
            parent_root: B256::repeat_byte(0x01),
            state_root: B256::repeat_byte(0x02),
            body_root: B256::repeat_byte(0x03),
        };
        let mut ssz = vec![0u8; BeaconBlockHeader::SSZ_SIZE];
        ssz[0..8].copy_from_slice(&header.slot.to_le_bytes());
        ssz[8..16].copy_from_slice(&header.proposer_index.to_le_bytes());
        ssz[16..48].copy_from_slice(header.parent_root.as_slice());
        ssz[48..80].copy_from_slice(header.state_root.as_slice());
        ssz[80..112].copy_from_slice(header.body_root.as_slice());

        let decoded = BeaconBlockHeader::from_ssz(&ssz).unwrap();
        assert_eq!(decoded, header);
    }

    #[test]
    fn test_sync_committee_decode() {
        let mut ssz = vec![0u8; SyncCommittee::SSZ_SIZE];
        // Set pubkey[0] = [1; 48], pubkey[1] = [2; 48], etc.
        for i in 0..SYNC_COMMITTEE_SIZE {
            ssz[i * 48..i * 48 + 48].fill(i as u8);
        }
        ssz[SYNC_COMMITTEE_SIZE * 48..].fill(0xFF);

        let sc = SyncCommittee::from_ssz(&ssz).unwrap();
        assert_eq!(sc.pubkeys.len(), SYNC_COMMITTEE_SIZE);
        assert_eq!(sc.pubkeys[0].0[0], 0);
        assert_eq!(sc.pubkeys[1].0[0], 1);
        assert_eq!(sc.aggregate_pubkey.0[0], 0xFF);
    }

    #[test]
    fn test_sync_aggregate_decode() {
        let mut ssz = vec![0u8; SyncAggregate::SSZ_SIZE];
        ssz[0] = 0b0000_0011; // validators 0 and 1 participated
        ssz[64..].fill(0xAB);

        let agg = SyncAggregate::from_ssz(&ssz).unwrap();
        assert_eq!(agg.participant_count(), 2);
        assert_eq!(agg.sync_committee_signature.0[0], 0xAB);
    }

    #[test]
    fn test_participant_pubkeys() {
        let mut ssz = vec![0u8; SyncCommittee::SSZ_SIZE];
        for i in 0..SYNC_COMMITTEE_SIZE {
            ssz[i * 48..i * 48 + 48].fill(i as u8);
        }
        let sc = SyncCommittee::from_ssz(&ssz).unwrap();

        // Bits: validator 0 and 1 participated
        let mut bits = [0u8; 64];
        bits[0] = 0b0000_0011;
        let pks = sc.participant_pubkeys(&bits);
        assert_eq!(pks.len(), 2);
        assert_eq!(pks[0].0[0], 0);
        assert_eq!(pks[1].0[0], 1);
    }

    #[test]
    fn test_light_client_update_decode_size_check() {
        // Correct size should parse
        let valid = vec![0u8; LightClientUpdate::SSZ_SIZE];
        assert!(LightClientUpdate::from_ssz(&valid).is_some());

        // Too small should fail
        let invalid = vec![0u8; LightClientUpdate::SSZ_SIZE - 1];
        assert!(LightClientUpdate::from_ssz(&invalid).is_none());
    }

    #[test]
    fn test_compute_domain() {
        let fork = [0x01, 0x00, 0x00, 0x00];
        let genesis_root = B256::repeat_byte(0xAA);
        let domain = compute_domain_sync_committee(fork, genesis_root);
        assert_eq!(domain[0..4], DOMAIN_SYNC_COMMITTEE);
        // Remaining 28 bytes should be the first 28 bytes of keccak256(fork || genesis_root)
        let mut fork_data = [0u8; 36];
        fork_data[0..4].copy_from_slice(&fork);
        fork_data[4..36].copy_from_slice(genesis_root.as_slice());
        let expected_root = keccak256(&fork_data);
        assert_eq!(domain[4..32], expected_root.as_slice()[0..28]);
    }
}
