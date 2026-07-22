//! Simple token-bucket rate limiter middleware for Axum.
//!
//! Limits requests per IP address. Configurable requests-per-minute.
//! Returns 429 Too Many Requests when the limit is exceeded.

use axum::extract::connect_info::ConnectInfo;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Per-client rate limit state.
#[derive(Debug)]
struct ClientState {
    /// Number of requests remaining in the current window.
    tokens: u32,
    /// When the current window resets.
    reset_at: Instant,
}

/// Shared rate limiter state.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    /// Whether request limiting is active.
    enabled: bool,
    /// Maximum requests per window.
    max_requests: u32,
    /// Window duration in seconds.
    window_secs: u64,
    /// Proxy IPs whose X-Forwarded-For / X-Real-IP headers are trusted.
    trusted_proxies: Arc<Vec<IpAddr>>,
    /// Client IP → state mapping.
    /// std::sync::Mutex is used because critical sections are very short
    /// (single HashMap lookup/update) and never await.
    clients: Arc<Mutex<HashMap<String, ClientState>>>,
}

impl RateLimiter {
    /// Create a new rate limiter.
    ///
    /// - `max_requests`: maximum number of requests allowed per window.
    /// - `window_secs`: duration of the rate limit window in seconds.
    pub fn new(max_requests: u32, window_secs: u64) -> Self {
        Self {
            enabled: max_requests > 0,
            max_requests: max_requests.max(1),
            window_secs: window_secs.max(1),
            trusted_proxies: Arc::new(Vec::new()),
            clients: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Create a new rate limiter that trusts proxy headers only from the
    /// provided proxy source IPs.
    pub fn with_trusted_proxies(
        max_requests: u32,
        window_secs: u64,
        trusted_proxies: Vec<IpAddr>,
    ) -> Self {
        let mut limiter = Self::new(max_requests, window_secs);
        limiter.trusted_proxies = Arc::new(trusted_proxies);
        limiter
    }

    /// Check if a request is allowed. Returns true if the request should proceed.
    fn allow(&self, key: &str) -> bool {
        if !self.enabled {
            return true;
        }

        let mut clients = match self.clients.lock() {
            Ok(guard) => guard,
            // If the mutex is poisoned, reset the map and continue
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                guard.clear();
                return true;
            }
        };
        let now = Instant::now();

        let entry = clients
            .entry(key.to_string())
            .or_insert_with(|| ClientState {
                tokens: self.max_requests,
                reset_at: now + std::time::Duration::from_secs(self.window_secs),
            });

        // Reset window if expired
        if now >= entry.reset_at {
            entry.tokens = self.max_requests;
            entry.reset_at = now + std::time::Duration::from_secs(self.window_secs);
        }

        if entry.tokens > 0 {
            entry.tokens -= 1;
            true
        } else {
            false
        }
    }

    /// Clean up expired entries. Called periodically by the background task.
    fn cleanup(&self) {
        let mut clients = match self.clients.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                guard.clear();
                return;
            }
        };
        let now = Instant::now();
        clients.retain(|_, state| now < state.reset_at);
    }

    /// Spawn a background task that periodically cleans up expired entries.
    pub fn spawn_cleanup_task(&self) {
        if !self.enabled {
            return;
        }

        let limiter = self.clone();
        // Cleanup interval: half the window duration, min 60s, max 600s
        let interval_secs = (self.window_secs / 2).clamp(60, 600);
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_secs(interval_secs));
            interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
            loop {
                interval.tick().await;
                limiter.cleanup();
            }
        });
    }

    fn client_key(&self, headers: &HeaderMap, addr: SocketAddr) -> String {
        if self.trusted_proxies.contains(&addr.ip()) {
            if let Some(forwarded) = extract_forwarded_client_key(headers) {
                return forwarded;
            }
        }
        addr.ip().to_string()
    }
}

/// Extract client IP from trusted proxy headers (X-Forwarded-For, X-Real-IP).
/// Returns `None` if no identifying header is present.
fn extract_forwarded_client_key(headers: &HeaderMap) -> Option<String> {
    // Try X-Forwarded-For first (first IP in the list)
    if let Some(xff) = headers.get("x-forwarded-for") {
        if let Ok(val) = xff.to_str() {
            if let Some(ip) = val.split(',').next() {
                let ip = ip.trim();
                if !ip.is_empty() {
                    return Some(ip.to_string());
                }
            }
        }
    }

    // Try X-Real-IP
    if let Some(xri) = headers.get("x-real-ip") {
        if let Ok(val) = xri.to_str() {
            let val = val.trim();
            if !val.is_empty() {
                return Some(val.to_string());
            }
        }
    }

    None
}

/// Axum middleware for rate limiting.
///
/// Records a `rate_limit_blocks_total` counter for each blocked request.
pub async fn rate_limit_middleware(
    axum::extract::State(limiter): axum::extract::State<RateLimiter>,
    headers: HeaderMap,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    request: Request,
    next: Next,
) -> Response {
    let key = limiter.client_key(&headers, addr);

    if limiter.allow(&key) {
        next.run(request).await
    } else {
        // Record metric for observability
        if let Some(c) = crate::metrics::rate_limit::BLOCKED.get() {
            c.inc();
        }
        crate::error::AppError::rate_limited("Too many requests. Please try again later.")
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_rate_limiter_allows_within_limit() {
        let limiter = RateLimiter::new(5, 60);
        for _ in 0..5 {
            assert!(limiter.allow("client_a"));
        }
    }

    #[test]
    fn test_rate_limiter_disabled_when_max_is_zero() {
        let limiter = RateLimiter::new(0, 60);
        for _ in 0..100 {
            assert!(limiter.allow("client_a"));
        }
    }

    #[test]
    fn test_rate_limiter_blocks_over_limit() {
        let limiter = RateLimiter::new(3, 60);
        assert!(limiter.allow("client_a"));
        assert!(limiter.allow("client_a"));
        assert!(limiter.allow("client_a"));
        assert!(!limiter.allow("client_a")); // 4th request blocked
    }

    #[test]
    fn test_rate_limiter_per_client() {
        let limiter = RateLimiter::new(2, 60);
        assert!(limiter.allow("client_a"));
        assert!(limiter.allow("client_a"));
        assert!(!limiter.allow("client_a")); // a blocked
                                             // Different client has own bucket
        assert!(limiter.allow("client_b"));
        assert!(limiter.allow("client_b"));
    }

    #[test]
    fn test_rate_limiter_window_reset() {
        let limiter = RateLimiter::new(1, 1); // 1 second window
        assert!(limiter.allow("client_a"));
        assert!(!limiter.allow("client_a"));
        // Wait for window to expire
        std::thread::sleep(std::time::Duration::from_millis(1100));
        assert!(limiter.allow("client_a")); // reset after window
    }

    #[test]
    fn test_extract_forwarded_client_key_xff() {
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "192.168.1.1, 10.0.0.1".parse().unwrap());
        assert_eq!(
            extract_forwarded_client_key(&headers),
            Some("192.168.1.1".to_string())
        );
    }

    #[test]
    fn test_extract_forwarded_client_key_xri() {
        let mut headers = HeaderMap::new();
        headers.insert("x-real-ip", "10.0.0.1".parse().unwrap());
        assert_eq!(
            extract_forwarded_client_key(&headers),
            Some("10.0.0.1".to_string())
        );
    }

    #[test]
    fn test_extract_forwarded_client_key_none() {
        let headers = HeaderMap::new();
        assert_eq!(extract_forwarded_client_key(&headers), None);
    }

    #[test]
    fn test_client_key_ignores_forwarded_headers_by_default() {
        let limiter = RateLimiter::new(10, 60);
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.10".parse().unwrap());
        let addr: SocketAddr = "198.51.100.2:12345".parse().unwrap();

        assert_eq!(limiter.client_key(&headers, addr), "198.51.100.2");
    }

    #[test]
    fn test_client_key_uses_forwarded_headers_from_trusted_proxy() {
        let limiter =
            RateLimiter::with_trusted_proxies(10, 60, vec!["198.51.100.2".parse().unwrap()]);
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.10".parse().unwrap());
        let addr: SocketAddr = "198.51.100.2:12345".parse().unwrap();

        assert_eq!(limiter.client_key(&headers, addr), "203.0.113.10");
    }
}
