//! Bridge deposit: Protocol balance → EVM balance (per spec §5.2)
//!
//! Internal deposit logic has been moved to EVM-based execution.
//! All bridge state (limits, pauses, pending deposits) lives in EVM storage.
