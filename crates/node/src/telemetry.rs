//! T15.1 — Telemetry Module (per spec §20)
//!
//! Prometheus metrics: consensus, mempool, bridge, P2P, performance, system.
//! `/metrics` endpoint on `:9090`. Alert rules for operational monitoring.

pub mod registry;
pub mod alert;
pub mod dispatcher;
pub mod server;
pub mod otel;
#[cfg(test)]
pub mod tests;

pub use registry::*;
pub use alert::*;
pub use dispatcher::*;
pub use server::*;
pub use otel::*;
