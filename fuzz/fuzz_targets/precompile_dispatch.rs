#![no_main]

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    use call_precompile::storage::HashMapStorageProvider;
    use call_precompile::StatefulPrecompile;
    use call_primitives::Address;

    let mut provider = HashMapStorageProvider::new(1_000_000);
    let mut precompile = call_validator::ValidatorPrecompile;
    let _ = precompile.call(data, Address::ZERO, &mut provider);
});
