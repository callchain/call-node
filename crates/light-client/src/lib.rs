//! Callchain Light Client (Phase 2: Trustless Bridge)
//!
//! Independently verifies Ethereum block headers and transaction inclusion proofs,
//! enabling cross-chain deposits without relying on validator signatures.
//!
//! ## Architecture
//!
//! ```text
//! Ethereum Header (RLP)
//!     │
//!     ├── Verify parent hash chain against trusted anchor
//!     ├── Verify tx inclusion via MPT proof against transactions_root
//!     ├── Verify receipt via MPT proof against receipts_root
//!     └── Parse bridge event from receipt logs
//! ```
//!
//! ## Security Model
//!
//! The light client starts from a trusted anchor (known-good header) and verifies
//! each subsequent header by checking that its `parent_hash` matches the previous
//! verified header. This ensures the header chain follows the canonical Ethereum
//! chain. Transaction and receipt inclusion is proven via Merkle-Patricia Trie proofs.
//!
//! **Note**: This does NOT verify Ethereum's BLS consensus signatures. The anchor
//! must be a finalized block (e.g., from Ethereum's consensus layer).

mod beacon;
mod ethereum;
mod types;
mod verifier;

pub use beacon::*;
pub use ethereum::proof::{parse_bridge_event_from_logs, parse_receipt_logs, rlp_encode_u64};
pub use ethereum::EthLightClient;
pub use types::*;
pub use verifier::{bytes_to_nibbles, verify_mpt_proof};

#[cfg(feature = "eth-sync")]
pub mod sync;

#[cfg(test)]
mod tests;
