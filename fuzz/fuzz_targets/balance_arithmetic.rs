#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if data.len() >= 48 {
        let balance = u128::from_le_bytes(data[0..16].try_into().unwrap());
        let amount = u128::from_le_bytes(data[16..32].try_into().unwrap());
        let fee = u128::from_le_bytes(data[32..48].try_into().unwrap());

        // Fuzz checked arithmetic — should never panic
        let _ = balance.checked_sub(amount).and_then(|b| b.checked_sub(fee));
        let _ = balance.checked_add(amount);
        let _ = balance.checked_mul(amount);
    }
});
