//! call-payload-builder — Block assembly from mempool (per spec §13.5.1)
//!
//! Multi-pool transaction selection with limits enforcement,
//! execution ordering, and state root computation.

pub mod builder;

pub use builder::*;
