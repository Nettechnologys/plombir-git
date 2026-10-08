//! Simple token-bucket rate limiter middleware for Axum.
//!
//! Limits requests per client address — resolved by [`crate::client_ip`] and
//! aggregated to a /64 for IPv6. Configurable requests-per-minute.
//! Returns 429 Too Many Requests when the limit is exceeded.

use axum::extract::connect_info::ConnectInfo;
use axum::extract::Request;
use axum::http::HeaderMap;
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use rg_core::task_tracker::wait_optional_shutdown;
use std::collections::HashMap;
use std::net::SocketAddr;

use crate::client_ip::{budget_key, ClientIp, ClientIpResolver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Default hard cap on the number of distinct client keys tracked at once.
/// Chosen to bound worst-case memory (~a few MB of `ClientState` + keys) while
/// comfortably exceeding any realistic legitimate client population.
///
/// Public for the same reason `DEFAULT_PACKAGE_UPLOAD_MAX_BYTES` is:
/// `plombir-git.example.toml` states this number to the operator beside
/// `[rate_limit].max_keys = 0`, and the only way to check that statement
/// against the value the limiter actually uses is for a test in `rg-cli` to be
/// able to read it.
pub const DEFAULT_MAX_KEYS: usize = 100_000;

/// Minimum spacing between inline (cap-triggered) sweeps of expired entries.
/// Throttles the O(n) `retain` so a sustained distinct-IP flood pays it at
/// most once per second instead of on every request.
const SWEEP_MIN_INTERVAL: Duration = Duration::from_secs(1);

/// How many per-client budgets the shared overflow bucket holds.
///
/// Once the map is full, every client it has no room for spends from one
/// shared bucket instead of being refused outright: a full map used to answer
/// every new client with 429, so whoever could fill it — 100k cheap requests
/// from distinct addresses — locked everyone else out of login until their
/// windows expired (card_c2f0454ceb89). The bucket is shared, so it is bigger
/// than one client's budget; it is still finite, so the flood that filled the
/// map cannot also use it to escape limiting.
const OVERFLOW_BUDGETS: u32 = 10;

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
    /// The one bucket every client spends from while the map has no room for
    /// it. `None` until the map first fills.
    overflow: Option<ClientState>,
}

impl ClientMap {
    fn new(now: Instant) -> Self {
        Self {
            entries: HashMap::new(),
            last_sweep: now,
            overflow: None,
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
    /// map is full a previously-unseen key spends from the shared overflow
    /// bucket instead of being inserted, so a distinct-IP flood can neither
    /// exhaust memory nor lock out every client that arrives after it.
    max_keys: usize,
    /// How a request's client address is worked out — the same resolver the
    /// rest of the server uses, so the limiter and the audit log agree.
    resolver: ClientIpResolver,
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
            resolver: ClientIpResolver::default(),
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
        trusted_proxies: Vec<crate::client_ip::TrustedProxy>,
    ) -> Self {
        Self::with_resolver(
            max_requests,
            window_secs,
            ClientIpResolver::new(trusted_proxies),
        )
    }

    /// Create a new rate limiter that resolves client addresses with
    /// `resolver` — pass the server's own, so every consumer agrees.
    pub fn with_resolver(max_requests: u32, window_secs: u64, resolver: ClientIpResolver) -> Self {
        let mut limiter = Self::new(max_requests, window_secs);
        limiter.resolver = resolver;
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
            // Still full after the (possible) sweep → the shared bucket.
            if guard.entries.len() >= self.max_keys {
                let budget = self.max_requests.saturating_mul(OVERFLOW_BUDGETS);
                let overflow = guard.overflow.get_or_insert(ClientState {
                    tokens: budget,
                    reset_at: now + window,
                });
                if now >= overflow.reset_at {
                    overflow.tokens = budget;
                    overflow.reset_at = now + window;
                    tracing::warn!(
                        max_keys = self.max_keys,
                        "rate limiter is tracking as many clients as it may; new clients share one overflow budget"
                    );
                }
                return if overflow.tokens > 0 {
                    overflow.tokens -= 1;
                    true
                } else {
                    false
                };
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

    /// Spend one request from `key`'s budget — for a limiter keyed by an
    /// identity rather than by the client address, such as the bot-account
    /// limiter the PAT middleware drives. `true` when the request may proceed.
    pub(crate) fn allow_key(&self, key: &str) -> bool {
        self.allow(key)
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

    /// The budget a request spends from: the client address the server
    /// already resolved, or — for a limiter mounted where that layer did not
    /// run — the same resolution done here.
    fn client_key(
        &self,
        resolved: Option<ClientIp>,
        headers: &HeaderMap,
        addr: SocketAddr,
    ) -> String {
        let ip = match resolved {
            Some(ClientIp(ip)) => ip,
            None => self.resolver.resolve(addr.ip(), headers),
        };
        budget_key(ip)
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
    let resolved = request.extensions().get::<ClientIp>().copied();
    let key = limiter.client_key(resolved, &headers, addr);

    if limiter.allow(&key) {
        next.run(request).await
    } else {
        // Record metric for observability
        if let Some(c) = crate::metrics::rate_limit::BLOCKED.get() {
            c.inc();
        }
        let path = request.uri().path();
        let message = "Too many requests. Please try again later.";
        crate::refusal::pre_router_refusal_response(
            path,
            axum::http::StatusCode::TOO_MANY_REQUESTS,
            || crate::error::AppError::rate_limited(message).into_response(),
            rg_core::package_registry::oci::types::error_codes::TOO_MANY_REQUESTS,
            message,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::IpAddr;

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

    /// card_c2f0454ceb89: a full map no longer answers every new client with
    /// 429. They share one overflow bucket, which is finite.
    #[test]
    fn test_max_keys_cap_sends_new_clients_to_a_shared_overflow_bucket() {
        let limiter = RateLimiter::new(2, 60).with_max_keys(2);
        // Fill the map with two distinct client keys.
        assert!(limiter.allow("a"));
        assert!(limiter.allow("b"));
        // A newcomer is served, from the overflow bucket...
        assert!(limiter.allow("c"));
        // ...which every newcomer shares, up to its budget.
        let budget = 2 * OVERFLOW_BUDGETS;
        for n in 1..budget {
            assert!(limiter.allow(&format!("newcomer-{n}")), "request {n}");
        }
        assert!(!limiter.allow("d"), "the overflow bucket is finite");
        // The map did not grow: the memory bound still holds.
        assert_eq!(limiter.clients.lock().unwrap().entries.len(), 2);
        // Already-tracked clients keep being served from their own buckets.
        assert!(limiter.allow("a"));
        assert!(limiter.allow("b"));
    }

    /// The flood that filled the map with IPv6 addresses of one /64 filled it
    /// with one key.
    #[test]
    fn test_ipv6_clients_of_one_64_share_one_key() {
        let limiter = RateLimiter::new(2, 60).with_max_keys(2);
        let headers = HeaderMap::new();
        for host in 1..50u16 {
            let addr = SocketAddr::new(
                IpAddr::V6(std::net::Ipv6Addr::new(0x2001, 0xdb8, 1, 2, 0, 0, 0, host)),
                4000,
            );
            limiter.allow(&limiter.client_key(None, &headers, addr));
        }
        assert_eq!(limiter.clients.lock().unwrap().entries.len(), 1);
        let neighbour: SocketAddr = "[2001:db8:1:3::1]:4000".parse().unwrap();
        assert!(limiter.allow(&limiter.client_key(None, &headers, neighbour)));
    }

    #[test]
    fn test_max_keys_cap_reclaims_expired_slots() {
        // 1-second window, room for a single client.
        let clock = ManualClock::new();
        let limiter = RateLimiter::new(5, 1)
            .with_max_keys(1)
            .with_clock(clock.clone());
        assert!(limiter.allow("a")); // inserts "a"
        assert!(limiter.allow("b")); // full, "a" not expired → overflow
        assert!(!limiter.clients.lock().unwrap().entries.contains_key("b"));

        // After "a"'s window expires, the throttled inline sweep evicts it and
        // the freed slot admits a genuinely new client. The step also clears
        // SWEEP_MIN_INTERVAL, which `with_clock` anchored to the same timeline.
        clock.advance(Duration::from_millis(1100));
        assert!(limiter.allow("b"));
        assert!(limiter.clients.lock().unwrap().entries.contains_key("b"));
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
    fn test_client_key_ignores_forwarded_headers_by_default() {
        let limiter = RateLimiter::new(10, 60);
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.10".parse().unwrap());
        let addr: SocketAddr = "198.51.100.2:12345".parse().unwrap();

        assert_eq!(limiter.client_key(None, &headers, addr), "198.51.100.2");
    }

    #[test]
    fn test_client_key_uses_forwarded_headers_from_trusted_proxy() {
        let limiter =
            RateLimiter::with_trusted_proxies(10, 60, vec!["198.51.100.2".parse().unwrap()]);
        let mut headers = HeaderMap::new();
        headers.insert("x-forwarded-for", "203.0.113.10".parse().unwrap());
        let addr: SocketAddr = "198.51.100.2:12345".parse().unwrap();

        assert_eq!(limiter.client_key(None, &headers, addr), "203.0.113.10");

        // The left entry is the client's own word behind an appending proxy.
        headers.insert("x-forwarded-for", "1.2.3.4, 203.0.113.10".parse().unwrap());
        assert_eq!(limiter.client_key(None, &headers, addr), "203.0.113.10");
    }

    /// Where the server already resolved the address, that is the key.
    #[test]
    fn test_client_key_prefers_the_resolved_address() {
        let limiter = RateLimiter::new(10, 60);
        let addr: SocketAddr = "198.51.100.2:12345".parse().unwrap();
        let resolved = Some(ClientIp("203.0.113.10".parse().unwrap()));
        assert_eq!(
            limiter.client_key(resolved, &HeaderMap::new(), addr),
            "203.0.113.10"
        );
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
