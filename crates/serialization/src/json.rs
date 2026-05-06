//! JSON encoding/decode helpers

use crate::SerializationError;
use serde::{Deserialize, Serialize};

/// Serialize a value to JSON string
pub fn to_json_string<T: Serialize>(value: &T) -> Result<String, SerializationError> {
    serde_json::to_string(value).map_err(|e| SerializationError::JsonEncode(e.to_string()))
}

/// Deserialize a value from a JSON string
pub fn from_json_str<'a, T: Deserialize<'a>>(s: &'a str) -> Result<T, SerializationError> {
    serde_json::from_str(s).map_err(|e| SerializationError::JsonDecode(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use call_primitives::Address;

    #[test]
    fn test_json_address_roundtrip() {
        let addr = Address::repeat_byte(0xEF);
        let json = to_json_string(&addr).expect("serialize");
        let decoded: Address = from_json_str(&json).expect("deserialize");
        assert_eq!(addr, decoded);
    }

    #[test]
    fn test_json_invalid_input_rejected() {
        let result: Result<Address, _> = from_json_str("not-json");
        assert!(result.is_err());
    }

    #[test]
    fn test_json_complex_struct_roundtrip() {
        #[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
        struct Payload {
            sender: Address,
            nonce: u64,
            data: Vec<u8>,
        }
        let payload = Payload {
            sender: Address::repeat_byte(0xAB),
            nonce: 123,
            data: vec![1, 2, 3],
        };
        let json = to_json_string(&payload).expect("serialize");
        let decoded: Payload = from_json_str(&json).expect("deserialize");
        assert_eq!(payload, decoded);
    }
}
