//! RLP encoding/decode helpers

use crate::SerializationError;
use alloy_rlp::{Decodable, Encodable};

/// RLP-encode any encodable type to bytes
pub fn rlp_encode<T: Encodable>(value: &T) -> Vec<u8> {
    let mut buf = Vec::with_capacity(value.length());
    value.encode(&mut buf);
    buf
}

/// RLP-decode bytes into type T
pub fn rlp_decode<T: Decodable>(buf: &[u8]) -> Result<T, SerializationError> {
    T::decode(&mut &buf[..])
        .map_err(|e| SerializationError::RlpDecode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;

    #[test]
    fn test_rlp_roundtrip() {
        let addr = Address::repeat_byte(0xCD);
        let encoded = rlp_encode(&addr);
        let decoded: Address = rlp_decode(&encoded).expect("decode");
        assert_eq!(addr, decoded);
    }
}
