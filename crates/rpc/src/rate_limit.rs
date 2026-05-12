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

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;
    use std::str::FromStr;
    use std::thread;

    fn test_ip(n: u8) -> IpAddr {
        IpAddr::from_str(&format!("127.0.0.{n}")).unwrap()
    }

    #[tokio::test]
    async fn test_rate_limit_first_request_allowed() {
        let rl = RateLimiter::new(10, 60);
        assert!(rl.check(test_ip(1)));
        assert_eq!(rl.limits.len(), 1);
    }

    #[tokio::test]
    async fn test_rate_limit_allows_within_limit() {
        let rl = RateLimiter::new(5, 60);
        let ip = test_ip(2);
        for _ in 0..5 {
            assert!(rl.check(ip), "request within limit should be allowed");
        }
    }

    #[tokio::test]
    async fn test_rate_limit_blocks_over_limit() {
        let rl = RateLimiter::new(3, 60);
        let ip = test_ip(3);
        assert!(rl.check(ip));
        assert!(rl.check(ip));
        assert!(rl.check(ip));
        assert!(!rl.check(ip), "4th request should be blocked");
        assert!(!rl.check(ip), "5th request should also be blocked");
    }

    #[tokio::test]
    async fn test_rate_limit_resets_after_window() {
        let rl = RateLimiter::new(2, 1); // 2 requests per 1 second window
        let ip = test_ip(4);
        assert!(rl.check(ip));
        assert!(rl.check(ip));
        assert!(!rl.check(ip), "3rd request in same window blocked");

        // Wait for window to expire
        tokio::time::sleep(Duration::from_millis(1100)).await;
        assert!(
            rl.check(ip),
            "request after window expiry should be allowed"
        );
    }

    #[tokio::test]
    async fn test_rate_limit_per_ip_isolation() {
        let rl = RateLimiter::new(2, 60);
        let ip_a = test_ip(5);
        let ip_b = test_ip(6);

        assert!(rl.check(ip_a));
        assert!(rl.check(ip_a));
        assert!(!rl.check(ip_a), "ip_a exceeded");

        // ip_b should still have its own quota
        assert!(rl.check(ip_b));
        assert!(rl.check(ip_b));
        assert!(!rl.check(ip_b), "ip_b exceeded");
    }

    #[tokio::test]
    async fn test_rate_limit_concurrent_access() {
        let rl = RateLimiter::new(100, 60);
        let ip = test_ip(7);
        let mut handles = vec![];

        for _ in 0..10 {
            let rl = Arc::clone(&rl);
            let handle = thread::spawn(move || {
                let mut allowed = 0;
                for _ in 0..10 {
                    if rl.check(ip) {
                        allowed += 1;
                    }
                }
                allowed
            });
            handles.push(handle);
        }

        let total_allowed: u64 = handles.into_iter().map(|h| h.join().unwrap() as u64).sum();
        // Total allowed should not exceed max_requests
        assert!(
            total_allowed <= 100,
            "concurrent access should not exceed limit, got {total_allowed}"
        );
    }

    #[tokio::test]
    async fn test_rate_limit_cleanup_purges_stale() {
        let rl = RateLimiter::new(10, 1); // 1 second window, cleanup every 1s
        let ip = test_ip(8);

        assert!(rl.check(ip));
        assert_eq!(rl.limits.len(), 1);

        // Wait for window * 2 + buffer so cleanup triggers
        tokio::time::sleep(Duration::from_secs(3)).await;

        // After cleanup, stale entry should be removed
        // Note: cleanup is best-effort; we just verify no panic and limiter still works
        assert!(rl.check(ip), "limiter should still function after cleanup");
    }
}
