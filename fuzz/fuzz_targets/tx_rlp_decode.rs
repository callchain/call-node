#![no_main]

use libfuzzer_sys::fuzz_target;
use alloy_rlp::Decodable;

fuzz_target!(|data: &[u8]| {
    // Fuzz decoding of ProtocolVersion (uses RlpDecodable)
    let _ = call_primitives::ProtocolVersion::decode(data);
});
