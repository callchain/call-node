//! Pre-compiled ERC-20 bytecode for `WrappedToken.sol`.
//!
//! Generated via: `solc --bin --optimize --optimize-runs 200 WrappedToken.sol`

use alloy_primitives::{Address, Bytes};

/// Hex-encoded init bytecode (constructor + runtime embedded).
const INIT_BYTECODE_HEX: &str = include_str!("../contracts/WrappedToken.bin");

/// Hex-encoded runtime bytecode (deployed contract code).
const RUNTIME_BYTECODE_HEX: &str = include_str!("../contracts/WrappedToken.bin-runtime");

/// Decode the hex-encoded init bytecode at first use.
fn init_bytecode() -> Vec<u8> {
    let hex = INIT_BYTECODE_HEX.trim();
    hex::decode(hex).expect("valid hex init bytecode")
}

/// Decode the hex-encoded runtime bytecode at first use.
pub fn runtime_bytecode() -> Vec<u8> {
    let hex = RUNTIME_BYTECODE_HEX.trim();
    hex::decode(hex).expect("valid hex runtime bytecode")
}

/// ABI-encode constructor args for WrappedToken:
/// `(string name, string symbol, uint8 decimals, address bridge)`
pub fn build_erc20_init_code(
    name: &str,
    symbol: &str,
    decimals: u8,
    bridge: Address,
) -> Bytes {
    let name_len = name.len();
    let symbol_len = symbol.len();

    // Head section: 4 * 32 = 128 bytes
    // [0:32]   offset to name
    // [32:64]  offset to symbol
    // [64:96]  decimals
    // [96:128] bridge address (left-padded to 32 bytes)
    let name_offset: u32 = 128;
    let symbol_offset: u32 = name_offset + 32 + ((name_len as u32 + 31) / 32) * 32;

    let init = init_bytecode();
    let mut encoded = Vec::with_capacity(
        init.len() + 128 + 64 + name_len + symbol_len + 64,
    );

    // Init code
    encoded.extend_from_slice(&init);

    // ABI-encoded constructor args
    let mut u32_buf = [0u8; 32];
    u32_buf[28..32].copy_from_slice(&name_offset.to_be_bytes());
    encoded.extend_from_slice(&u32_buf);
    u32_buf[28..32].copy_from_slice(&symbol_offset.to_be_bytes());
    encoded.extend_from_slice(&u32_buf);
    encoded.extend_from_slice(&[0u8; 31]);
    encoded.push(decimals);
    // bridge address: left-padded to 32 bytes
    let mut bridge_bytes = [0u8; 32];
    bridge_bytes[12..].copy_from_slice(bridge.as_slice());
    encoded.extend_from_slice(&bridge_bytes);

    // Name string
    u32_buf[28..32].copy_from_slice(&(name_len as u32).to_be_bytes());
    encoded.extend_from_slice(&u32_buf);
    encoded.extend_from_slice(name.as_bytes());
    let name_pad = (32 - (name_len % 32)) % 32;
    encoded.extend_from_slice(&vec![0u8; name_pad]);

    // Symbol string
    u32_buf[28..32].copy_from_slice(&(symbol_len as u32).to_be_bytes());
    encoded.extend_from_slice(&u32_buf);
    encoded.extend_from_slice(symbol.as_bytes());
    let symbol_pad = (32 - (symbol_len % 32)) % 32;
    encoded.extend_from_slice(&vec![0u8; symbol_pad]);

    Bytes::from(encoded)
}
