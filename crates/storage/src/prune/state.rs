//! In-memory pruning state tracking.
//!
//! Archive nodes skip all pruning. Full/Validator/Light nodes track
//! execution traces, receipts, block bodies, and snapshots by height,
//! removing entries that fall beyond their respective retention windows.

use crate::prune::config::StateSnapshot;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};

/// Tracks block metadata and pruning state for the layered retention policy.
///
/// Archive nodes skip all pruning. Full/Validator/Light nodes track
/// execution traces, receipts, block bodies, and snapshots by height,
/// removing entries that fall beyond their respective retention windows.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct PruneState {
    /// Execution traces keyed by block height
    execution_traces: BTreeMap<u64, Vec<ExecutionTrace>>,
    /// Receipts keyed by block height
    receipts: BTreeMap<u64, Vec<ReceiptEntry>>,
    /// Block bodies keyed by block height
    block_bodies: BTreeMap<u64, BlockBody>,
    /// Stored snapshots sorted by height (oldest first)
    snapshots: VecDeque<StateSnapshot>,
    /// Whether a compaction has been requested
    compaction_pending: bool,
    /// Total number of traces pruned (for telemetry)
    pub traces_pruned: u64,
    /// Total number of receipts pruned (for telemetry)
    pub receipts_pruned: u64,
    /// Total number of block bodies pruned (for telemetry)
    pub bodies_pruned: u64,
    /// Total number of snapshots pruned (for telemetry)
    pub snapshots_pruned: u64,
    /// Last height that was compacted
    last_compact_height: u64,
}

/// Minimal execution trace entry for pruning tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExecutionTrace {
    pub tx_index: u32,
    pub gas_used: u64,
    pub success: bool,
}

/// Minimal receipt entry for pruning tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReceiptEntry {
    pub tx_index: u32,
    pub status: u8,
    pub gas_used: u64,
}

/// Minimal block body for pruning tracking.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BlockBody {
    pub block_hash: call_primitives::Hash,
    pub tx_count: u32,
    pub body_size: u64,
}

impl PruneState {
    /// Create a new empty prune state tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Record an execution trace for a block.
    pub fn add_execution_trace(&mut self, height: u64, trace: ExecutionTrace) {
        self.execution_traces.entry(height).or_default().push(trace);
    }

    /// Record a receipt for a block.
    pub fn add_receipt(&mut self, height: u64, receipt: ReceiptEntry) {
        self.receipts.entry(height).or_default().push(receipt);
    }

    /// Record a block body.
    pub fn add_block_body(&mut self, height: u64, body: BlockBody) {
        self.block_bodies.insert(height, body);
    }

    /// Record a state snapshot.
    pub fn add_snapshot(&mut self, snapshot: StateSnapshot) {
        // Keep snapshots sorted by height
        if let Some(pos) = self
            .snapshots
            .iter()
            .position(|s| s.height > snapshot.height)
        {
            self.snapshots.insert(pos, snapshot);
        } else {
            self.snapshots.push_back(snapshot);
        }
    }

    /// Get the current count of tracked execution traces.
    pub fn trace_count(&self) -> usize {
        self.execution_traces.values().map(|v| v.len()).sum()
    }

    /// Get the current count of tracked receipts.
    pub fn receipt_count(&self) -> usize {
        self.receipts.values().map(|v| v.len()).sum()
    }

    /// Get the current count of tracked block bodies.
    pub fn body_count(&self) -> usize {
        self.block_bodies.len()
    }

    /// Get the current count of tracked snapshots.
    pub fn snapshot_count(&self) -> usize {
        self.snapshots.len()
    }

    /// Mark that database compaction should be run.
    pub fn request_compaction(&mut self, at_height: u64) {
        self.compaction_pending = true;
        self.last_compact_height = at_height;
    }

    /// Check if compaction is pending.
    pub fn needs_compaction(&self) -> bool {
        self.compaction_pending
    }

    /// Clear the compaction pending flag after compaction completes.
    pub fn clear_compaction_pending(&mut self) {
        self.compaction_pending = false;
    }

    /// Remove all entries above the given height (used during emergency rollback).
    pub fn retain_up_to(&mut self, height: u64) {
        self.execution_traces.retain(|h, _| *h <= height);
        self.receipts.retain(|h, _| *h <= height);
        self.block_bodies.retain(|h, _| *h <= height);
        self.snapshots.retain(|s| s.height <= height);
    }

    // ── Crate-internal accessors for pruner.rs ──────────────────────────

    /// Return keys of execution traces older than `boundary`.
    pub(crate) fn execution_trace_keys_older_than(&self, boundary: u64) -> Vec<u64> {
        self.execution_traces
            .keys()
            .filter(|h| **h < boundary)
            .copied()
            .collect()
    }

    /// Remove execution traces at the given heights.
    pub(crate) fn remove_execution_traces(&mut self, heights: &[u64]) {
        for h in heights {
            self.execution_traces.remove(h);
        }
    }

    /// Return keys of receipts older than `boundary`.
    pub(crate) fn receipt_keys_older_than(&self, boundary: u64) -> Vec<u64> {
        self.receipts
            .keys()
            .filter(|h| **h < boundary)
            .copied()
            .collect()
    }

    /// Remove receipts at the given heights.
    pub(crate) fn remove_receipts(&mut self, heights: &[u64]) {
        for h in heights {
            self.receipts.remove(h);
        }
    }

    /// Return keys of block bodies older than `boundary`.
    pub(crate) fn block_body_keys_older_than(&self, boundary: u64) -> Vec<u64> {
        self.block_bodies
            .keys()
            .filter(|h| **h < boundary)
            .copied()
            .collect()
    }

    /// Remove block bodies at the given heights.
    pub(crate) fn remove_block_bodies(&mut self, heights: &[u64]) {
        for h in heights {
            self.block_bodies.remove(h);
        }
    }

    /// Pop the oldest snapshot and return its height.
    pub(crate) fn pop_oldest_snapshot(&mut self) -> Option<u64> {
        self.snapshots.pop_front().map(|s| s.height)
    }

    /// Get the last compacted height.
    pub(crate) fn last_compact_height(&self) -> u64 {
        self.last_compact_height
    }
}
