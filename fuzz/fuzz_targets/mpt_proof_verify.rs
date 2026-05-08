#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz MPT proof verification with random inputs.
    if data.len() < 32 {
        return;
    }

    let root_hash = alloy_primitives::B256::from_slice(&data[..32]);
    let key_bytes = &data[32..];

    // Build a random proof from remaining data
    let mut proof: Vec<Vec<u8>> = Vec::new();
    let mut offset = 0;
    while offset + 2 <= key_bytes.len() {
        let len = key_bytes[offset] as usize;
        offset += 1;
        if offset + len <= key_bytes.len() {
            proof.push(key_bytes[offset..offset + len].to_vec());
            offset += len;
        } else {
            break;
        }
    }

    let _ = call_light_client::verifier::verify_mpt_proof(root_hash, key_bytes, &proof);
});
