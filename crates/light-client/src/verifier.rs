//! Merkle-Patricia Trie proof verifier for Ethereum.
//!
//! Implements read-only proof verification: given a root hash, a key,
//! and a sequence of RLP-encoded nodes, verify the key maps to a value.
//!
//! Based on the Ethereum specification:
//! https://ethereum.org/en/developers/docs/data-structures-and-encoding/patricia-merkle-trie/

use alloy_primitives::{keccak256, B256};
use alloy_rlp::Header;

/// Merkle-Patricia Trie proof verification error.
#[derive(Debug, thiserror::Error)]
pub enum MptError {
    #[error("invalid RLP encoding")]
    InvalidRlp,
    #[error("proof incomplete: missing nodes")]
    ProofIncomplete,
    #[error("node hash mismatch: expected {expected}, got {actual}")]
    NodeHashMismatch { expected: B256, actual: B256 },
    #[error("invalid node encoding")]
    InvalidNode,
}

/// Encode bytes into nibble array (each byte → two nibbles).
pub fn bytes_to_nibbles(bytes: &[u8]) -> Vec<u8> {
    bytes.iter().flat_map(|b| [b >> 4, b & 0x0F]).collect()
}

/// Compact encoding for MPT keys.
/// `is_leaf`: true for leaf nodes, false for extension nodes.
/// Returns the compact-encoded key as bytes (including the prefix byte).
#[cfg(test)]
pub fn compact_encode(nibbles: &[u8], is_leaf: bool) -> Vec<u8> {
    let has_odd = nibbles.len() % 2 != 0;
    let first_byte = if has_odd {
        // Odd: type in low nibble (1=ext, 3=leaf), first nibble in high nibble
        let type_low = if is_leaf { 3 } else { 1 };
        (nibbles[0] << 4) | type_low
    } else {
        // Even: type in high nibble (0x00=ext, 0x20=leaf), no nibble in prefix
        if is_leaf { 0x20 } else { 0x00 }
    };
    let mut out = Vec::with_capacity(1 + nibbles.len() / 2 + 1);
    out.push(first_byte);

    // Even keys: start packing from nibble 0; odd keys: start from nibble 1
    let start = if has_odd { 1 } else { 0 };
    for i in (start..nibbles.len()).step_by(2) {
        if i + 1 < nibbles.len() {
            let packed = (nibbles[i] << 4) | nibbles[i + 1];
            out.push(packed);
        }
    }
    out
}

/// Decode a compact-encoded key. Returns (is_leaf, nibbles).
fn compact_decode(data: &[u8]) -> Result<(bool, Vec<u8>), MptError> {
    if data.is_empty() {
        return Err(MptError::InvalidNode);
    }
    let first = data[0];
    let high = first >> 4;
    let low = first & 0x0F;

    // Determine type from high and low nibbles:
    // Even ext: high=0, low=0 → type byte 0x00
    // Even leaf: high=2, low=0 → type byte 0x20
    // Odd ext: high=nibble, low=1 → type byte (nibble<<4)|1
    // Odd leaf: high=nibble, low=3 → type byte (nibble<<4)|3
    let is_leaf;
    let has_odd;
    if low == 0 {
        // Even-length key
        has_odd = false;
        is_leaf = match high {
            0 => false,  // even extension
            2 => true,   // even leaf
            _ => return Err(MptError::InvalidNode),
        };
    } else {
        // Odd-length key: type in low nibble (1=ext, 3=leaf)
        has_odd = true;
        is_leaf = match low {
            1 => false,  // odd extension
            3 => true,   // odd leaf
            _ => return Err(MptError::InvalidNode),
        };
    }

    let mut nibbles = Vec::with_capacity(data.len() * 2);
    if has_odd {
        nibbles.push(high);  // first nibble in high nibble
    }
    for &byte in &data[1..] {
        nibbles.push(byte >> 4);
        nibbles.push(byte & 0x0F);
    }
    Ok((is_leaf, nibbles))
}

/// Parse a list of RLP items from raw bytes.
/// Returns the individual items as byte slices (still RLP-encoded).
fn parse_rlp_list(data: &[u8]) -> Result<Vec<&[u8]>, MptError> {
    if data.is_empty() {
        return Err(MptError::InvalidRlp);
    }

    let first_byte = data[0];
    if first_byte < 0xC0 {
        return Err(MptError::InvalidRlp);
    }

    let payload_start: usize = if first_byte < 0xF8 {
        // Short list
        let list_len = (first_byte - 0xC0) as usize;
        if list_len == 0 || 1 + list_len != data.len() {
            return Err(MptError::InvalidRlp);
        }
        1
    } else {
        // Long list
        let len_of_len = (first_byte - 0xF7) as usize;
        if 1 + len_of_len > data.len() {
            return Err(MptError::InvalidRlp);
        }
        let total_payload = usize::from_be_bytes({
            let mut buf = [0u8; 8];
            buf[8 - len_of_len..].copy_from_slice(&data[1..1 + len_of_len]);
            buf
        });
        if 1 + len_of_len + total_payload != data.len() {
            return Err(MptError::InvalidRlp);
        }
        1 + len_of_len
    };

    let payload = &data[payload_start..];
    let mut items = Vec::new();
    let mut offset = 0;

    while offset < payload.len() {
        let (item, consumed) = decode_one_rlp_item(&payload[offset..])?;
        items.push(item);
        offset += consumed;
    }

    Ok(items)
}

/// Decode a single RLP item from the start of the buffer.
/// Returns (item_bytes, total_consumed_bytes).
fn decode_one_rlp_item(data: &[u8]) -> Result<(&[u8], usize), MptError> {
    if data.is_empty() {
        return Err(MptError::InvalidRlp);
    }

    let first = data[0];
    if first < 0x80 {
        // Single byte
        Ok((&data[..1], 1))
    } else if first < 0xB8 {
        // Short string
        let len = (first - 0x80) as usize;
        if 1 + len > data.len() {
            return Err(MptError::InvalidRlp);
        }
        Ok((&data[1..1 + len], 1 + len))
    } else if first < 0xC0 {
        // Long string
        let len_of_len = (first - 0xB7) as usize;
        if 1 + len_of_len > data.len() {
            return Err(MptError::InvalidRlp);
        }
        let len = usize::from_be_bytes({
            let mut buf = [0u8; 8];
            let src = &data[1..1 + len_of_len];
            buf[8 - len_of_len..].copy_from_slice(src);
            buf
        });
        let total = 1 + len_of_len + len;
        if total > data.len() {
            return Err(MptError::InvalidRlp);
        }
        Ok((&data[1 + len_of_len..total], total))
    } else {
        // List — return the entire list as one item
        let _header = Header::decode(&mut &data[..]).map_err(|_| MptError::InvalidRlp)?;
        let total = 1 + if first < 0xF8 {
            (first - 0xC0) as usize
        } else {
            let len_of_len = (first - 0xF7) as usize;
            if 1 + len_of_len > data.len() {
                return Err(MptError::InvalidRlp);
            }
            usize::from_be_bytes({
                let mut buf = [0u8; 8];
                buf[8 - len_of_len..].copy_from_slice(&data[1..1 + len_of_len]);
                buf
            })
        };
        if total > data.len() {
            return Err(MptError::InvalidRlp);
        }
        Ok((&data[..total], total))
    }
}

/// Encode an RLP short string (for values < 56 bytes).
/// Falls back to long string encoding for larger values.
#[cfg(test)]
pub(crate) fn rlp_encode_short_bytes(data: &[u8]) -> Vec<u8> {
    if data.len() == 1 && data[0] < 0x80 {
        data.to_vec()
    } else if data.len() < 56 {
        let mut out = Vec::with_capacity(1 + data.len());
        out.push(0x80 + data.len() as u8);
        out.extend(data);
        out
    } else {
        // Long string encoding
        let len_bytes = data.len().to_be_bytes();
        let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(len_bytes.len());
        let num_len_bytes = len_bytes.len() - skip;
        let mut out = Vec::with_capacity(1 + num_len_bytes + data.len());
        out.push(0xB7 + num_len_bytes as u8);
        out.extend_from_slice(&len_bytes[skip..]);
        out.extend(data);
        out
    }
}

/// Encode an RLP list from items (each item is already RLP-encoded).
#[cfg(test)]
fn rlp_encode_list(items: &[Vec<u8>]) -> Vec<u8> {
    let total_len: usize = items.iter().map(|i| i.len()).sum();
    let mut out = Vec::with_capacity(1 + total_len);
    if total_len < 56 {
        out.push(0xC0 + total_len as u8);
    } else {
        // Long list encoding: 0xF7 + num_length_bytes + length_be_bytes + payload
        let len_bytes = total_len.to_be_bytes();
        let skip = len_bytes.iter().position(|&b| b != 0).unwrap_or(len_bytes.len());
        let num_len_bytes = len_bytes.len() - skip;
        out.push(0xF7 + num_len_bytes as u8);
        out.extend_from_slice(&len_bytes[skip..]);
    }
    for item in items {
        out.extend(item);
    }
    out
}

/// Verify an MPT proof against a root hash.
///
/// - `root_hash`: the expected root of the trie
/// - `key_bytes`: the key to look up (raw bytes, will be converted to nibbles)
/// - `proof`: ordered list of RLP-encoded nodes from root to leaf
///
/// Returns `Some(value_bytes)` if the key exists, `None` if the key doesn't exist.
/// The proof is verified by re-computing node hashes at each step.
pub fn verify_mpt_proof(
    root_hash: B256,
    key_bytes: &[u8],
    proof: &[Vec<u8>],
) -> Result<Option<Vec<u8>>, MptError> {
    if proof.is_empty() {
        return Ok(None);
    }

    let key_nibbles = bytes_to_nibbles(key_bytes);
    let mut expected_hash = root_hash;
    let mut remaining = key_nibbles.as_slice();

    for node_rlp in proof {
        // Verify the node's hash matches expected
        let actual_hash = keccak256(node_rlp);
        if actual_hash != expected_hash {
            return Err(MptError::NodeHashMismatch {
                expected: expected_hash,
                actual: actual_hash,
            });
        }

        let items = parse_rlp_list(node_rlp)?;

        match items.len() {
            2 => {
                // Leaf or extension
                let (is_leaf, node_key) = compact_decode(items[0])?;

                if is_leaf {
                    // Check key match
                    if remaining.starts_with(&node_key) {
                        return Ok(Some(items[1].to_vec()));
                    }
                    return Ok(None);
                } else {
                    // Extension
                    if remaining.starts_with(&node_key) {
                        remaining = &remaining[node_key.len()..];
                        expected_hash = extract_child_ref(items[1])?;
                    } else {
                        return Ok(None);
                    }
                }
            }
            17 => {
                // Branch
                if remaining.is_empty() {
                    // Return the value slot (index 16)
                    return Ok(Some(items[16].to_vec()));
                }
                let nibble = remaining[0] as usize;
                if nibble > 15 {
                    return Err(MptError::InvalidNode);
                }
                remaining = &remaining[1..];

                let child_ref = items[nibble];
                if child_ref.is_empty() || (child_ref.len() == 1 && child_ref[0] == 0x80) {
                    return Ok(None); // empty child
                }
                expected_hash = extract_child_ref(child_ref)?;
            }
            _ => return Err(MptError::InvalidNode),
        }
    }

    // Ran out of proof nodes before finishing key
    Ok(None)
}

/// Extract the hash reference from a child field.
/// If the field is a 32-byte raw hash, return it directly.
/// If it's an RLP-encoded 32-byte string (0xb8, 0x20, ...), extract the raw hash.
/// If it's inline RLP (< 32 bytes), hash it to get the reference.
fn extract_child_ref(data: &[u8]) -> Result<B256, MptError> {
    // Case 1: raw 32-byte hash
    if data.len() == 32 {
        return Ok(B256::from_slice(data));
    }
    // Case 2: RLP-encoded 32-byte string (0xb8, 0x20, <32 bytes>)
    if data.len() == 34 && data[0] == 0xb8 && data[1] == 0x20 {
        return Ok(B256::from_slice(&data[2..34]));
    }
    // Case 3: inline RLP — hash the RLP bytes to get the node reference
    Ok(keccak256(data))
}

/// Helper to build an RLP-encoded leaf node for testing.
#[cfg(test)]
pub(crate) fn make_leaf_node_rlp(key_nibbles: &[u8], value: &[u8]) -> Vec<u8> {
    let compact = compact_encode(key_nibbles, true);
    let compact_rlp = rlp_encode_short_bytes(&compact);
    let value_rlp = rlp_encode_short_bytes(value);
    rlp_encode_list(&[compact_rlp, value_rlp])
}

#[cfg(test)]
pub(crate) fn make_extension_node_rlp(key_nibbles: &[u8], child_hash: B256) -> Vec<u8> {
    let compact = compact_encode(key_nibbles, false);
    let compact_rlp = rlp_encode_short_bytes(&compact);
    let child_rlp = rlp_encode_short_bytes(&child_hash.0);
    rlp_encode_list(&[compact_rlp, child_rlp])
}

/// Helper to build an RLP-encoded branch node for testing.
#[cfg(test)]
pub(crate) fn make_branch_node_rlp(
    children: &[Option<B256>; 16],
    value: Option<&[u8]>,
) -> Vec<u8> {
    let mut items = Vec::with_capacity(17);
    for i in 0..16 {
        if let Some(hash) = children[i] {
            items.push(rlp_encode_short_bytes(&hash.0));
        } else {
            items.push(vec![0x80]); // empty
        }
    }
    if let Some(v) = value {
        items.push(rlp_encode_short_bytes(v));
    } else {
        items.push(vec![0x80]); // empty
    }
    rlp_encode_list(&items)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compact_encode_decode_roundtrip() {
        // Even number of nibbles, leaf
        let nibbles = vec![0, 1, 2, 3];
        let encoded = compact_encode(&nibbles, true);
        let (is_leaf, decoded) = compact_decode(&encoded).unwrap();
        assert!(is_leaf);
        assert_eq!(decoded, nibbles);

        // Odd number of nibbles, leaf
        let nibbles = vec![1, 2, 3];
        let encoded = compact_encode(&nibbles, true);
        let (is_leaf, decoded) = compact_decode(&encoded).unwrap();
        assert!(is_leaf);
        assert_eq!(decoded, nibbles);

        // Even, extension
        let nibbles = vec![0, 0, 1, 2];
        let encoded = compact_encode(&nibbles, false);
        let (is_leaf, decoded) = compact_decode(&encoded).unwrap();
        assert!(!is_leaf);
        assert_eq!(decoded, nibbles);
    }

    #[test]
    fn test_mpt_proof_single_leaf() {
        // Build a trie with just a leaf node
        let key_bytes = [0x12u8, 0x34]; // nibbles: [1, 2, 3, 4]
        let key_nibbles = bytes_to_nibbles(&key_bytes);
        let value = b"hello";

        let leaf_rlp = make_leaf_node_rlp(&key_nibbles, value);
        let root = keccak256(&leaf_rlp);

        let proof = vec![leaf_rlp];
        let result = verify_mpt_proof(root, &key_bytes, &proof).unwrap();
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap(), value);
    }

    #[test]
    fn test_mpt_proof_extension_then_leaf() {
        // Trie: root → extension(0) → leaf(1, 0, 2)
        // Key: [0x01, 0x02] (nibbles: [0, 1, 0, 2])
        // After traversing extension covering nibble [0], remaining = [1, 0, 2]
        let key_bytes = [0x01u8, 0x02];
        let key_nibbles = bytes_to_nibbles(&key_bytes);
        let value = b"found";

        // Leaf stores only the remaining key after extension
        let leaf_key = &key_nibbles[1..]; // [1, 0, 2]
        let leaf_rlp = make_leaf_node_rlp(leaf_key, value);
        let leaf_hash = keccak256(&leaf_rlp);

        // Extension covers nibble [0]
        let ext_rlp = make_extension_node_rlp(&[0], leaf_hash);
        let root = keccak256(&ext_rlp);

        let proof = vec![ext_rlp, leaf_rlp];
        let result = verify_mpt_proof(root, &key_bytes, &proof).unwrap();
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap(), value);
    }

    #[test]
    fn test_mpt_proof_key_not_found() {
        // Trie has key [0x12], look up [0xAB]
        let key_bytes = [0x12u8];
        let key_nibbles = bytes_to_nibbles(&key_bytes);
        let value = b"data";

        let leaf_rlp = make_leaf_node_rlp(&key_nibbles, value);
        let root = keccak256(&leaf_rlp);

        let proof = vec![leaf_rlp];
        let wrong_key = [0xABu8];
        let result = verify_mpt_proof(root, &wrong_key, &proof).unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_mpt_proof_branch() {
        // Branch node with child at nibble 5
        let key_bytes = [0x5Au8]; // nibbles: [5, 10]
        let key_nibbles = bytes_to_nibbles(&key_bytes);
        let value = b"branch_val";

        // Leaf for the child
        let leaf_rlp = make_leaf_node_rlp(&key_nibbles[1..], value); // remaining nibbles after branch
        let leaf_hash = keccak256(&leaf_rlp);

        // Branch with child at nibble 5
        let mut children: [Option<B256>; 16] = [None; 16];
        children[5] = Some(leaf_hash);
        let branch_rlp = make_branch_node_rlp(&children, None);
        let root = keccak256(&branch_rlp);

        let proof = vec![branch_rlp, leaf_rlp];
        let result = verify_mpt_proof(root, &key_bytes, &proof).unwrap();
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap(), value);
    }

    #[test]
    fn test_mpt_proof_branch_value_at_root() {
        // Branch node with value at index 16 (key is empty/fully consumed)
        let value = b"at_branch";
        let children: [Option<B256>; 16] = [None; 16];
        let branch_rlp = make_branch_node_rlp(&children, Some(value));
        let root = keccak256(&branch_rlp);

        let proof = vec![branch_rlp];
        let result = verify_mpt_proof(root, &[], &proof).unwrap();
        assert!(result.is_some());
        assert_eq!(result.as_ref().unwrap(), value);
    }

    #[test]
    fn test_mpt_proof_tampered_hash_fails() {
        let key_bytes = [0x12u8];
        let key_nibbles = bytes_to_nibbles(&key_bytes);
        let value = b"data";

        let leaf_rlp = make_leaf_node_rlp(&key_nibbles, value);

        // Wrong root hash
        let wrong_root = B256::repeat_byte(0xFF);
        let proof = vec![leaf_rlp.clone()];
        let result = verify_mpt_proof(wrong_root, &key_bytes, &proof);
        assert!(matches!(result, Err(MptError::NodeHashMismatch { .. })));
    }
}
