//! T7.1 — P2P Network Layer (per spec §8)
//!
//! Message types, network traits, and state sync interfaces.

pub mod message;
pub mod event;
pub mod trait_;
pub mod wire;
pub mod config;
pub mod commonware;
pub mod memory;

#[cfg(test)]
pub mod tests;

// Re-export commonly-used items for backwards compatibility
pub use message::*;
pub use event::NetworkEvent;
pub use trait_::Network;
pub use config::{CommonwareConfig, load_or_generate_identity_key};
pub use commonware::CommonwareNetwork;
pub use memory::InMemoryNetwork;
