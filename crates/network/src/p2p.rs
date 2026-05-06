//! T7.1 — P2P Network Layer (per spec §8)
//!
//! Message types, network traits, and state sync interfaces.

pub mod commonware;
pub mod config;
pub mod event;
pub mod memory;
pub mod message;
pub mod trait_;
pub mod wire;

#[cfg(test)]
pub mod tests;

// Re-export commonly-used items for backwards compatibility
pub use commonware::CommonwareNetwork;
pub use config::{load_or_generate_identity_key, CommonwareConfig};
pub use event::NetworkEvent;
pub use memory::InMemoryNetwork;
pub use message::*;
pub use trait_::Network;
