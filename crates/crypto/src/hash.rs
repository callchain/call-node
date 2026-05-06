//! Hash functions: keccak256, sha256, Merkle tree

use call_primitives::Hash;
use sha2::Sha256;
use sha3::{Digest, Keccak256};

/// Compute Keccak-256 hash
pub fn keccak256(data: &[u8]) -> Hash {
    let mut hasher = Keccak256::new();
    hasher.update(data);
    Hash::from_slice(&hasher.finalize())
}

/// Compute SHA-256 hash
pub fn sha256(data: &[u8]) -> Hash {
    let mut hasher = Sha256::new();
    hasher.update(data);
    Hash::from_slice(&hasher.finalize())
}

/// Compute Merkle root from a list of leaf hashes.
/// Returns the first leaf if only one, or None if empty.
///
/// Algorithm: repeatedly pair adjacent leaves, hash pairs,
/// until a single root remains.
pub fn build_merkle_root(leaves: &[Hash]) -> Option<Hash> {
    if leaves.is_empty() {
        return None;
    }
    if leaves.len() == 1 {
        return Some(leaves[0]);
    }

    let mut current_level: Vec<Hash> = leaves.to_vec();
    while current_level.len() > 1 {
        let mut next_level = Vec::with_capacity(current_level.len().div_ceil(2));
        for chunk in current_level.chunks(2) {
            let combined = match chunk {
                [a, b] => {
                    let mut data = Vec::with_capacity(64);
                    data.extend_from_slice(a.as_slice());
                    data.extend_from_slice(b.as_slice());
                    keccak256(&data)
                }
                [a] => {
                    // Odd leaf: pair with itself (standard Merkle tree behavior)
                    let mut data = Vec::with_capacity(64);
                    data.extend_from_slice(a.as_slice());
                    data.extend_from_slice(a.as_slice());
                    keccak256(&data)
                }
                _ => unreachable!(),
            };
            next_level.push(combined);
        }
        current_level = next_level;
    }
    Some(current_level[0])
}

/// Verify a Merkle proof: given a leaf, proof path (siblings), and expected root.
///
/// `proof_path` is a list of (sibling_hash, is_left_sibling) pairs from leaf to root.
pub fn verify_merkle_proof(
    mut leaf: Hash,
    proof_path: &[(Hash, bool)],
    expected_root: Hash,
) -> bool {
    for (sibling, is_left) in proof_path {
        let mut hasher = Keccak256::new();
        if *is_left {
            hasher.update(sibling.as_slice());
            hasher.update(leaf.as_slice());
        } else {
            hasher.update(leaf.as_slice());
            hasher.update(sibling.as_slice());
        }
        leaf = Hash::from_slice(&hasher.finalize());
    }
    leaf == expected_root
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_keccak256_known_vectors() {
        // keccak256("") = c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470
        let empty = keccak256(b"");
        assert_eq!(
            hex::encode(&empty[..]),
            "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"
        );

        // keccak256("hello") = 1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8
        let hello = keccak256(b"hello");
        assert_eq!(
            hex::encode(&hello[..]),
            "1c8aff950685c2ed4bc3174f3472287b56d9517b9c948127319a09a7a36deac8"
        );
    }

    #[test]
    fn test_sha256_known_vectors() {
        // sha256("abc") = ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad
        let abc = sha256(b"abc");
        assert_eq!(
            hex::encode(&abc[..]),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn test_merkle_root_single_leaf() {
        let leaf = keccak256(b"leaf");
        assert_eq!(build_merkle_root(&[leaf]), Some(leaf));
    }

    #[test]
    fn test_merkle_root_multiple_leaves() {
        let a = keccak256(b"a");
        let b = keccak256(b"b");
        let c = keccak256(b"c");

        let root = build_merkle_root(&[a, b, c]).expect("root");
        assert_eq!(root.as_slice().len(), 32);

        // Root should be deterministic
        let root2 = build_merkle_root(&[a, b, c]).expect("root");
        assert_eq!(root, root2);
    }

    #[test]
    fn test_merkle_root_empty() {
        assert_eq!(build_merkle_root(&[]), None);
    }

    #[test]
    fn test_merkle_proof_verification() {
        let a = keccak256(b"a");
        let b = keccak256(b"b");
        let root = build_merkle_root(&[a, b]).expect("root");

        // Proof for 'a': sibling is 'b', is_left=false
        let proof = vec![(b, false)];
        assert!(verify_merkle_proof(a, &proof, root));

        // Proof for 'b': sibling is 'a', is_left=true
        let proof = vec![(a, true)];
        assert!(verify_merkle_proof(b, &proof, root));

        // Wrong proof should fail
        assert!(!verify_merkle_proof(
            a,
            &[(keccak256(b"wrong"), false)],
            root
        ));
    }
}
