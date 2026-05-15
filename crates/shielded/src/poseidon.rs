//! Poseidon hash module for Pasta Pallas base field (Fp).
//!
//! Provides plain Rust Poseidon hashing for off-circuit operations:
//! - Note commitment / nullifier / RCM derivation
//! - Merkle tree hashing
//! - Viewing key derivation
//!
//! Uses `halo2_gadgets::poseidon::primitives::Hash` with `P128Pow5T3` spec
//! (rate=2, width=3, 8 full rounds, 56 partial rounds). This matches the
//! Orchard implementation and ensures plain-hash == circuit-hash equality.
//!
//! For in-circuit hashing, circuits use `halo2_gadgets::poseidon::Pow5Chip`
//! with the same `P128Pow5T3` spec.

use halo2_gadgets::poseidon::primitives::{ConstantLength, Hash, P128Pow5T3};
use pasta_curves::group::ff::PrimeField;
use pasta_curves::Fp;

/// Domain tags for Poseidon hashing.
/// Used to separate different hash purposes so the same inputs produce different outputs.
pub mod domain {
    pub const IVK_FROM_SK: &str = "call/shielded/ivk";
    pub const FVK_FROM_IVK: &str = "fvk_from_ivk";
    pub const NULLIFIER: &str = "nullifier";
    pub const RCM: &str = "rcm";
    pub const COMMITMENT: &str = "commitment";
    pub const MERKLE: &str = "merkle";
}

// ---------------------------------------------------------------------------
// Plain Poseidon hashing over Pasta Fp
// ---------------------------------------------------------------------------

/// Hash 2 Fp elements using Poseidon (most common case).
fn hash_2(a: Fp, b: Fp) -> Fp {
    Hash::<Fp, P128Pow5T3, ConstantLength<2>, 3, 2>::init().hash([a, b])
}

/// Hash Fp field elements using Poseidon over Pasta Pallas.
///
/// Supports 1..=16 elements. Uses `Hash<ConstantLength<N>>` for small
/// inputs (optimal sponge padding) and cascades pairwise for larger inputs.
///
/// # Panics
/// Panics if inputs is empty or has more than 16 elements.
pub fn poseidon_hash(inputs: &[Fp]) -> Fp {
    assert!(
        !inputs.is_empty() && inputs.len() <= 16,
        "Poseidon input count must be 1..=16"
    );

    match inputs.len() {
        1 => Hash::<Fp, P128Pow5T3, ConstantLength<1>, 3, 2>::init().hash([inputs[0]]),
        2 => hash_2(inputs[0], inputs[1]),
        3 => Hash::<Fp, P128Pow5T3, ConstantLength<3>, 3, 2>::init()
            .hash([inputs[0], inputs[1], inputs[2]]),
        4 => Hash::<Fp, P128Pow5T3, ConstantLength<4>, 3, 2>::init()
            .hash([inputs[0], inputs[1], inputs[2], inputs[3]]),
        5 => Hash::<Fp, P128Pow5T3, ConstantLength<5>, 3, 2>::init()
            .hash([inputs[0], inputs[1], inputs[2], inputs[3], inputs[4]]),
        // For 6..=16, cascade pairwise to keep within the rate-2 sponge width.
        _ => {
            let mut state = inputs[0];
            for &next in &inputs[1..] {
                state = hash_2(state, next);
            }
            state
        }
    }
}

/// Convert a 32-byte array to Pallas Fp (little-endian, modular reduction).
pub fn bytes_to_fp(bytes: &[u8; 32]) -> Fp {
    let mut repr = <Fp as PrimeField>::Repr::default();
    repr.as_mut().copy_from_slice(bytes);
    // Try canonical representation first (most inputs are canonical since p ≈ 2^254)
    if let Some(fp) = Fp::from_repr_vartime(repr) {
        return fp;
    }
    // Non-canonical: interpret as little-endian integer and reduce mod p.
    // This path is extremely rare for uniform random 32-byte inputs (prob ~3/4).
    let mut result = Fp::zero();
    let base = Fp::from(256u64);
    for i in (0..32).rev() {
        result *= base;
        result += Fp::from(bytes[i] as u64);
    }
    result
}

/// Convert Pallas Fp to a 32-byte array (little-endian canonical representation).
pub fn fp_to_bytes(fp: &Fp) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes.copy_from_slice(fp.to_repr().as_ref());
    bytes
}

/// Hash raw 32-byte inputs directly.
///
/// Converts each `[u8; 32]` to Fp, hashes with Poseidon, returns `[u8; 32]`.
pub fn poseidon_hash_bytes(inputs: &[[u8; 32]]) -> [u8; 32] {
    let fps: Vec<_> = inputs.iter().map(|b| bytes_to_fp(b)).collect();
    fp_to_bytes(&poseidon_hash(&fps))
}

/// Domain-tagged Poseidon hash.
///
/// Prepends the domain separator (derived from tag string) before hashing.
pub fn poseidon_hash_tagged(tag: &str, inputs: &[Fp]) -> Fp {
    let tag_fp = bytes_to_fp(&tag_to_bytes(tag));
    let mut tagged = Vec::with_capacity(1 + inputs.len());
    tagged.push(tag_fp);
    tagged.extend_from_slice(inputs);
    poseidon_hash(&tagged)
}

/// Hash a pair of 32-byte values using Poseidon over Pasta.
///
/// Converts each `[u8; 32]` to Fp, hashes with Poseidon, returns `[u8; 32]`.
pub fn poseidon_hash_pair(left: &[u8; 32], right: &[u8; 32]) -> [u8; 32] {
    let left_fp = bytes_to_fp(left);
    let right_fp = bytes_to_fp(right);
    fp_to_bytes(&hash_2(left_fp, right_fp))
}

/// Convert a u128 value to a 32-byte Fp-compatible representation (zero-padded LE).
pub fn value_to_fp_bytes(value: u128) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(&value.to_le_bytes());
    bytes
}

/// Multi-input convenience wrapper: hash exactly 2 inputs.
pub fn poseidon_hash_2(a: &Fp, b: &Fp) -> Fp {
    hash_2(*a, *b)
}

/// Multi-input convenience wrapper: hash exactly 3 inputs.
pub fn poseidon_hash_3(a: &Fp, b: &Fp, c: &Fp) -> Fp {
    Hash::<Fp, P128Pow5T3, ConstantLength<3>, 3, 2>::init().hash([*a, *b, *c])
}

/// Multi-input convenience wrapper: hash exactly 5 inputs.
pub fn poseidon_hash_5(inputs: [Fp; 5]) -> Fp {
    poseidon_hash(&inputs)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Convert a domain tag string to a 32-byte array.
fn tag_to_bytes(tag: &str) -> [u8; 32] {
    let mut bytes = [0u8; 32];
    let tag_bytes = tag.as_bytes();
    let len = tag_bytes.len().min(32);
    bytes[..len].copy_from_slice(&tag_bytes[..len]);
    bytes
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_poseidon_hash_deterministic() {
        let a = Fp::from(1u64);
        let b = Fp::from(2u64);
        let h1 = poseidon_hash(&[a, b]);
        let h2 = poseidon_hash(&[a, b]);
        assert_eq!(h1, h2);
    }

    #[test]
    fn test_poseidon_hash_different_inputs() {
        let a = Fp::from(1u64);
        let b = Fp::from(2u64);
        let b2 = Fp::from(3u64);
        let h1 = poseidon_hash(&[a, b]);
        let h2 = poseidon_hash(&[a, b2]);
        assert_ne!(h1, h2);
    }

    #[test]
    fn test_poseidon_hash_domain_separation() {
        let a = Fp::from(1u64);
        let b = Fp::from(2u64);
        let h_nullifier = poseidon_hash_tagged(domain::NULLIFIER, &[a, b]);
        let h_rcm = poseidon_hash_tagged(domain::RCM, &[a, b]);
        assert_ne!(h_nullifier, h_rcm);
    }

    #[test]
    fn test_poseidon_hash_single_input() {
        let a = Fp::from(1u64);
        let h = poseidon_hash(&[a]);
        let bytes = fp_to_bytes(&h);
        assert!(bytes.iter().any(|&b| b != 0));
    }

    #[test]
    fn test_poseidon_hash_five_inputs() {
        let inputs: Vec<_> = (1..=5).map(|i| Fp::from(i as u64)).collect();
        let h = poseidon_hash(&inputs);
        let bytes = fp_to_bytes(&h);
        assert!(bytes.iter().any(|&b| b != 0));
    }

    #[test]
    fn test_poseidon_hash_max_inputs() {
        let inputs: Vec<_> = (1..=16).map(|i| Fp::from(i as u64)).collect();
        let h = poseidon_hash(&inputs);
        let bytes = fp_to_bytes(&h);
        assert!(bytes.iter().any(|&b| b != 0));
    }

    #[test]
    #[should_panic(expected = "Poseidon input count must be 1..=16")]
    fn test_poseidon_hash_zero_inputs_rejected() {
        poseidon_hash(&[]);
    }

    #[test]
    #[should_panic(expected = "Poseidon input count must be 1..=16")]
    fn test_poseidon_hash_too_many_inputs_rejected() {
        let inputs: Vec<_> = (1..=17).map(|i| Fp::from(i as u64)).collect();
        poseidon_hash(&inputs);
    }

    #[test]
    fn test_poseidon_hash_bytes_roundtrip() {
        let a = [1u8; 32];
        let b = [2u8; 32];
        let h = poseidon_hash_bytes(&[a, b]);
        assert!(h.iter().any(|&byte| byte != 0));
        let h2 = poseidon_hash_bytes(&[a, b]);
        assert_eq!(h, h2);
    }

    #[test]
    fn test_poseidon_hash_tagged_all_domains() {
        let a = Fp::from(1u64);
        let domains = [
            domain::FVK_FROM_IVK,
            domain::NULLIFIER,
            domain::RCM,
            domain::COMMITMENT,
            domain::MERKLE,
        ];
        let mut hashes = Vec::new();
        for d in domains {
            hashes.push(poseidon_hash_tagged(d, &[a]));
        }
        for i in 0..hashes.len() {
            for j in (i + 1)..hashes.len() {
                assert_ne!(
                    hashes[i], hashes[j],
                    "domain {} == domain {}",
                    domains[i], domains[j]
                );
            }
        }
    }

    #[test]
    fn test_bytes_to_fp_field_element() {
        let aa = [0xAAu8; 32];
        let bb = [0xBBu8; 32];
        let one_le = {
            let mut b = [0u8; 32];
            b[0] = 1;
            b
        };
        let two_le = {
            let mut b = [0u8; 32];
            b[0] = 2;
            b
        };
        let zero = [0u8; 32];
        let fp_aa = bytes_to_fp(&aa);
        let fp_bb = bytes_to_fp(&bb);
        let fp_one = bytes_to_fp(&one_le);
        let fp_two = bytes_to_fp(&two_le);
        let fp_zero = bytes_to_fp(&zero);
        assert_ne!(
            fp_aa, fp_bb,
            "[0xAA;32] and [0xBB;32] should map to different Fp elements"
        );
        assert_ne!(fp_one, fp_zero, "[0x01,0x00...] should not map to zero");
        assert_ne!(fp_two, fp_zero, "[0x02,0x00...] should not map to zero");
    }

    #[test]
    fn test_fp_roundtrip() {
        let original = Fp::from(12345u64);
        let bytes = fp_to_bytes(&original);
        let recovered = bytes_to_fp(&bytes);
        assert_eq!(original, recovered);
    }

    #[test]
    fn test_poseidon_hash_convenience_wrappers() {
        let a = Fp::from(1u64);
        let b = Fp::from(2u64);
        let c = Fp::from(3u64);

        let h2 = poseidon_hash_2(&a, &b);
        let h2_direct = poseidon_hash(&[a, b]);
        assert_eq!(h2, h2_direct);

        let h3 = poseidon_hash_3(&a, &b, &c);
        let h3_direct = poseidon_hash(&[a, b, c]);
        assert_eq!(h3, h3_direct);

        let h5 = poseidon_hash_5([a, b, c, Fp::from(4u64), Fp::from(5u64)]);
        let h5_direct = poseidon_hash(&[a, b, c, Fp::from(4u64), Fp::from(5u64)]);
        assert_eq!(h5, h5_direct);
    }
}
