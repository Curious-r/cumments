//! Small in-memory sliding-window rate limiter.
//!
//! Used for anti-spam on open endpoints (site registration and verification
//! issuance). It is not a security boundary: keys come from the client IP,
//! which behind a reverse proxy depends on `X-Forwarded-For` being set
//! correctly.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use crate::trusted_proxy::TrustedProxySet;

/// Upper bound on tracked keys; beyond it, new keys are refused instead of
/// growing memory without limit (keys are client-controlled via XFF).
const MAX_KEYS: usize = 10_000;
/// Upper bound on SSE limiter keys. Eviction keeps attacker-controlled keys
/// from growing process memory; the semaphore remains the hard concurrency cap.
const MAX_SSE_KEYS: usize = 10_000;

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
    trusted_proxies: &TrustedProxySet,
) -> String {
    if addr.is_some_and(|peer| trusted_proxies.contains(peer.ip())) {
        if let Some(forwarded) = headers
            .get("x-forwarded-for")
            .and_then(|value| value.to_str().ok())
        {
            for entry in forwarded.split(',').rev() {
                let entry = entry.trim();
                if entry.is_empty() {
                    continue;
                }
                // Only an IP can identify a downstream client safely. A
                // malformed untrusted value means the proxy chain violates the
                // XFF contract; stop instead of adopting attacker-controlled
                // text (of arbitrary length) as a limiter key.
                let Ok(ip) = entry.parse::<IpAddr>() else {
                    break;
                };
                if trusted_proxies.contains(ip) {
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

    /// The endpoint's fixed rate-limit window. Used as the conservative
    /// `Retry-After` value on 429 responses.
    pub fn window(&self) -> Duration {
        self.window
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

/// Bounded token-bucket limiter for expensive, long-lived SSE connections.
///
/// `burst` is the bucket capacity and `requests / window` is the refill rate.
/// Unlike a fixed window, this absorbs EventSource reconnects and page reloads
/// without remembering disconnect history, while still enforcing a sustained
/// connection-establishment rate. Keys are LRU-evicted so a flood of spoofed or
/// rotating addresses cannot grow the map without bound.
pub struct SseRateLimiter {
    requests: usize,
    window: Duration,
    burst: usize,
    buckets: Mutex<SseBuckets>,
}

#[derive(Default)]
struct SseBuckets {
    buckets: HashMap<String, SseBucket>,
    lru: VecDeque<String>,
}

struct SseBucket {
    tokens: f64,
    last_refill: Instant,
}

impl SseRateLimiter {
    pub fn new(requests: usize, window: Duration, burst: usize) -> Self {
        Self {
            requests,
            window,
            burst,
            buckets: Mutex::new(SseBuckets::default()),
        }
    }

    /// Consumes one connection token or returns how long to wait for the next.
    pub fn acquire(&self, key: &str) -> Result<(), Duration> {
        let now = Instant::now();
        let mut state = self.buckets.lock().expect("SSE limiter mutex poisoned");
        state.retain(&now, self.capacity_seconds());

        let tokens = match state.buckets.get_mut(key) {
            Some(bucket) => {
                bucket.refill(now, self.refill_rate(), self.burst);
                if bucket.tokens < 1.0 {
                    let wait = Duration::from_secs_f64((1.0 - bucket.tokens) / self.refill_rate());
                    state.touch(key);
                    return Err(wait);
                }
                bucket.tokens -= 1.0;
                bucket.last_refill = now;
                bucket.tokens
            }
            None => {
                state.insert(
                    key.to_string(),
                    SseBucket {
                        tokens: self.burst as f64 - 1.0,
                        last_refill: now,
                    },
                );
                self.burst as f64 - 1.0
            }
        };

        state.touch(key);
        debug_assert!(tokens >= 0.0);
        Ok(())
    }

    fn refill_rate(&self) -> f64 {
        self.requests as f64 / self.window.as_secs_f64()
    }

    fn capacity_seconds(&self) -> f64 {
        self.burst as f64 / self.refill_rate()
    }
}

impl SseBuckets {
    fn insert(&mut self, key: String, bucket: SseBucket) {
        while self.lru.len() >= MAX_SSE_KEYS {
            let Some(oldest) = self.lru.pop_front() else {
                break;
            };
            self.buckets.remove(&oldest);
        }
        self.lru.push_back(key.clone());
        self.buckets.insert(key, bucket);
    }

    fn touch(&mut self, key: &str) {
        if let Some(index) = self.lru.iter().position(|entry| entry == key)
            && let Some(entry) = self.lru.remove(index)
        {
            self.lru.push_back(entry);
        }
    }

    fn retain(&mut self, now: &Instant, capacity_seconds: f64) {
        let ttl = Duration::from_secs_f64(capacity_seconds.max(1.0));
        self.buckets
            .retain(|_, bucket| now.duration_since(bucket.last_refill) < ttl);
        self.lru.retain(|key| self.buckets.contains_key(key));
    }
}

impl SseBucket {
    fn refill(&mut self, now: Instant, refill_rate: f64, capacity: usize) {
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * refill_rate).min(capacity as f64);
        self.last_refill = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::trusted_proxy::TrustedProxyRule;
    use axum::http::HeaderValue;
    use std::net::Ipv4Addr;

    fn no_proxies() -> TrustedProxySet {
        TrustedProxySet::default()
    }

    fn trusted_proxies(entries: &[&str]) -> TrustedProxySet {
        let rules: Vec<_> = entries
            .iter()
            .map(|entry| TrustedProxyRule::parse(entry).expect("valid trusted proxy rule"))
            .collect();
        TrustedProxySet::from_rules(&rules).expect("valid trusted proxy set")
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
        let trusted = trusted_proxies(&["127.0.0.1/32"]);

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "203.0.113.9");
    }

    #[test]
    fn untrusted_proxy_gets_peer_key() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert("x-forwarded-for", HeaderValue::from_static("203.0.113.9"));
        let peer = SocketAddr::from((Ipv4Addr::new(10, 0, 0, 5), 1234));
        let trusted = trusted_proxies(&["127.0.0.1/32"]);

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
        let trusted = trusted_proxies(&["127.0.0.1/32"]);

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
        let trusted = trusted_proxies(&["127.0.0.1/32", "10.0.0.2/32"]);

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "203.0.113.9");
    }

    #[test]
    fn cidr_trusted_proxy_skips_intermediate_proxies() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 10.0.0.2"),
        );
        let peer = SocketAddr::from((Ipv4Addr::new(10, 0, 0, 5), 1234));
        let trusted = trusted_proxies(&["10.0.0.0/8"]);

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "203.0.113.9");
    }

    #[test]
    fn malformed_trusted_proxy_xff_falls_back_to_peer() {
        let mut headers = axum::http::HeaderMap::new();
        // The rightmost entry is the value this hop would use as a client.
        // If it is not an IP, we cannot establish a safe per-client identity.
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("198.51.100.9, definitely-not-an-ip"),
        );
        let peer = SocketAddr::from((Ipv4Addr::new(127, 0, 0, 1), 1234));
        let trusted = trusted_proxies(&["127.0.0.1/32"]);

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "127.0.0.1");
    }

    #[test]
    fn sse_token_bucket_allows_burst_then_reports_next_token_wait() {
        let limiter = SseRateLimiter::new(1, Duration::from_secs(2), 2);
        let key = "client";

        assert!(limiter.acquire(key).is_ok());
        assert!(limiter.acquire(key).is_ok());

        let wait = limiter.acquire(key).expect_err("burst is exhausted");
        assert!(
            wait >= Duration::from_millis(1900),
            "unexpected wait {wait:?}"
        );
    }

    #[test]
    fn sse_limiter_evicts_the_oldest_key_at_capacity() {
        let limiter = SseRateLimiter::new(1, Duration::from_secs(3600), 1);
        for i in 0..MAX_SSE_KEYS {
            assert!(
                limiter.acquire(&format!("key-{i}")).is_ok(),
                "failed to insert key {i}"
            );
        }
        assert_eq!(
            limiter.buckets.lock().expect("buckets").buckets.len(),
            MAX_SSE_KEYS
        );

        assert!(limiter.acquire("new-key").is_ok());
        assert_eq!(
            limiter.buckets.lock().expect("buckets").buckets.len(),
            MAX_SSE_KEYS
        );

        // The oldest key was evicted, so its next attempt starts a fresh bucket.
        assert!(limiter.acquire("key-0").is_ok());
        assert_eq!(
            limiter.buckets.lock().expect("buckets").buckets.len(),
            MAX_SSE_KEYS
        );
    }

    #[test]
    fn untrusted_peer_cannot_spoof_a_trusted_cidr_entry() {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("203.0.113.9, 10.0.0.2"),
        );
        let peer = SocketAddr::from((Ipv4Addr::new(192, 0, 2, 1), 1234));
        let trusted = trusted_proxies(&["10.0.0.0/8"]);

        let key = client_key(&headers, Some(peer), &trusted);
        assert_eq!(key, "192.0.2.1");
    }
}
