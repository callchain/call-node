//! T15.1 — Telemetry Module (per spec §20)
//!
//! Prometheus metrics: consensus, mempool, bridge, P2P, performance, system.
//! `/metrics` endpoint on `:9090`. Alert rules for operational monitoring.

pub mod alert;
pub mod dispatcher;
pub mod otel;
pub mod registry;
pub mod server;
#[cfg(test)]
pub mod tests;

pub use alert::*;
pub use dispatcher::*;
pub use otel::*;
pub use registry::*;
pub use server::*;
