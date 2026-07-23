//! ForgeKeep HTTP server implementation using Axum.
//!
//! Provides:
//!  - Git Smart HTTP protocol endpoints (`/git/...`)
//!  - REST API (`/api/v1/...`)
//!  - Health check (`/health`)
//!  - TLS/HTTPS support (rustls)
//!  - API pagination
//!
//! The implementation is split across focused submodules:
//!  - [`routes`] — router assembly and the full route table
//!  - [`handlers`] — health, SPA fallback, OpenAPI/Swagger handlers
//!  - [`git_http`] — Git Smart HTTP endpoints + post-push hooks
//!  - [`pat_auth`] — PAT ⇄ JWT bridging and git actor extraction

pub mod api;
pub mod error;
pub mod git_v2;
pub mod instance;
pub mod metrics;
pub mod middleware;
pub mod oci;
pub mod openapi;
pub mod pagination;
pub mod rate_limit;
pub mod security;
pub mod ws;

mod git_http;
mod handlers;
mod pat_auth;
mod routes;

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use rg_core::package_registry::oci::OciStorage;
use sea_orm::DatabaseConnection;

/// Shared application state injected into every Axum handler via `State<AppState>`.
#[derive(Clone)]
pub struct AppState {
    pub repo_root: Arc<PathBuf>,
    pub db: DatabaseConnection,
    pub jwt_secret: Arc<String>,
    /// Optional shared secret for verifying HMAC-SHA256 signatures on *inbound*
    /// external webhooks (defense-in-depth on `/webhooks/external/*`). `None`
    /// (the default) disables signature checking; the endpoints then rely on
    /// JWT/PAT auth alone.
    pub external_webhook_secret: Option<Arc<String>>,
    pub docker_enabled: bool,
    pub external_runners: bool,
    /// Whether imageless CI jobs may run as a shell on the host (default false).
    pub allow_host_runner: bool,
    pub rate_limiter: rate_limit::RateLimiter,
    pub notification_hub: ws::NotificationHub,
    pub smtp_config: Option<rg_core::email::SmtpConfig>,
    /// Backend-neutral durable object storage.
    pub blob_storage: Arc<dyn rg_core::blob_storage::BlobStorage>,
    pub oci_storage: Arc<OciStorage>,
    pub log_write_queue: rg_core::ci::log_write_queue::LogWriteQueue,
    /// External-facing base URL for SSO callbacks (None = detect from request).
    pub external_url: Option<String>,
    /// CI job timeout in seconds.
    pub job_timeout_secs: u64,
    /// Wall-clock timeout (seconds) for streaming git operations
    /// (upload-pack / receive-pack). On elapse the `git` subprocess is killed
    /// (via `kill_on_drop`) and the handler returns 504. 0 disables the bound.
    pub git_stream_timeout_secs: u64,
    /// CI engine (M-14: trait object decouples rg-http from rg-ci).
    pub ci_engine: Arc<dyn rg_core::ci::CiTrigger + Send + Sync>,
}

/// HTTP server configuration.
pub struct HttpServerConfig {
    /// Address to listen on (e.g., "0.0.0.0:8080").
    pub listen_addr: String,
    /// Root directory for git repositories.
    pub repo_root: PathBuf,
    /// Database connection.
    pub db: DatabaseConnection,
    /// JWT secret key.
    pub jwt_secret: String,
    /// Optional shared secret for verifying HMAC-SHA256 signatures on inbound
    /// external webhooks. `None` disables signature checking (auth-only).
    pub external_webhook_secret: Option<String>,
    /// Whether Docker runner is enabled for CI jobs.
    pub docker_enabled: bool,
    /// Whether to use external runners instead of embedded runner for CI.
    pub external_runners: bool,
    /// Whether imageless CI jobs may run as a shell on the host. Defaults to
    /// `false`: on shared/public instances every job must use a Docker sandbox
    /// or a dedicated runner so pushed CI config cannot execute on the server.
    pub allow_host_runner: bool,
    /// Rate limit: max requests per window (0 = disabled).
    pub rate_limit_max: u32,
    /// Rate limit: window duration in seconds.
    pub rate_limit_window_secs: u64,
    /// Proxy source IPs whose forwarding headers are trusted for rate limiting.
    pub rate_limit_trusted_proxies: Vec<IpAddr>,
    /// Hard cap on the number of distinct client keys the rate limiter tracks
    /// at once (memory-exhaustion guard). 0 = use the built-in default (100k).
    pub rate_limit_max_keys: usize,
    /// Stricter per-IP request cap applied only to the credential endpoints
    /// (`/users/register`, `/users/login`). 0 disables the auth limiter.
    pub rate_limit_auth_max: u32,
    /// Window duration (seconds) for the credential-endpoint limiter.
    pub rate_limit_auth_window_secs: u64,
    /// SMTP configuration for email notifications (None = disabled).
    pub smtp_config: Option<rg_core::email::SmtpConfig>,
    /// OCI container registry storage path. None = use {repo_root}/oci.
    pub oci_storage_path: Option<PathBuf>,
    /// TLS configuration: (cert_path, key_path). None = HTTP only.
    pub tls_config: Option<(PathBuf, PathBuf)>,
    /// External-facing base URL (e.g., "https://git.example.com").
    /// Used for SSO callbacks. Defaults to http://localhost:{port}.
    pub external_url: Option<String>,
    /// CI job timeout in seconds (default: 3600).
    pub job_timeout_secs: u64,
    /// Wall-clock timeout (seconds) for the streaming git transport
    /// (upload-pack / receive-pack). Bounds a hung or pathologically slow `git`
    /// subprocess so it can't hold a connection + process indefinitely. 0
    /// disables the bound (default: 300).
    pub git_stream_timeout_secs: u64,
    /// CI engine implementation (M-14: injected from rg-cli, decouples rg-http from rg-ci).
    pub ci_engine: Arc<dyn rg_core::ci::CiTrigger + Send + Sync>,
    /// Graceful-shutdown signal. Flips to `true` on SIGTERM/ctrl_c; the server
    /// then stops accepting connections, drains in-flight requests, and lets the
    /// background workers exit cleanly.
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
    /// Grace window (seconds) for draining in-flight requests and the CI-log
    /// queue after the shutdown signal fires before the process is forced down.
    pub shutdown_grace_secs: u64,
}

/// Start the HTTP server and run forever.
pub async fn run(config: HttpServerConfig) -> Result<()> {
    let trusted_proxies = config.rate_limit_trusted_proxies;
    let rate_limiter = rate_limit::RateLimiter::with_trusted_proxies(
        config.rate_limit_max,
        config.rate_limit_window_secs,
        trusted_proxies.clone(),
    )
    .with_max_keys(config.rate_limit_max_keys);
    // Separate, stricter limiter for the credential endpoints (register/login).
    // It shares the trusted-proxy set and the same memory cap, but is always
    // active by default so registration spam / password guessing is throttled
    // even when the global limiter is disabled.
    let auth_rate_limiter = rate_limit::RateLimiter::with_trusted_proxies(
        config.rate_limit_auth_max,
        config.rate_limit_auth_window_secs,
        trusted_proxies,
    )
    .with_max_keys(config.rate_limit_max_keys);
    let shutdown_rx = config.shutdown_rx.clone();
    let shutdown_grace = std::time::Duration::from_secs(config.shutdown_grace_secs.max(1));

    rate_limiter.spawn_cleanup_task_with_shutdown(Some(shutdown_rx.clone()));
    auth_rate_limiter.spawn_cleanup_task_with_shutdown(Some(shutdown_rx.clone()));

    let notification_hub = ws::NotificationHub::new();

    // ── Initialize Prometheus metrics registry ──────────────────
    metrics::init_registry().expect("Failed to initialize Prometheus metrics registry");

    let blob_storage: Arc<dyn rg_core::blob_storage::BlobStorage> = Arc::new(
        rg_core::blob_storage::LocalBlobStorage::new(config.repo_root.clone()),
    );
    let oci_storage = if let Some(path) = config.oci_storage_path.as_ref() {
        tracing::warn!(
            path = %path.display(),
            "dedicated OCI storage path uses the local compatibility backend"
        );
        Arc::new(OciStorage::new(path))
    } else {
        Arc::new(OciStorage::from_backend(
            blob_storage.clone(),
            config.repo_root.join("_oci_uploads"),
        ))
    };

    // Clone DB before it moves into state
    let watchdog_db = config.db.clone();
    let log_queue_db = config.db.clone();

    let (log_write_queue, log_consumer_handle) =
        rg_core::ci::log_write_queue::LogWriteQueue::spawn_with_shutdown(
            log_queue_db,
            shutdown_rx.clone(),
        );

    let state = AppState {
        repo_root: Arc::new(config.repo_root),
        db: config.db,
        jwt_secret: Arc::new(config.jwt_secret),
        external_webhook_secret: config.external_webhook_secret.map(Arc::new),
        docker_enabled: config.docker_enabled,
        external_runners: config.external_runners,
        allow_host_runner: config.allow_host_runner,
        rate_limiter: rate_limiter.clone(),
        notification_hub: notification_hub.clone(),
        smtp_config: config.smtp_config,
        blob_storage,
        oci_storage,
        log_write_queue,
        external_url: config.external_url,
        job_timeout_secs: config.job_timeout_secs,
        git_stream_timeout_secs: config.git_stream_timeout_secs,
        ci_engine: config.ci_engine,
    };

    let app = routes::create_router(state.clone(), rate_limiter.clone(), auth_rate_limiter);

    tokio::spawn(api::ci_retention::run_cleanup_loop(
        state.clone(),
        shutdown_rx.clone(),
    ));

    // Spawn runner watchdog background task
    {
        let watchdog_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            run_runner_watchdog(watchdog_db, watchdog_shutdown).await;
        });
    }

    tracing::info!("CORS permissive mode active — tighten in production");

    // ── HTTPS mode (axum-server + rustls) ──────────────────
    //
    // CRITICAL: Axum TLS requires `axum-server`, NOT `axum::serve()` (pitfall #2)
    //
    // `axum::serve()` only supports plain TCP (no TLS).
    // To use TLS, you MUST use `axum_server::bind_rustls()` instead.
    //
    // Correct pattern (used below):
    //   let rustls_config = RustlsConfig::from_config(tls_config);
    //   axum_server::bind_rustls(addr, rustls_config).serve(app).await?;
    //
    // Wrong pattern (no TLS support):
    //   let listener = TcpListener::bind(addr).await?;
    //   axum::serve(listener, app).await?;  // ERROR: no TLS!
    if let Some((cert_path, key_path)) = &config.tls_config {
        // ── HTTPS mode (axum-server + rustls) ──────────────────────────
        let tls_config = load_tls_config(cert_path, key_path).await?;
        let config_clone = config.listen_addr.clone();

        tracing::info!(addr = %config.listen_addr, "HTTPS server listening (TLS)");

        let app = app;
        let rustls_config = axum_server::tls_rustls::RustlsConfig::from_config(tls_config);

        // axum-server drains in-flight connections via a `Handle`: on the
        // shutdown signal we ask it to stop gracefully, and it force-closes any
        // still-open connections once the grace window elapses.
        let handle = axum_server::Handle::new();
        spawn_graceful_shutdown_trigger(handle.clone(), shutdown_rx.clone(), shutdown_grace);

        axum_server::bind_rustls(
            config_clone
                .parse()
                .with_context(|| format!("invalid TLS listen address: {}", config_clone))?,
            rustls_config,
        )
        .handle(handle)
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .context("HTTPS server error")?;
    } else {
        // ── HTTP mode ───────────────────────────────────────────────────
        let listener = tokio::net::TcpListener::bind(&config.listen_addr)
            .await
            .with_context(|| format!("failed to bind to {}", config.listen_addr))?;

        tracing::info!(addr = %config.listen_addr, "HTTP server listening");

        // `axum::serve(...).with_graceful_shutdown` stops accepting new
        // connections once the signal fires and waits for in-flight requests to
        // finish. We bound that wait with the grace window so a stuck handler
        // can never block process exit indefinitely.
        let serve_fut = axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal(shutdown_rx.clone()));

        let grace_rx = shutdown_rx.clone();
        tokio::select! {
            result = serve_fut => result.context("HTTP server error")?,
            _ = async {
                shutdown_signal(grace_rx).await;
                tokio::time::sleep(shutdown_grace).await;
            } => {
                tracing::warn!(
                    grace_secs = shutdown_grace.as_secs(),
                    "graceful shutdown grace window elapsed — forcing HTTP server stop"
                );
            }
        }
    }

    // ── Drain the CI-log queue ─────────────────────────────────────────
    // The server has stopped accepting work; give the log-write consumer the
    // remaining grace window to flush its buffered writes before we return
    // (and the runtime is torn down).
    match tokio::time::timeout(shutdown_grace, log_consumer_handle).await {
        Ok(Ok(())) => tracing::info!("CI-log queue drained on shutdown"),
        Ok(Err(join_err)) => tracing::warn!(%join_err, "CI-log queue consumer join failed"),
        Err(_) => tracing::warn!("CI-log queue drain timed out within grace window"),
    }

    // ── Drain detached delivery tasks ──────────────────────────────────
    // Webhook deliveries (`webhook::service::trigger_event`) and WS
    // notifications (`ws::push_notification`) are spawned detached so handlers
    // don't block on them. The shared tracker lets us await the outstanding
    // ones — bounded by the grace window — so a SIGTERM under load doesn't
    // sever an in-flight delivery or leave a half-written `webhook_delivery`
    // row. Each delivery already carries its own outbound-HTTP timeout, so a
    // wedged remote can't hold the drain past that bound either.
    let delivery_tracker = rg_core::task_tracker::delivery_tracker();
    delivery_tracker.close();
    if !delivery_tracker.is_empty() {
        match tokio::time::timeout(shutdown_grace, delivery_tracker.wait()).await {
            Ok(()) => tracing::info!("detached delivery tasks drained on shutdown"),
            Err(_) => tracing::warn!(
                grace_secs = shutdown_grace.as_secs(),
                "detached delivery task drain timed out within grace window"
            ),
        }
    }

    Ok(())
}

/// Resolve when the graceful-shutdown signal fires (or the coordinator is
/// dropped). Used as the future handed to `with_graceful_shutdown`.
async fn shutdown_signal(mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    // Already signalled? return immediately.
    if *shutdown_rx.borrow() {
        return;
    }
    let _ = shutdown_rx.changed().await;
}

/// Bridge the `watch` shutdown signal to an `axum_server::Handle`: once the
/// signal fires, ask the handle to shut down gracefully with the grace window.
fn spawn_graceful_shutdown_trigger(
    handle: axum_server::Handle,
    shutdown_rx: tokio::sync::watch::Receiver<bool>,
    grace: std::time::Duration,
) {
    tokio::spawn(async move {
        shutdown_signal(shutdown_rx).await;
        tracing::info!("shutdown signal received — draining HTTPS connections");
        handle.graceful_shutdown(Some(grace));
    });
}

/// Load TLS certificate and private key, return a rustls ServerConfig.
async fn load_tls_config(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<Arc<tokio_rustls::rustls::ServerConfig>> {
    use std::io::BufReader;
    use tokio_rustls::rustls::pki_types::pem::PemObject;
    use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer};
    use tokio_rustls::rustls::ServerConfig;

    let cert_file = std::fs::File::open(cert_path)
        .with_context(|| format!("failed to open TLS cert: {}", cert_path.display()))?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'static>> = CertificateDer::pem_reader_iter(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse TLS certificates")?;

    let key_file = std::fs::File::open(key_path)
        .with_context(|| format!("failed to open TLS key: {}", key_path.display()))?;
    let mut key_reader = BufReader::new(key_file);

    let key = PrivateKeyDer::from_pem_reader(&mut key_reader).with_context(|| {
        format!("failed to parse TLS private key in {}", key_path.display())
    })?;

    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("failed to build TLS server config")?;

    Ok(Arc::new(server_config))
}

/// Create the Axum router for testing (no rate limiter, no static file serving).
pub fn create_router_for_test(state: AppState) -> Router {
    routes::build_test_router(state)
}

// ── Runner Watchdog ───────────────────────────────────────

/// Background task that periodically checks for:
/// 1. Stuck jobs (assigned/running for too long) → reset to pending
/// 2. Offline runners (no heartbeat) → mark as offline
async fn run_runner_watchdog(
    db: DatabaseConnection,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    // Wait for server to fully start (abort early if shutdown fires first)
    tokio::select! {
        _ = tokio::time::sleep(std::time::Duration::from_secs(10)) => {}
        _ = shutdown_rx.changed() => return,
    }

    loop {
        // Check every 60 seconds
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
            _ = shutdown_rx.changed() => {
                tracing::info!("runner watchdog received shutdown, stopping");
                break;
            }
        }

        // 1. Reset stuck jobs (assigned/running > 10 min)
        match rg_db::ops::pipeline_ops::find_stuck_jobs(&db, 600).await {
            Ok(stuck) => {
                for job in &stuck {
                    let job_id = job.id;
                    tracing::warn!(
                        job_id,
                        status = %job.status,
                        "Runner watchdog: resetting stuck job"
                    );
                    if let Err(e) = rg_db::ops::pipeline_ops::reset_stuck_job(&db, job_id).await {
                        tracing::error!(job_id, error = %e, "Failed to reset stuck job");
                    }
                }
                if !stuck.is_empty() {
                    tracing::info!(
                        count = stuck.len(),
                        "Runner watchdog: reset {} stuck jobs",
                        stuck.len()
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Runner watchdog: failed to find stuck jobs");
            }
        }

        // 2. Mark runners as offline if no heartbeat for 90 seconds
        match rg_db::ops::pipeline_ops::find_offline_runners(&db, 90).await {
            Ok(offline) => {
                for runner in &offline {
                    tracing::warn!(
                        runner_id = runner.id,
                        name = %runner.name,
                        "Runner watchdog: marking runner as offline"
                    );
                    if let Err(e) =
                        rg_db::ops::runner_ops::update_status(&db, runner.id, "offline").await
                    {
                        tracing::error!(runner_id = runner.id, error = %e, "Failed to mark runner offline");
                    }

                    // Reset jobs assigned to this offline runner
                    if let Err(e) =
                        rg_db::ops::pipeline_ops::reset_runner_jobs(&db, runner.id).await
                    {
                        tracing::error!(runner_id = runner.id, error = %e, "Failed to reset jobs for offline runner");
                    }
                }
                if !offline.is_empty() {
                    tracing::info!(
                        count = offline.len(),
                        "Runner watchdog: marked {} runners offline",
                        offline.len()
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %e, "Runner watchdog: failed to find offline runners");
            }
        }
    }
}
