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
use std::time::{Duration, Instant};

/// Default hard cap on the number of distinct client keys tracked at once.
/// Chosen to bound worst-case memory (~a few MB of `ClientState` + keys) while
/// comfortably exceeding any realistic legitimate client population.
const DEFAULT_MAX_KEYS: usize = 100_000;

/// Minimum spacing between inline (cap-triggered) sweeps of expired entries.
/// Throttles the O(n) `retain` so a sustained distinct-IP flood pays it at
/// most once per second instead of on every request.
const SWEEP_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// Source of "now" for the limiter.
///
/// Production always reads the system clock. The test build gets a second
/// variant so window expiry can be exercised by moving time forward instead of
/// sleeping through a real window — `Instant` cannot be constructed at an
/// arbitrary point, so the only way to observe a reset is to control the reads.
/// Outside `cfg(test)` the enum has a single variant and is zero-sized, so both
/// the production layout and the emitted code path are unchanged.
#[derive(Debug, Clone)]
enum Clock {
    System,
    #[cfg(test)]
    Manual(Arc<Mutex<Instant>>),
}

impl Clock {
    fn now(&self) -> Instant {
        match self {
            Clock::System => Instant::now(),
            #[cfg(test)]
            Clock::Manual(t) => *t.lock().unwrap_or_else(|p| p.into_inner()),
        }
    }
}

/// Per-client rate limit state.
#[derive(Debug)]
struct ClientState {
    /// Number of requests remaining in the current window.
    tokens: u32,
    /// When the current window resets.
    reset_at: Instant,
}

/// The client map plus bookkeeping for the amortized inline sweep.
#[derive(Debug)]
struct ClientMap {
    /// Client key → per-client state.
    entries: HashMap<String, ClientState>,
    /// Last time an inline (cap-triggered) sweep of expired entries ran.
    last_sweep: Instant,
}

impl ClientMap {
    fn new(now: Instant) -> Self {
        Self {
            entries: HashMap::new(),
            last_sweep: now,
        }
    }
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
    /// Hard cap on the number of distinct client keys tracked at once. Once the
    /// map is full a previously-unseen key is rejected (429) instead of being
    /// inserted, so a distinct-IP flood cannot exhaust memory.
    max_keys: usize,
    /// Proxy IPs whose X-Forwarded-For / X-Real-IP headers are trusted.
    trusted_proxies: Arc<Vec<IpAddr>>,
    /// Client IP → state mapping.
    /// std::sync::Mutex is used because critical sections are very short
    /// (single HashMap lookup/update) and never await.
    clients: Arc<Mutex<ClientMap>>,
    /// Where `now` comes from. Always the system clock in production.
    clock: Clock,
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
            max_keys: DEFAULT_MAX_KEYS,
            trusted_proxies: Arc::new(Vec::new()),
            clients: Arc::new(Mutex::new(ClientMap::new(Instant::now()))),
            clock: Clock::System,
        }
    }

    /// Override the maximum number of distinct client keys tracked at once.
    /// A value of `0` is treated as "use the default" rather than "track
    /// nothing", so a misconfigured `max_keys = 0` never disables limiting.
    pub fn with_max_keys(mut self, max_keys: usize) -> Self {
        self.max_keys = if max_keys == 0 {
            DEFAULT_MAX_KEYS
        } else {
            max_keys
        };
        self
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

        let mut guard = match self.clients.lock() {
            Ok(guard) => guard,
            // If the mutex is poisoned, reset the map and continue
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                guard.entries.clear();
                return true;
            }
        };
        let now = self.clock.now();
        let window = Duration::from_secs(self.window_secs);

        // Fast path: a client we already track — update its bucket in place.
        // No cap check needed since this never grows the map.
        if let Some(entry) = guard.entries.get_mut(key) {
            if now >= entry.reset_at {
                entry.tokens = self.max_requests;
                entry.reset_at = now + window;
            }
            return if entry.tokens > 0 {
                entry.tokens -= 1;
                true
            } else {
                false
            };
        }

        // New client key. Enforce the max_keys cap BEFORE inserting so a
        // distinct-IP flood cannot grow the map without bound.
        if guard.entries.len() >= self.max_keys {
            // Amortized inline sweep of expired entries, throttled to at most
            // once per SWEEP_MIN_INTERVAL so we never pay an O(n) scan on every
            // request during a flood. Reclaims slots for genuinely new clients
            // once old windows expire.
            if now.duration_since(guard.last_sweep) >= SWEEP_MIN_INTERVAL {
                guard.entries.retain(|_, state| now < state.reset_at);
                guard.last_sweep = now;
            }
            // Still full after the (possible) sweep → reject the new key.
            if guard.entries.len() >= self.max_keys {
                return false;
            }
        }

        // Admit the new client, consuming one token immediately.
        guard.entries.insert(
            key.to_string(),
            ClientState {
                tokens: self.max_requests - 1,
                reset_at: now + window,
            },
        );
        true
    }

    /// Clean up expired entries. Called periodically by the background task.
    fn cleanup(&self) {
        let mut guard = match self.clients.lock() {
            Ok(guard) => guard,
            Err(poisoned) => {
                let mut guard = poisoned.into_inner();
                guard.entries.clear();
                return;
            }
        };
        let now = self.clock.now();
        guard.entries.retain(|_, state| now < state.reset_at);
        guard.last_sweep = now;
    }

    /// Spawn a background task that periodically cleans up expired entries.
    pub fn spawn_cleanup_task(&self) {
        self.spawn_cleanup_task_with_shutdown(None);
    }

    /// Spawn the periodic cleanup task, optionally wired to a graceful shutdown
    /// signal so it exits cleanly on `SIGTERM`/ctrl_c instead of being aborted
    /// when the runtime winds down. Cleanup is pure in-memory bookkeeping, so
    /// there is no state to flush — this only lets the task stop gracefully.
    pub fn spawn_cleanup_task_with_shutdown(
        &self,
        mut shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
    ) {
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
                tokio::select! {
                    _ = interval.tick() => limiter.cleanup(),
                    _ = wait_optional_shutdown(&mut shutdown_rx) => {
                        tracing::info!("rate-limit cleanup received shutdown, stopping");
                        break;
                    }
                }
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

/// A clock the test drives by hand, shared with the limiter it is injected
/// into. Lets a window expire in zero wall-clock time.
#[cfg(test)]
#[derive(Debug, Clone)]
struct ManualClock(Arc<Mutex<Instant>>);

#[cfg(test)]
impl ManualClock {
    fn new() -> Self {
        Self(Arc::new(Mutex::new(Instant::now())))
    }

    /// Move the limiter's notion of "now" forward.
    fn advance(&self, by: Duration) {
        let mut guard = self.0.lock().unwrap_or_else(|p| p.into_inner());
        *guard += by;
    }
}

#[cfg(test)]
impl RateLimiter {
    /// Drive this limiter from `clock` instead of the system clock.
    ///
    /// Also re-seeds the sweep bookkeeping, which `new()` anchored to a real
    /// `Instant`, so the throttle interval is measured against the injected
    /// timeline rather than against construction time.
    fn with_clock(mut self, clock: ManualClock) -> Self {
        let now = *clock.0.lock().unwrap_or_else(|p| p.into_inner());
        self.clients
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .last_sweep = now;
        self.clock = Clock::Manual(clock.0);
        self
    }
}

/// Await a shutdown signal if present, otherwise never resolve. Lets a
/// `tokio::select!` arm be conditionally armed on an `Option<Receiver>`.
async fn wait_optional_shutdown(shutdown_rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match shutdown_rx {
        Some(rx) => {
            let _ = rx.changed().await;
        }
        None => std::future::pending::<()>().await,
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
        let clock = ManualClock::new();
        let limiter = RateLimiter::new(1, 1).with_clock(clock.clone()); // 1 second window
        assert!(limiter.allow("client_a"));
        assert!(!limiter.allow("client_a"));
        // Step past the window instead of sleeping through it: the limiter reads
        // its clock on every call, so this is the same observation without the
        // 1.1s of wall clock or the dependency on how long the OS actually slept.
        clock.advance(Duration::from_millis(1100));
        assert!(limiter.allow("client_a")); // reset after window
    }

    #[test]
    fn test_default_max_keys() {
        // A freshly constructed limiter uses the built-in default cap.
        assert_eq!(RateLimiter::new(5, 60).max_keys, DEFAULT_MAX_KEYS);
    }

    #[test]
    fn test_with_max_keys_zero_falls_back_to_default() {
        // A misconfigured `max_keys = 0` must not silently stop tracking clients.
        assert_eq!(
            RateLimiter::new(5, 60).with_max_keys(0).max_keys,
            DEFAULT_MAX_KEYS
        );
        assert_eq!(RateLimiter::new(5, 60).with_max_keys(42).max_keys, 42);
    }

    #[test]
    fn test_max_keys_cap_rejects_new_clients_when_full() {
        let limiter = RateLimiter::new(5, 60).with_max_keys(2);
        // Fill the map with two distinct client keys.
        assert!(limiter.allow("a"));
        assert!(limiter.allow("b"));
        // A previously-unseen key is rejected once the map is full — this is
        // the memory-exhaustion guard under a distinct-IP flood.
        assert!(!limiter.allow("c"));
        assert!(!limiter.allow("d"));
        // Already-tracked clients keep being served from their own buckets.
        assert!(limiter.allow("a"));
        assert!(limiter.allow("b"));
    }

    #[test]
    fn test_max_keys_cap_reclaims_expired_slots() {
        // 1-second window, room for a single client.
        let clock = ManualClock::new();
        let limiter = RateLimiter::new(5, 1)
            .with_max_keys(1)
            .with_clock(clock.clone());
        assert!(limiter.allow("a")); // inserts "a"
        assert!(!limiter.allow("b")); // full, "a" not expired → reject "b"

        // After "a"'s window expires, the throttled inline sweep evicts it and
        // the freed slot admits a genuinely new client. The step also clears
        // SWEEP_MIN_INTERVAL, which `with_clock` anchored to the same timeline.
        clock.advance(Duration::from_millis(1100));
        assert!(limiter.allow("b"));
    }

    #[test]
    fn test_disabled_limiter_ignores_cap() {
        // With limiting disabled every request is allowed regardless of cap.
        let limiter = RateLimiter::new(0, 60).with_max_keys(1);
        for i in 0..100 {
            assert!(limiter.allow(&format!("client_{i}")));
        }
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

    /// End-to-end check of the per-route mechanism the credential endpoints use:
    /// the limiter attached via `.layer()` returns 429 once the per-IP budget is
    /// spent, and it correctly reads `ConnectInfo` from request extensions.
    #[tokio::test]
    async fn test_per_route_middleware_returns_429_after_limit() {
        use axum::body::Body;
        use axum::extract::connect_info::ConnectInfo;
        use axum::http::{Request, StatusCode};
        use axum::routing::post;
        use axum::Router;
        use tower::ServiceExt;

        async fn dummy() -> &'static str {
            "ok"
        }

        let limiter = RateLimiter::new(2, 60);
        let app: Router = Router::new().route(
            "/register",
            post(dummy).layer(axum::middleware::from_fn_with_state(
                limiter,
                rate_limit_middleware,
            )),
        );

        let addr: SocketAddr = "203.0.113.7:5555".parse().unwrap();
        let make_req = || {
            let mut req = Request::builder()
                .method("POST")
                .uri("/register")
                .body(Body::empty())
                .unwrap();
            req.extensions_mut().insert(ConnectInfo(addr));
            req
        };

        // The first two requests from this IP are within budget.
        for _ in 0..2 {
            let resp = app.clone().oneshot(make_req()).await.unwrap();
            assert_eq!(resp.status(), StatusCode::OK);
        }
        // The third is rejected with 429 regardless of any global limit.
        let resp = app.clone().oneshot(make_req()).await.unwrap();
        assert_eq!(resp.status(), StatusCode::TOO_MANY_REQUESTS);
    }
}
