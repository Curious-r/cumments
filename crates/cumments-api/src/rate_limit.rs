//! Small in-memory sliding-window rate limiter.
//!
//! Used for anti-spam on open endpoints (site registration and verification
//! issuance). It is not a security boundary: keys come from the client IP,
//! which behind a reverse proxy depends on `X-Forwarded-For` being set
//! correctly.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Upper bound on tracked keys; beyond it, new keys are refused instead of
/// growing memory without limit (keys are client-controlled via XFF).
const MAX_KEYS: usize = 10_000;

/// Best-effort client key: the first `X-Forwarded-For` value when present
/// (set by a trusted reverse proxy), otherwise the peer address.
pub fn client_key(headers: &axum::http::HeaderMap, addr: Option<std::net::SocketAddr>) -> String {
    if let Some(forwarded) = headers
        .get("x-forwarded-for")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|ip| !ip.is_empty())
    {
        return forwarded.to_string();
    }
    addr.map(|addr| addr.ip().to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

pub struct RateLimiter {
    window: Duration,
    max_requests: usize,
    hits: Mutex<HashMap<String, Vec<Instant>>>,
}

impl RateLimiter {
    pub fn new(max_requests: usize, window: Duration) -> Self {
        Self {
            window,
            max_requests,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Whether a request from `key` is within the limit. Records the request
    /// on success only, so denied clients do not extend their own window.
    pub fn allow(&self, key: &str) -> bool {
        let now = Instant::now();
        let mut hits = self.hits.lock().expect("rate limiter mutex poisoned");
        hits.retain(|_, bucket| {
            bucket.retain(|hit| now.duration_since(*hit) < self.window);
            !bucket.is_empty()
        });
        if !hits.contains_key(key) && hits.len() >= MAX_KEYS {
            return false;
        }
        let bucket = hits.entry(key.to_string()).or_default();
        if bucket.len() >= self.max_requests {
            return false;
        }
        bucket.push(now);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sliding_window_limits_and_releases_requests() {
        let limiter = RateLimiter::new(2, Duration::from_millis(50));
        assert!(limiter.allow("a"));
        assert!(limiter.allow("a"));
        assert!(!limiter.allow("a"));
        assert!(limiter.allow("b"));

        std::thread::sleep(Duration::from_millis(60));
        assert!(limiter.allow("a"));
    }

    #[test]
    fn key_cap_refuses_new_keys_instead_of_growing_memory() {
        let limiter = RateLimiter::new(1000, Duration::from_secs(3600));
        for i in 0..MAX_KEYS {
            assert!(limiter.allow(&format!("key-{i}")));
        }
        assert!(!limiter.allow("new-key"));
        assert!(limiter.allow("key-0"), "existing keys keep working");
    }
}
