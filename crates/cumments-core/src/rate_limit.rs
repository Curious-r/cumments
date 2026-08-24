//! Small in-memory sliding-window limiter shared by HTTP and Matrix flows.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Upper bound on tracked keys. New keys are denied until old windows expire;
/// this avoids the "clear every sender" failure mode and keeps memory bounded.
pub const DEFAULT_MAX_KEYS: usize = 10_000;

/// A per-key sliding-window rate limiter with a hard bound on tracked keys.
///
/// The window is intentionally simple: bot commands are low-volume, and an
/// explicit count of successful attempts is easier to audit than refill tokens.
/// Time is injectable so expiry can be tested without sleeping.
pub struct SlidingWindowRateLimiter {
    max_requests: usize,
    window: Duration,
    max_keys: usize,
    hits: Mutex<HashMap<String, VecDeque<Instant>>>,
}

impl SlidingWindowRateLimiter {
    pub fn new(max_requests: usize, window: Duration, max_keys: usize) -> Self {
        Self {
            max_requests,
            window,
            max_keys,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Returns whether this attempt is allowed using the current wall clock.
    pub fn allow(&self, key: &str) -> bool {
        self.allow_at(key, Instant::now())
    }

    /// Returns whether this attempt is allowed at `now`. Allowed attempts are
    /// recorded; denied attempts do not extend the caller's window.
    pub fn allow_at(&self, key: &str, now: Instant) -> bool {
        let mut hits = self.hits.lock().expect("sliding-window mutex poisoned");
        hits.retain(|_, bucket| {
            bucket.retain(|hit| now.duration_since(*hit) < self.window);
            !bucket.is_empty()
        });

        if !hits.contains_key(key) && hits.len() >= self.max_keys {
            return false;
        }

        let bucket = hits.entry(key.to_string()).or_default();
        while bucket
            .front()
            .is_some_and(|hit| now.duration_since(*hit) >= self.window)
        {
            bucket.pop_front();
        }
        if bucket.len() >= self.max_requests {
            return false;
        }

        bucket.push_back(now);
        true
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.hits
            .lock()
            .expect("sliding-window mutex poisoned")
            .len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_the_first_attempt_and_enforces_the_window() {
        let limiter = SlidingWindowRateLimiter::new(2, Duration::from_secs(60), 10);
        let start = Instant::now();

        assert!(limiter.allow_at("alice", start));
        assert_eq!(limiter.len(), 1);
        assert!(limiter.allow_at("alice", start + Duration::from_secs(1)));
        assert!(!limiter.allow_at("alice", start + Duration::from_secs(2)));
        assert!(limiter.allow_at("alice", start + Duration::from_secs(61)));
    }

    #[test]
    fn denied_attempts_do_not_extend_a_window() {
        let limiter = SlidingWindowRateLimiter::new(1, Duration::from_secs(60), 10);
        let start = Instant::now();

        assert!(limiter.allow_at("alice", start));
        assert!(!limiter.allow_at("alice", start + Duration::from_millis(1)));
        assert!(limiter.allow_at("alice", start + Duration::from_secs(60)));
    }

    #[test]
    fn full_capacity_denies_new_keys_without_clearing_existing_keys() {
        let limiter = SlidingWindowRateLimiter::new(2, Duration::from_secs(60), DEFAULT_MAX_KEYS);
        let start = Instant::now();
        for index in 0..DEFAULT_MAX_KEYS {
            assert!(
                limiter.allow_at(&format!("sender-{index}"), start),
                "seed key {index}"
            );
        }

        assert!(!limiter.allow_at("new-sender", start));
        assert!(
            limiter.allow_at("sender-0", start + Duration::from_secs(1)),
            "an existing sender must not lose its window when capacity is full"
        );
    }

    #[test]
    fn expired_keys_free_capacity() {
        let limiter = SlidingWindowRateLimiter::new(1, Duration::from_secs(60), DEFAULT_MAX_KEYS);
        let start = Instant::now();
        for index in 0..DEFAULT_MAX_KEYS {
            assert!(limiter.allow_at(&format!("sender-{index}"), start));
        }

        let after_expiry = start + Duration::from_secs(60);
        assert!(limiter.allow_at("new-sender", after_expiry));
        assert_eq!(limiter.len(), 1);
    }
}
