//! Callchain precompiles for the EVM layer (per spec §25.4)
//!
//! - Oracle precompile at `0x101`: getPrice, getTWAP, isStale, getOracleStatus
//! - Protocol balance precompile at `0x102`
//! - Bridge precompile at `0x103`

mod oracle;
mod balance;
mod bridge;

pub use oracle::*;
pub use balance::*;
pub use bridge::*;

use alloy_primitives::{address, Address};

/// Register all precompile addresses
pub fn all_precompiles() -> &'static [Address] {
    static PRECOMPILES: [Address; 3] = [
        address!("0000000000000000000000000000000000000101"),
        address!("0000000000000000000000000000000000000102"),
        address!("0000000000000000000000000000000000000103"),
    ];
    &PRECOMPILES
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_precompile_addresses() {
        let precompiles = all_precompiles();
        assert_eq!(precompiles.len(), 3);
    }
}
