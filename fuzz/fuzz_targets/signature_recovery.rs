#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    // Fuzz signature recovery with random bytes.
    if data.len() < 97 {
        return;
    }

    let sig: [u8; 65] = data[..65].try_into().unwrap();
    let msg_hash: [u8; 32] = data[65..97].try_into().unwrap();

    // Attempt recovery — should not panic regardless of input
    let _ = call_crypto::recover_secp256k1_signer(&msg_hash, &sig);
});
