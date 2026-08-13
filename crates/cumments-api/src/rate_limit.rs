//! Small in-memory sliding-window rate limiter.
//!
//! Used for anti-spam on open endpoints (site registration and verification
//! issuance). It is not a security boundary: keys come from the client IP,
//! which behind a reverse proxy depends on `X-Forwarded-For` being set
//! correctly.

use std::collections::HashMap;
use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Upper bound on tracked keys; beyond it, new keys are refused instead of
/// growing memory without limit (keys are client-controlled via XFF).
const MAX_KEYS: usize = 10_000;

/// Best-effort client key.
///
/// `X-Forwarded-For` is honored only when the immediate peer is one of the
/// configured trusted proxies; otherwise the peer address is used so direct
/// clients cannot spoof their rate-limit key. When the peer *is* trusted, the
/// list is walked right-to-left skipping trusted proxy addresses, so a client
/// cannot prepend arbitrary entries: the rightmost untrusted address is the
/// client as seen by the first trusted proxy.
pub fn client_key(
    headers: &axum::http::HeaderMap,
    addr: Option<SocketAddr>,
    trusted_proxies: &HashSet<IpAddr>,
) -> String {
    if addr.is_some_and(|peer| trusted_proxies.contains(&peer.ip())) {
        if let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
        {
            for entry in forwarded.split(',').rev() {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                if entry
                    .parse::<IpAddr>()
                    .is_ok_and(|ip| trusted_proxies.contains(&ip))
                {
                    // This hop is another trusted proxy; keep walking toward
                    // the client.
                    continue;
                }
                return entry.to_string();
            }
        }
        // Every entry was a trusted proxy (or the header was empty): fall
        // back to the peer we received the request from.
        if let Some(addr) = addr {
            return addr.ip().to_string();
        }
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
    use axum::http::HeaderValue;
    use std::net::Ipv4Addr;

    fn no_proxies() -> HashSet<IpAddr> {
        HashSet::new()
    }

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

    #[test]
    fn direct_client_cannot_spoof_xff() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        let peer = SocketAddr::from((Ipv4Addr::new(198, 51, 100, 7), 1234));

        let key = client_key(&headers, Some(peer), &no_proxies());
        assert_eq!(key, "198.51.100.7", "peer IP must win over spoofed XFF");
    }

    #[test]
    fn trusted_proxy_xff_is_used() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        let peer = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 1), 1234));
        let trusted: HashSet<IpAddr> = [IpAddr::V4(Ipv4Addr::LOCALHOST)].into();

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "203.0.113.9");
    }

    #[test]
    fn untrusted_proxy_gets_peer_key() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        let peer = SocketAddr::from((Ipv4Addr::new(10, 0, 0, 5), 1234));
        let trusted: HashSet<IpAddr> = [IpAddr::V4(Ipv4Addr::LOCALHOST)].into();

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "10.0.0.5");
    }

    #[test]
    fn trusted_proxy_uses_rightmost_untrusted_xff_entry() {
        let mut headers = axum::http::HeaderMap::new();
        // A malicious client can prepend entries, but cannot change the
        // entry appended by the first trusted proxy.
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.9, 203.0.113.7"),
        );
        let peer = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 1), 1234));
        let trusted: HashSet<IpAddr> = [IpAddr::V4(Ipv4Addr::LOCALHOST)].into();

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "203.0.113.7");
    }

    #[test]
    fn trusted_proxy_chain_skips_intermediate_proxies() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 10.0.0.2"),
        );
        let peer = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 1), 1234));
        let trusted: HashSet<IpAddr> = [
            IpAddr::V4(Ipv4Addr::LOCALHOST),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 2)),
        ]
        .into();

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "203.0.113.9");
    }
}
