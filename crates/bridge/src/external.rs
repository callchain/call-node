//! External bridge: cross-chain deposit/withdraw with validator signatures (per spec §5.6)
//!
//! - ExternalChain enum: EthereumMainnet, Arbitrum
//! - ExternalBridgeOp enum with full fields
//! - verify_bridge_signatures: 14+ validator secp256k1 signatures
//! - sign_bridge_event: validator signing service
//! - BridgeConfig limits enforcement

pub(crate) mod deposit;
pub mod types;
pub(crate) mod withdraw;

pub use deposit::*;
pub use types::*;
pub use withdraw::*;
