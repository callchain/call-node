//! Callchain serialization — RLP, JSON, StorageCodec

mod json;
mod rlp;

pub use json::*;
pub use rlp::*;

use thiserror::Error;

/// Error type for serialization failures
#[derive(Debug, Error)]
pub enum SerializationError {
    #[error("RLP encode failed: {0}")]
    RlpEncode(String),
    #[error("RLP decode failed: {0}")]
    RlpDecode(String),
    #[error("JSON encode failed: {0}")]
    JsonEncode(String),
    #[error("JSON decode failed: {0}")]
    JsonDecode(String),
}

/// StorageCodec trait for binary encoding to/from database storage.
/// Uses RLP for protocol types, with potential for custom codecs.
pub trait StorageCodec: Sized {
    /// Encode to a byte buffer
    fn encode_to_buf(&self) -> Vec<u8>;

    /// Decode from a byte buffer
    fn decode_from_buf(buf: &[u8]) -> Result<Self, SerializationError>;
}

// Blanket impl for RLP-encodable types
impl<T: alloy_rlp::Encodable + alloy_rlp::Decodable> StorageCodec for T {
    fn encode_to_buf(&self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(self.length());
        self.encode(&mut buf);
        buf
    }

    fn decode_from_buf(buf: &[u8]) -> Result<Self, SerializationError> {
        Self::decode(&mut &buf[..]).map_err(|e| SerializationError::RlpDecode(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::{Deserialize, Serialize};

    #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
    struct TestStruct {
        name: String,
        value: u64,
    }

    #[test]
    fn test_address_rlp_roundtrip() {
        use call_primitives::Address;
        let addr = Address::repeat_byte(0xAB);
        let encoded = addr.encode_to_buf();
        let decoded = Address::decode_from_buf(&encoded).expect("decode");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_address_json_hex_roundtrip() {
        use call_primitives::Address;
        let addr = Address::repeat_byte(0xAB);
        let json = to_json_string(&addr).expect("serialize");
        let decoded: Address = from_json_str(&json).expect("deserialize");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_storage_codec_roundtrip() {
        use call_primitives::U256;
        let value = U256::from(12345u64);
        let encoded = value.encode_to_buf();
        let decoded = U256::decode_from_buf(&encoded).expect("decode");
        assert_eq!(value, decoded);
    }

    #[test]
    fn test_json_serialize_struct() {
        let test = TestStruct {
            name: "test".into(),
            value: 42,
        };
        let json = to_json_string(&test).expect("serialize");
        let decoded: TestStruct = from_json_str(&json).expect("deserialize");
        assert_eq!(test, decoded);
    }
}
