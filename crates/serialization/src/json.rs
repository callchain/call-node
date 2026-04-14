//! JSON encoding/decode helpers

use crate::SerializationError;
use serde::{Deserialize, Serialize};

/// Serialize a value to JSON string
pub fn to_json_string<T: Serialize>(value: &T) -> Result<String, SerializationError> {
    serde_json::to_string(value)
        .map_err(|e| SerializationError::JsonEncode(e.to_string()))
}

/// Deserialize a value from a JSON string
pub fn from_json_str<'a, T: Deserialize<'a>>(s: &'a str) -> Result<T, SerializationError> {
    serde_json::from_str(s)
        .map_err(|e| SerializationError::JsonDecode(e.to_string()))
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
}
