//! Per-IP request rate limiter for RPC connections.

use dashmap::DashMap;
use std::net::IpAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::time::interval;

/// Simple sliding-window rate limiter keyed by client IP.
pub struct RateLimiter {
    limits: DashMap<IpAddr, (u64, Instant)>,
    max_requests: u64,
    window: Duration,
}

impl RateLimiter {
    /// Create a new rate limiter and spawn a background cleanup task.
    pub fn new(max_requests: u64, window_secs: u64) -> Arc<Self> {
        let limiter = Arc::new(Self {
            limits: DashMap::new(),
            max_requests,
            window: Duration::from_secs(window_secs),
        });

        // Background task: purge stale entries every window interval.
        let cleanup = Arc::clone(&limiter);
        tokio::spawn(async move {
            let mut ticker = interval(cleanup.window);
            loop {
                ticker.tick().await;
                let cutoff = Instant::now() - cleanup.window * 2;
                cleanup.limits.retain(|_, (_, ts)| *ts > cutoff);
            }
        });

        limiter
    }

    /// Check whether a request from `addr` is allowed.
    /// Returns `true` if within limit, `false` if exceeded.
    pub fn check(&self, addr: IpAddr) -> bool {
        let now = Instant::now();

        match self.limits.entry(addr) {
            dashmap::Entry::Occupied(mut e) => {
                let (count, start) = e.get_mut();
                if now.duration_since(*start) > self.window {
                    // Window expired — reset.
                    *count = 1;
                    *start = now;
                    true
                } else {
                    *count += 1;
                    *count <= self.max_requests
                }
            }
            dashmap::Entry::Vacant(e) => {
                e.insert((1, now));
                true
            }
        }
    }
}
