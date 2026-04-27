//! Oracle constants and helper functions (per spec §25.5)

/// Blocks between oracle price updates
pub const ORACLE_UPDATE_INTERVAL: u64 = 1000;
/// Oracle period in seconds (used for submission validation)
pub const ORACLE_PERIOD_SECS: u64 = 240;
/// Outlier threshold: 5% deviation from median
pub const ORACLE_OUTLIER_THRESHOLD_BPS: u64 = 500;
/// Strikes before validator is disabled
pub const ORACLE_OUTLIER_TOLERANCE: u32 = 10;
/// TWAP window max: 24 hours
pub const ORACLE_TWAP_WINDOW_SECS: u64 = 86_400;
/// Price staleness threshold: 900 seconds (15 min)
pub const ORACLE_STALENESS_SECS: u64 = 900;
/// Minimum independent data sources per validator
pub const ORACLE_MIN_DATA_SOURCES: usize = 2;

/// Compute the oracle quorum from the number of active validators.
/// Returns ceil(2/3 * n), minimum 2, capped at n.
pub fn oracle_quorum(active_count: usize) -> usize {
    if active_count == 0 {
        return 0;
    }
    ((2 * active_count + 2) / 3).min(active_count).max(1)
}
