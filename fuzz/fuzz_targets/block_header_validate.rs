#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz block header deserialization and basic validation.
    if let Ok(header) = bincode::deserialize::<call_consensus::BlockHeader>(data) {
        // Basic invariants that must never panic
        let _ = header.hash();
        let _ = header.height;
        let _ = header.timestamp_millis;
    }
});
