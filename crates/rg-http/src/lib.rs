//! IronForge HTTP server implementation using Axum.
//!
//! Provides:
//!  - Git Smart HTTP protocol endpoints (`/git/...`)
//!  - REST API (`/api/v1/...`)
//!  - Health check (`/health`)
//!  - TLS/HTTPS support (rustls)
//!  - API pagination

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

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Extension, Query, State};
use axum::http::{header, HeaderValue, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, patch, post, put};
use axum::Router;
use rg_core::package_registry::oci::OciStorage;
use sea_orm::DatabaseConnection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tower_http::cors::CorsLayer;
use tower_http::limit::RequestBodyLimitLayer;

// gix is used via `gix::open()` etc. — crate available at crate root
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

/// Build a restrictive CORS layer.
///
/// If `IRONFORGE_CORS_ORIGINS` is set (comma-separated URLs), only those
/// origins are allowed. Otherwise, all origins are reflected (for development
/// convenience) with a warning logged.
///
/// Replaces `CorsLayer::permissive()` — restricts allowed methods and headers
/// to only what IronForge needs.
fn build_cors_layer() -> CorsLayer {
    use std::time::Duration;

    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::OPTIONS,
    ];

    let headers_list = [header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT];

    match std::env::var("IRONFORGE_CORS_ORIGINS") {
        Ok(origins_str) if !origins_str.is_empty() => {
            let origins: Vec<HeaderValue> = origins_str
                .split(',')
                .map(|s| s.trim())
                .filter(|s| !s.is_empty())
                .filter_map(|s| HeaderValue::from_str(s).ok())
                .collect();

            if origins.is_empty() {
                tracing::warn!(
                    "IRONFORGE_CORS_ORIGINS set but no valid origins parsed — CORS disabled"
                );
                CorsLayer::new()
                    .allow_methods(methods)
                    .allow_headers(headers_list)
            } else {
                tracing::info!(origins = ?origins, "CORS: allowing configured origins");
                CorsLayer::new()
                    .allow_origin(origins)
                    .allow_methods(methods)
                    .allow_headers(headers_list)
                    .allow_credentials(true)
                    .max_age(Duration::from_secs(3600))
            }
        }
        _ => {
            tracing::warn!(
                "IRONFORGE_CORS_ORIGINS not set — CORS allows all origins (not recommended for production)"
            );
            CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::mirror_request())
                .allow_methods(methods)
                .allow_headers(headers_list)
                .allow_credentials(true)
                .max_age(Duration::from_secs(3600))
        }
    }
}

/// Shared application state injected into every Axum handler via `State<AppState>`.
#[derive(Clone)]
pub struct AppState {
    pub repo_root: Arc<PathBuf>,
    pub db: DatabaseConnection,
    pub jwt_secret: Arc<String>,
    pub docker_enabled: bool,
    pub external_runners: bool,
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
    /// Whether Docker runner is enabled for CI jobs.
    pub docker_enabled: bool,
    /// Whether to use external runners instead of embedded runner for CI.
    pub external_runners: bool,
    /// Rate limit: max requests per window (0 = disabled).
    pub rate_limit_max: u32,
    /// Rate limit: window duration in seconds.
    pub rate_limit_window_secs: u64,
    /// Proxy source IPs whose forwarding headers are trusted for rate limiting.
    pub rate_limit_trusted_proxies: Vec<IpAddr>,
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
    /// CI engine implementation (M-14: injected from rg-cli, decouples rg-http from rg-ci).
    pub ci_engine: Arc<dyn rg_core::ci::CiTrigger + Send + Sync>,
}

/// Start the HTTP server and run forever.
pub async fn run(config: HttpServerConfig) -> Result<()> {
    let rate_limiter = rate_limit::RateLimiter::with_trusted_proxies(
        config.rate_limit_max,
        config.rate_limit_window_secs,
        config.rate_limit_trusted_proxies,
    );
    rate_limiter.spawn_cleanup_task();

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

    let state = AppState {
        repo_root: Arc::new(config.repo_root),
        db: config.db,
        jwt_secret: Arc::new(config.jwt_secret),
        docker_enabled: config.docker_enabled,
        external_runners: config.external_runners,
        rate_limiter: rate_limiter.clone(),
        notification_hub: notification_hub.clone(),
        smtp_config: config.smtp_config,
        blob_storage,
        oci_storage,
        log_write_queue: rg_core::ci::log_write_queue::LogWriteQueue::spawn(log_queue_db),
        external_url: config.external_url,
        job_timeout_secs: config.job_timeout_secs,
        ci_engine: config.ci_engine,
    };

    let app = create_router(state.clone(), rate_limiter.clone());

    tokio::spawn(api::ci_retention::run_cleanup_loop(state.clone()));

    // Spawn runner watchdog background task
    tokio::spawn(async move {
        run_runner_watchdog(watchdog_db).await;
    });

    tracing::info!("CORS permissive mode active — tighten in production");

    // ── HTTPS mode (axum-server + rustls) ──────────────────
    //
    //
    // To use TLS, you MUST use `axum_server::bind_rustls()` instead.
    //
    // Correct pattern (used below):
    //   let rustls_config = RustlsConfig::from_config(tls_config);
    //   axum_server::bind_rustls(addr, rustls_config).serve(app).await?;
    //
    // Wrong pattern (no TLS support):
    //   let listener = TcpListener::bind(addr).await?;
    //   axum::serve(listener, app).await?;  // ERROR: no TLS!
    // ── HTTPS mode (axum-server + rustls) ──────────────────
    //
    // CRITICAL: Axum TLS requires `axum-server`, NOT `axum::serve()` (踩坑经验 #2)
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
        axum_server::bind_rustls(
            config_clone
                .parse()
                .with_context(|| format!("invalid TLS listen address: {}", config_clone))?,
            rustls_config,
        )
        .serve(app.into_make_service_with_connect_info::<std::net::SocketAddr>())
        .await
        .context("HTTPS server error")?;
    } else {
        // ── HTTP mode ───────────────────────────────────────────────────
        let listener = tokio::net::TcpListener::bind(&config.listen_addr)
            .await
            .with_context(|| format!("failed to bind to {}", config.listen_addr))?;

        tracing::info!(addr = %config.listen_addr, "HTTP server listening");

        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .context("HTTP server error")?;
    }

    Ok(())
}

/// Load TLS certificate and private key, return a rustls ServerConfig.
async fn load_tls_config(
    cert_path: &std::path::Path,
    key_path: &std::path::Path,
) -> Result<Arc<tokio_rustls::rustls::ServerConfig>> {
    use std::io::BufReader;
    use tokio_rustls::rustls::pki_types::CertificateDer;
    use tokio_rustls::rustls::ServerConfig;

    let cert_file = std::fs::File::open(cert_path)
        .with_context(|| format!("failed to open TLS cert: {}", cert_path.display()))?;
    let mut cert_reader = BufReader::new(cert_file);
    let certs: Vec<CertificateDer<'_>> = rustls_pemfile::certs(&mut cert_reader)
        .collect::<Result<Vec<_>, _>>()
        .context("failed to parse TLS certificates")?;

    let key_file = std::fs::File::open(key_path)
        .with_context(|| format!("failed to open TLS key: {}", key_path.display()))?;
    let mut key_reader = BufReader::new(key_file);

    let key = rustls_pemfile::private_key(&mut key_reader)
        .context("failed to parse TLS private key")?
        .ok_or_else(|| anyhow::anyhow!("no private key found in {}", key_path.display()))?;

    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("failed to build TLS server config")?;

    Ok(Arc::new(server_config))
}

/// Create the Axum router for testing (no rate limiter, no static file serving).
pub fn create_router_for_test(state: AppState) -> Router {
    build_test_router(state)
}

/// Create the Axum router (Git + REST API + health).
fn create_router(state: AppState, rate_limiter: rate_limit::RateLimiter) -> Router {
    build_router(state, rate_limiter)
}

/// Shared router builder used by both production and test routers.
///
/// CRITICAL: Axum `nest()` State requirement (踩坑经验 #2)
///
/// All nested routers MUST share the same `State<AppState>` type.
/// If `git_routes` or `api_v1` use a different State type,
/// Axum will reject the route with a compile-time error.
///
/// Correct pattern (used here):
///   let git_routes = Router::new()...with_state(state.clone());
///   let api_v1 = Router::new()...with_state(state.clone());
///   Router::new().nest("/git", git_routes).nest("/api/v1", api_v1)
///
/// Wrong pattern (will not compile):
///   let git_routes = Router::new()...with_state(git_state);  // different type
///   let api_v1 = Router::new()...with_state(api_state);     // different type
///   Router::new().nest("/git", git_routes).nest("/api/v1", api_v1)  // ERROR
fn build_router(state: AppState, rate_limiter: rate_limit::RateLimiter) -> Router {
    let (api_v1, git_routes) = build_routes(&state);
    let v2_routes = build_v2_routes(&state);
    let docs_routes = build_docs_routes(&state);

    Router::new()
        .nest("/git", git_routes)
        // ── Root-level Git Smart HTTP routes (standard git client format) ───
        // Git clients request /{owner}/{repo}.git/info/refs etc.
        // These must be at root level (no /git prefix) for compatibility.
        .route("/{owner}/{repo}/info/refs", get(handle_info_refs))
        .route(
            "/{owner}/{repo}/git-upload-pack",
            post(handle_git_upload_pack),
        )
        .route(
            "/{owner}/{repo}/git-receive-pack",
            post(handle_git_receive_pack),
        )
        .nest("/api/v1", api_v1)
        .nest("/v2", v2_routes)
        .route("/health", get(health))
        .route("/metrics", get(metrics::metrics_handler))
        .merge(docs_routes)
        // Serve SvelteKit static assets if the build directory exists
        .fallback_service(
            // SPA fallback: serve static assets, and for any unmatched path
            // (client-side routes like /login, /dashboard) return index.html
            // with a per-request CSP nonce injected into all <script> tags (H-2).
            ServeDir::new("web/build").fallback(get(spa_index_handler)),
        )
        // ── Middleware layers (order: bottom-up, last .layer() runs first) ──
        .layer(axum::middleware::from_fn(
            middleware::http_metrics_middleware,
        ))
        .layer(axum::middleware::from_fn(
            security::security_headers_middleware,
        ))
        .layer(axum::middleware::from_fn(middleware::request_id_middleware))
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                let request_id = request
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-");
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    uri = %request.uri(),
                    status = tracing::field::Empty,
                    request_id = %request_id,
                )
            },
        ))
        .layer(build_cors_layer())
        .layer(axum::middleware::from_extractor::<
            axum::extract::ConnectInfo<std::net::SocketAddr>,
        >())
        .layer(axum::middleware::from_fn_with_state(
            rate_limiter.clone(),
            rate_limit::rate_limit_middleware,
        ))
        // Maintenance mode check (runs early, before most handlers)
        .layer(axum::middleware::from_fn(
            middleware::maintenance_middleware,
        ))
        .with_state(state)
}

/// Build OCI Distribution v2 routes (Docker/OCI container registry).
fn build_v2_routes(state: &AppState) -> Router<AppState> {
    // 10 GiB body limit for blob upload requests.
    let upload_body_limit = RequestBodyLimitLayer::new(10 * 1024 * 1024 * 1024);

    // Upload sub-router with body size limit
    let upload_routes = Router::new()
        .route("/", post(oci::start_upload))
        .route(
            "/{uuid}",
            patch(oci::chunk_upload).put(oci::complete_upload),
        )
        .layer(upload_body_limit);

    Router::new()
        // API version check
        .route("/", get(oci::api_version_check))
        // Token authentication
        .route("/auth/token", get(oci::get_token))
        // Tags
        .route("/{owner}/{repo}/tags/list", get(oci::list_tags))
        // Manifests
        .route(
            "/{owner}/{repo}/manifests/{reference}",
            get(oci::get_manifest)
                .head(oci::head_manifest)
                .put(oci::put_manifest),
        )
        // Blobs
        .route(
            "/{owner}/{repo}/blobs/{digest}",
            get(oci::get_blob).head(oci::head_blob),
        )
        // Uploads (with body size limit)
        .nest("/{owner}/{repo}/blobs/uploads", upload_routes)
        .with_state(state.clone())
}

/// Build API docs routes with authentication required.
fn build_docs_routes(state: &AppState) -> Router<AppState> {
    Router::new()
        .route("/api-docs/openapi.json", get(openapi_handler))
        .route("/api-docs", get(swagger_ui_root_handler))
        .route("/api-docs/", get(swagger_ui_root_handler))
        .route("/api-docs/{*tail}", get(swagger_ui_handler))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            docs_auth_middleware,
        ))
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            pat_auth_middleware,
        ))
        .with_state(state.clone())
}

/// Build route definitions (shared between production and test routers).
fn build_routes(state: &AppState) -> (Router<AppState>, Router<AppState>) {
    // ── Git Smart HTTP routes ──────────────────────────────────────────────
    let git_routes = Router::new()
        .route("/{owner}/{repo}/info/refs", get(handle_info_refs))
        .route(
            "/{owner}/{repo}/git-upload-pack",
            post(handle_git_upload_pack),
        )
        .route(
            "/{owner}/{repo}/git-receive-pack",
            post(handle_git_receive_pack),
        );

    // Runner routes that require authentication (single middleware layer)
    let runners_auth = Router::new()
        .route("/runners/{id}/heartbeat", post(api::runners::heartbeat))
        .route("/runners/{id}/deregister", post(api::runners::deregister))
        .route("/runners/{id}/jobs/poll", get(api::runners::poll_job))
        .route(
            "/runners/{id}/jobs/{job_id}/start",
            post(api::runners::start_job),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/log",
            post(api::runners::upload_log),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/workspace",
            get(api::runners::download_workspace),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/cache",
            get(api::runners::download_cache)
                .put(api::runners::upload_cache)
                .layer(RequestBodyLimitLayer::new(1024 * 1024 * 1024)),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/finish",
            post(api::runners::finish_job),
        )
        .route(
            "/runners/{id}/jobs/{job_id}/artifacts",
            post(api::artifacts::upload_artifact),
        )
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api::runners::authenticate_runner,
        ))
        .with_state(state.clone());

    // ── REST API routes ───────────────────────────────────────────────────
    let api_v1 = Router::new()
        // Users
        .route("/users/register", post(api::users::register))
        .route("/users/login", post(api::users::login))
        .route("/users/logout", post(api::users::logout))
        .route("/users/me", get(api::users::me))
        .route("/users/forgot-password", post(api::users::forgot_password))
        .route("/users/reset-password", post(api::users::reset_password))
        // PAT
        .route(
            "/users/tokens",
            get(api::users::list_tokens).post(api::users::create_token),
        )
        .route("/users/tokens/{id}", delete(api::users::delete_token))
        // SSH keys
        .route(
            "/users/ssh-keys",
            get(api::ssh_keys::list_ssh_keys).post(api::ssh_keys::create_ssh_key),
        )
        .route(
            "/users/ssh-keys/{id}",
            delete(api::ssh_keys::delete_ssh_key),
        )
        .route(
            "/repos/{owner}/{name}/keys",
            get(api::deploy_keys::list_deploy_keys).post(api::deploy_keys::create_deploy_key),
        )
        .route(
            "/repos/{owner}/{name}/keys/{id}",
            delete(api::deploy_keys::delete_deploy_key),
        )
        // MFA
        .route("/users/mfa/setup", post(api::mfa::setup_mfa))
        .route("/users/mfa/enable", post(api::mfa::enable_mfa))
        .route("/users/mfa/verify", post(api::mfa::verify_mfa))
        .route("/users/mfa/backup", get(api::mfa::get_backup_codes))
        .route("/users/mfa/disable", post(api::mfa::disable_mfa))
        // SSO
        .route("/auth/sso/providers", get(api::sso::list_providers))
        .route("/auth/sso/{slug}", get(api::sso::authorize))
        .route("/auth/sso/{slug}/callback", get(api::sso::callback))
        .route("/auth/sso/{slug}/refresh", post(api::sso::refresh_token))
        .route(
            "/auth/sso/{slug}/unlink",
            delete(api::sso::unlink_oauth_account),
        )
        // Repos
        .route("/repos", post(api::repos::create_repo))
        // Template listing & explore (must be before /repos/{owner} to avoid route conflict)
        .route(
            "/repos/templates/gitignores",
            get(api::repos::list_gitignore_templates),
        )
        .route(
            "/repos/templates/licenses",
            get(api::repos::list_license_templates),
        )
        .route(
            "/repos/templates/readmes",
            get(api::repos::list_readme_templates),
        )
        .route("/repos/templates/labels", get(api::repos::list_label_sets))
        .route("/repos/explore", get(api::repos::explore))
        .route("/repos/{owner}", get(api::repos::list_repos))
        .route("/repos/{owner}/{name}", get(api::repos::get_repo))
        // Milestones (before issues to avoid routing conflicts)
        .route(
            "/repos/{owner}/{name}/milestones",
            get(api::issues::list_milestones).post(api::issues::create_milestone),
        )
        .route(
            "/repos/{owner}/{name}/milestones/{id}",
            get(api::issues::get_milestone)
                .patch(api::issues::update_milestone)
                .delete(api::issues::delete_milestone),
        )
        // Labels
        .route(
            "/repos/{owner}/{name}/labels",
            get(api::labels::list_labels).post(api::labels::create_label),
        )
        .route(
            "/repos/{owner}/{name}/labels/{id}",
            get(api::labels::get_label)
                .patch(api::labels::update_label)
                .delete(api::labels::delete_label),
        )
        // Issues
        .route(
            "/repos/{owner}/{name}/issue_templates",
            get(api::issues::list_issue_templates),
        )
        .route(
            "/repos/{owner}/{name}/issue_config",
            get(api::issues::get_issue_config),
        )
        .route(
            "/repos/{owner}/{name}/issue_config/validate",
            get(api::issues::validate_issue_config),
        )
        .route(
            "/repos/{owner}/{name}/pull_request_template",
            get(api::issues::get_pull_request_template),
        )
        .route(
            "/repos/{owner}/{name}/issues",
            get(api::issues::list_issues).post(api::issues::create_issue),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}",
            get(api::issues::get_issue).patch(api::issues::update_issue),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/labels",
            get(api::issues::get_issue_labels),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/comments",
            get(api::issues::list_comments).post(api::issues::add_comment),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/assets",
            get(api::attachments::list_issue_attachments)
                .post(api::attachments::create_issue_attachment)
                .layer(RequestBodyLimitLayer::new(101 * 1024 * 1024)),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}",
            get(api::attachments::get_issue_attachment)
                .delete(api::attachments::delete_issue_attachment),
        )
        .route(
            "/repos/{owner}/{name}/issues/comments/{comment_id}/assets",
            get(api::attachments::list_issue_comment_attachments)
                .post(api::attachments::create_issue_comment_attachment)
                .layer(RequestBodyLimitLayer::new(101 * 1024 * 1024)),
        )
        .route(
            "/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}",
            get(api::attachments::get_issue_comment_attachment)
                .delete(api::attachments::delete_issue_comment_attachment),
        )
        // Pull Requests
        .route(
            "/repos/{owner}/{name}/pulls",
            get(api::pulls::list_prs).post(api::pulls::create_pr),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}",
            get(api::pulls::get_pr).patch(api::pulls::update_pr),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/assets",
            get(api::attachments::list_pull_request_attachments)
                .post(api::attachments::create_pull_request_attachment)
                .layer(RequestBodyLimitLayer::new(101 * 1024 * 1024)),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}",
            get(api::attachments::get_pull_request_attachment)
                .delete(api::attachments::delete_pull_request_attachment),
        )
        .route(
            "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets",
            get(api::attachments::list_review_comment_attachments)
                .post(api::attachments::create_review_comment_attachment)
                .layer(RequestBodyLimitLayer::new(101 * 1024 * 1024)),
        )
        .route(
            "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}",
            get(api::attachments::get_review_comment_attachment)
                .delete(api::attachments::delete_review_comment_attachment),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/diff",
            get(api::pulls::get_diff),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/merge",
            post(api::pulls::merge_pr),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/auto-merge",
            put(api::pulls::enable_auto_merge).delete(api::pulls::disable_auto_merge),
        )
        .route(
            "/repos/{owner}/{name}/merge-queue",
            get(api::pulls::list_merge_queue),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/merge-queue",
            put(api::pulls::enqueue_merge_queue).delete(api::pulls::cancel_merge_queue),
        )
        // PR Reviews
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviews",
            get(api::reviews::list_reviews).post(api::reviews::submit_review),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviews/{id}",
            get(api::reviews::get_review),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviews/{id}/dismiss",
            post(api::reviews::dismiss_review),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/comments",
            get(api::reviews::list_review_comments).post(api::reviews::create_review_comment),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/timeline",
            get(api::reviews::get_review_timeline),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution",
            patch(api::reviews::set_thread_resolution),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply",
            post(api::reviews::apply_review_suggestion),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/suggestions/apply",
            post(api::reviews::apply_review_suggestions),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviewers",
            get(api::reviews::list_requested_reviewers).post(api::reviews::request_reviewer),
        )
        .route(
            "/repos/{owner}/{name}/pulls/{number}/reviewers/{username}",
            delete(api::reviews::remove_requested_reviewer),
        )
        // Wiki
        .route(
            "/repos/{owner}/{name}/wiki",
            get(api::wiki::list_pages).post(api::wiki::create_page),
        )
        .route(
            "/repos/{owner}/{name}/wiki/{title}",
            get(api::wiki::get_page)
                .patch(api::wiki::update_page)
                .delete(api::wiki::delete_page),
        )
        .route(
            "/repos/{owner}/{name}/wiki/{title}/history",
            get(api::wiki::list_revisions),
        )
        .route(
            "/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}",
            get(api::wiki::get_revision),
        )
        // LFS (body size limit for object uploads: 10 GiB)
        .route(
            "/repos/{owner}/{name}/lfs/objects/batch",
            post(api::lfs::batch),
        )
        .route(
            "/repos/{owner}/{name}/lfs/objects/{oid}",
            get(api::lfs::download_object)
                .put(api::lfs::upload_object)
                .layer(RequestBodyLimitLayer::new(10 * 1024 * 1024 * 1024)),
        )
        // Webhooks
        .route(
            "/repos/{owner}/{name}/hooks",
            get(api::webhooks::list_webhooks).post(api::webhooks::create_webhook),
        )
        .route(
            "/repos/{owner}/{name}/hooks/{id}",
            get(api::webhooks::get_webhook)
                .patch(api::webhooks::update_webhook)
                .delete(api::webhooks::delete_webhook),
        )
        .route(
            "/repos/{owner}/{name}/hooks/{id}/deliveries",
            get(api::webhooks::list_deliveries),
        )
        .route(
            "/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver",
            post(api::webhooks::redeliver),
        )
        // CI/CD Pipelines
        .route(
            "/repos/{owner}/{name}/pipelines",
            get(api::ci::list_pipelines).post(api::ci::trigger_pipeline),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}",
            get(api::ci::get_pipeline),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/retry",
            post(api::ci::retry_pipeline),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/cancel",
            post(api::ci::cancel_pipeline),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}",
            get(api::ci::get_job),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/play",
            post(api::ci::play_job),
        )
        .route(
            "/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve",
            post(api::ci_environments::approve),
        )
        .route(
            "/repos/{owner}/{name}/actions/environments",
            get(api::ci_environments::list).post(api::ci_environments::create),
        )
        .route(
            "/repos/{owner}/{name}/actions/environments/{id}",
            axum::routing::put(api::ci_environments::update).delete(api::ci_environments::delete),
        )
        .route(
            "/ci/oidc/.well-known/openid-configuration",
            get(api::ci_oidc::discovery),
        )
        .route("/ci/oidc/jwks", get(api::ci_oidc::jwks))
        .route("/ci/oidc/token", get(api::ci_oidc::token))
        .route(
            "/repos/{owner}/{name}/actions/retention",
            get(api::ci_retention::get_policy).put(api::ci_retention::update_policy),
        )
        .route(
            "/repos/{owner}/{name}/actions/retention/expired",
            axum::routing::delete(api::ci_retention::cleanup),
        )
        .route(
            "/repos/{owner}/{name}/actions/secrets",
            get(api::ci_secrets::list),
        )
        .route(
            "/repos/{owner}/{name}/actions/secrets/{secret_name}",
            axum::routing::put(api::ci_secrets::put).delete(api::ci_secrets::delete),
        )
        // Repository archive download
        .route(
            "/repos/{owner}/{name}/archive/{archive}",
            get(api::archive::download_archive),
        )
        // Branch Protection
        .route(
            "/repos/{owner}/{name}/branches/protection",
            get(api::branch_protection::list_protections)
                .post(api::branch_protection::create_protection),
        )
        .route(
            "/repos/{owner}/{name}/branches/protection/{id}",
            get(api::branch_protection::get_protection)
                .patch(api::branch_protection::update_protection)
                .delete(api::branch_protection::delete_protection),
        )
        .route(
            "/repos/{owner}/{name}/tags/protection",
            get(api::tag_protection::list).post(api::tag_protection::create),
        )
        .route(
            "/repos/{owner}/{name}/tags/protection/{id}",
            patch(api::tag_protection::update).delete(api::tag_protection::delete),
        )
        // Collaborators
        .route(
            "/repos/{owner}/{name}/collaborators",
            get(api::collaborators::list_collaborators).post(api::collaborators::add_collaborator),
        )
        .route(
            "/repos/{owner}/{name}/collaborators/{id}",
            patch(api::collaborators::update_permission)
                .delete(api::collaborators::remove_collaborator),
        )
        // Repo Content Browsing
        .route(
            "/repos/{owner}/{name}/tree",
            get(api::repo_content::list_tree),
        )
        .route(
            "/repos/{owner}/{name}/blob/{*path}",
            get(api::repo_content::get_blob),
        )
        .route("/repos/{owner}/{name}/log", get(api::repo_content::get_log))
        .route(
            "/repos/{owner}/{name}/branches",
            get(api::repo_content::list_branches),
        )
        .route(
            "/repos/{owner}/{name}/tags",
            get(api::repo_content::list_tags),
        )
        // GPG Signatures
        .route(
            "/repos/{owner}/{name}/commits/{sha}/signature",
            get(api::repo_content::get_commit_signature),
        )
        // File creation/update/deletion
        .route(
            "/repos/{owner}/{name}/contents/{*path}",
            post(api::repo_content::create_or_update_file).delete(api::repo_content::delete_file),
        )
        // Mirror
        .route(
            "/repos/{owner}/{name}/mirror",
            get(api::mirrors::get_mirror)
                .post(api::mirrors::create_mirror)
                .patch(api::mirrors::update_mirror)
                .delete(api::mirrors::delete_mirror),
        )
        .route(
            "/repos/{owner}/{name}/mirror/sync",
            post(api::mirrors::trigger_mirror_sync),
        )
        // Imports (GitHub/GitLab migration)
        .route(
            "/imports",
            post(api::imports::start_import).get(api::imports::list_imports),
        )
        .route(
            "/imports/{id}",
            get(api::imports::get_import_status).delete(api::imports::delete_import),
        )
        // Project Boards
        .route(
            "/repos/{owner}/{name}/boards",
            get(api::boards::list_boards).post(api::boards::create_board),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}",
            get(api::boards::get_board)
                .patch(api::boards::update_board)
                .delete(api::boards::delete_board),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/columns",
            post(api::boards::create_column),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/columns/{col_id}",
            patch(api::boards::update_column).delete(api::boards::delete_column),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards",
            post(api::boards::create_card),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/cards/{card_id}",
            patch(api::boards::update_card).delete(api::boards::delete_card),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move",
            post(api::boards::move_card),
        )
        .route(
            "/repos/{owner}/{name}/boards/{id}/cards/reorder",
            post(api::boards::reorder_cards),
        )
        // Time Tracking
        .route(
            "/repos/{owner}/{name}/issues/{number}/time",
            get(api::time_tracking::list_time_entries).post(api::time_tracking::add_time),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/time/total",
            get(api::time_tracking::total_time),
        )
        .route(
            "/repos/{owner}/{name}/issues/{number}/time/{id}",
            delete(api::time_tracking::delete_time_entry),
        )
        // Commit Statuses
        .route(
            "/repos/{owner}/{name}/statuses/{sha}",
            post(api::repos::create_commit_status),
        )
        .route(
            "/repos/{owner}/{name}/commits/{sha}/statuses",
            get(api::repos::list_commit_statuses),
        )
        .route(
            "/repos/{owner}/{name}/commits/{sha}/status",
            get(api::repos::get_combined_status),
        )
        // Organizations
        .route(
            "/orgs",
            get(api::orgs::list_orgs).post(api::orgs::create_org),
        )
        .route(
            "/orgs/{name}",
            get(api::orgs::get_org)
                .patch(api::orgs::update_org)
                .delete(api::orgs::delete_org),
        )
        .route(
            "/orgs/{name}/members",
            get(api::orgs::list_org_members).post(api::orgs::add_org_member),
        )
        .route(
            "/orgs/{name}/members/{user_id}",
            delete(api::orgs::remove_org_member),
        )
        .route(
            "/orgs/{name}/teams",
            get(api::orgs::list_org_teams).post(api::orgs::create_team),
        )
        .route(
            "/orgs/{name}/teams/{team_id}",
            get(api::orgs::get_team).delete(api::orgs::delete_team),
        )
        .route(
            "/orgs/{name}/teams/{team_id}/members",
            get(api::orgs::list_team_members).post(api::orgs::add_team_member),
        )
        .route(
            "/orgs/{name}/teams/{team_id}/members/{user_id}",
            delete(api::orgs::remove_team_member),
        )
        // Notifications
        .route(
            "/notifications",
            get(api::notifications::list_notifications),
        )
        .route(
            "/notifications/unread-count",
            get(api::notifications::unread_count),
        )
        .route(
            "/notifications/mark-all-read",
            post(api::notifications::mark_all_read),
        )
        .route(
            "/notifications/{id}/read",
            post(api::notifications::mark_read),
        )
        .route(
            "/notifications/{id}",
            delete(api::notifications::delete_notification),
        )
        // Star/Watch
        .route("/repos/{owner}/{name}/star", put(api::repos::star_repo))
        .route(
            "/repos/{owner}/{name}/starred",
            get(api::repos::get_starred_status),
        )
        .route(
            "/repos/{owner}/{name}/stargazers",
            get(api::repos::get_stargazers),
        )
        .route(
            "/repos/{owner}/{name}/watch",
            get(api::repos::get_watch_status)
                .put(api::repos::watch_repo)
                .delete(api::repos::unwatch_repo),
        )
        // Repo Delete (combined with GET)
        .route(
            "/repos/{owner}/{name}",
            delete(api::repos::delete_repo_handler),
        )
        // Releases
        .route(
            "/repos/{owner}/{name}/releases",
            get(api::releases::list_releases).post(api::releases::create_release),
        )
        .route(
            "/repos/{owner}/{name}/releases/{id}",
            get(api::releases::get_release)
                .patch(api::releases::update_release)
                .delete(api::releases::delete_release),
        )
        // Release Assets
        .route(
            "/repos/{owner}/{name}/releases/{release_id}/assets",
            get(api::releases::list_assets).post(api::releases::upload_asset),
        )
        .route(
            "/repos/{owner}/{name}/releases/assets/{asset_id}",
            get(api::releases::get_asset).delete(api::releases::delete_asset),
        )
        .route(
            "/repos/{owner}/{name}/releases/assets/{asset_id}/download",
            get(api::releases::download_asset),
        )
        // Fork
        .route(
            "/repos/{owner}/{name}/fork",
            post(api::repos::fork_repo_handler),
        )
        .route(
            "/repos/{owner}/{name}/forks",
            get(api::repos::list_forks_handler),
        )
        // Transfer
        .route(
            "/repos/{owner}/{name}/transfer",
            post(api::repos::transfer_repo_handler),
        )
        // Package Registry — protocol-specific routes first (before generic catch-all)
        .route(
            "/repos/{owner}/{name}/packages",
            get(api::packages::list_registries),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/publish",
            post(api::packages::publish),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/list",
            get(api::packages::list_packages),
        )
        // Cargo sparse index protocol
        .route(
            "/repos/{owner}/{name}/packages/cargo/index/{pkg}",
            get(api::packages::cargo_sparse_index),
        )
        // npm registry protocol
        .route(
            "/repos/{owner}/{name}/packages/npm/publish",
            post(api::packages::publish_npm),
        )
        .route(
            "/repos/{owner}/{name}/packages/npm/list",
            get(api::packages::list_npm_packages),
        )
        .route(
            "/repos/{owner}/{name}/packages/npm/{pkg_name}",
            get(api::packages::npm_registry_metadata),
        )
        // PyPI Simple Repository API (PEP 503)
        .route(
            "/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}",
            get(api::packages::pypi_simple_index),
        )
        // Maven metadata endpoint
        .route(
            "/repos/{owner}/{name}/packages/maven/{group_id}/{artifact_id}/maven-metadata.xml",
            get(api::packages::maven_metadata),
        )
        // NuGet protocol endpoints
        .route(
            "/repos/{owner}/{name}/packages/nuget/index.json",
            get(api::packages::nuget_service_index),
        )
        .route(
            "/repos/{owner}/{name}/packages/nuget/registration/{id}/index.json",
            get(api::packages::nuget_registration_index),
        )
        .route(
            "/repos/{owner}/{name}/packages/nuget/query",
            get(api::packages::nuget_search),
        )
        // RubyGems protocol endpoints
        .route(
            "/repos/{owner}/{name}/packages/rubygems/api/v1/dependencies",
            get(api::packages::rubygems_dependencies),
        )
        .route(
            "/repos/{owner}/{name}/packages/rubygems/api/v1/gems/{gem_name}",
            get(api::packages::rubygems_gem_info),
        )
        // Helm repository index
        .route(
            "/repos/{owner}/{name}/packages/helm/index.yaml",
            get(api::packages::helm_index),
        )
        // Composer packages.json
        .route(
            "/repos/{owner}/{name}/packages/composer/packages.json",
            get(api::packages::composer_packages_json),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}",
            get(api::packages::get_package),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions",
            get(api::packages::list_versions),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}",
            get(api::packages::get_version).delete(api::packages::delete_version),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank",
            patch(api::packages::yank_version),
        )
        .route(
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}",
            get(api::packages::download_file),
        )
        // CI/CD Runners
        .route("/runners/register", post(api::runners::register))
        .merge(runners_auth)
        .route(
            "/repos/{owner}/{name}/pipelines/{id}/artifacts",
            get(api::artifacts::list_pipeline_artifacts),
        )
        .route("/artifacts/{id}", get(api::artifacts::get_artifact))
        .route(
            "/artifacts/{id}/download",
            get(api::artifacts::download_artifact),
        )
        .route("/artifacts/{id}", delete(api::artifacts::delete_artifact))
        // Admin
        .route("/admin/runners", get(api::runners::list_runners_admin))
        .route(
            "/admin/runners/{id}",
            delete(api::runners::delete_runner_admin),
        )
        .route("/admin/users", get(api::admin::list_users))
        .route("/admin/users/{id}", get(api::admin::get_user))
        .route("/admin/users/{id}", patch(api::admin::update_user))
        .route("/admin/users/{id}", delete(api::admin::delete_user))
        .route("/admin/users/{id}/unlock", post(api::admin::unlock_user))
        .route("/admin/orgs", get(api::admin::list_orgs))
        .route("/admin/orgs/{name}", get(api::admin::get_org))
        .route("/admin/orgs/{name}", delete(api::admin::delete_org))
        // Admin SSO
        .route(
            "/admin/sso/providers",
            get(api::admin::list_sso_providers).post(api::admin::create_sso_provider),
        )
        .route(
            "/admin/sso/providers/{id}",
            get(api::admin::get_sso_provider)
                .patch(api::admin::update_sso_provider)
                .delete(api::admin::delete_sso_provider),
        )
        .route(
            "/admin/sso/providers/{id}/test",
            post(api::admin::test_sso_provider_connection),
        )
        // Audit logs (admin only)
        .route("/admin/audit/logs", get(api::audit::list_audit_logs))
        .route("/admin/audit/logs/{id}", get(api::audit::get_audit_log))
        .route(
            "/admin/login-attempts",
            get(api::audit::list_login_attempts),
        )
        // Admin instance settings
        .route(
            "/admin/settings",
            get(api::admin::get_settings).patch(api::admin::update_settings),
        )
        // Global Search
        .route("/search", get(api::search::search))
        // External CI/CD Webhook
        .route(
            "/repos/{owner}/{name}/webhooks/external/ci",
            post(api::webhooks_external::external_ci_webhook),
        )
        // ── AI Agent endpoints ─────────────────────────────
        .route(
            "/ai/repos/{owner}/{name}/summary",
            get(api::ai::ai_repo_summary),
        )
        .route(
            "/ai/repos/{owner}/{name}/issues",
            get(api::ai::ai_list_issues),
        )
        .route("/ai/repos/{owner}/{name}/prs", get(api::ai::ai_list_prs))
        .route("/ai/repos/{owner}/{name}/tree", get(api::ai::ai_repo_tree))
        .route(
            "/ai/repos/{owner}/{name}/search/code",
            get(api::ai::ai_search_code),
        )
        // .route("/ai/repos/{owner}/{name}/index", post(api::ai::ai_index_repository))  // 暂时注释：Axum Handler trait 问题，改用 CLI 命令
        // WebSocket
        .route("/ws/notifications", get(ws::ws_notifications_handler))
        .route("/ws/job/{job_id}", get(ws::ws_job_log_handler));

    // Accept Personal Access Tokens on the REST API by translating them to a
    // Bearer JWT before the (JWT-only) handlers run.
    let api_v1 = api_v1.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        pat_auth_middleware,
    ));

    (api_v1, git_routes)
}

/// Build the test router (no rate limiter, no static file serving).
fn build_test_router(state: AppState) -> Router {
    let (api_v1, git_routes) = build_routes(&state);
    let v2_routes = build_v2_routes(&state);
    let docs_routes = build_docs_routes(&state);

    Router::new()
        .nest("/git", git_routes)
        .route("/{owner}/{repo}/info/refs", get(handle_info_refs))
        .route(
            "/{owner}/{repo}/git-upload-pack",
            post(handle_git_upload_pack),
        )
        .route(
            "/{owner}/{repo}/git-receive-pack",
            post(handle_git_receive_pack),
        )
        .nest("/api/v1", api_v1)
        .nest("/v2", v2_routes)
        .merge(docs_routes)
        .route("/health", get(health))
        // ── Middleware layers (no rate limiter for tests) ──────────────────
        .layer(axum::middleware::from_fn(middleware::request_id_middleware))
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                let request_id = request
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-");
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    uri = %request.uri(),
                    status = tracing::field::Empty,
                    request_id = %request_id,
                )
            },
        ))
        .layer(build_cors_layer())
        .with_state(state)
}

/// H-2: SPA fallback handler — serves `index.html` with a per-request CSP
/// nonce injected into all `<script>` tags.
///
/// The nonce is generated by `security_headers_middleware` and stored in
/// request extensions. This handler reads it and injects `nonce="<value>"`
/// into every `<script>` tag so the browser allows inline scripts under
/// the strict `script-src 'self' 'nonce-<value>'` CSP.
async fn spa_index_handler(Extension(nonce): Extension<security::CspNonce>) -> Response {
    match tokio::fs::read("web/build/index.html").await {
        Ok(html_bytes) => {
            let html = String::from_utf8_lossy(&html_bytes).into_owned();
            let modified = security::inject_csp_nonce(&html, &nonce.0);
            (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "text/html; charset=utf-8")],
                modified,
            )
                .into_response()
        }
        Err(_) => (StatusCode::NOT_FOUND, "index.html not found").into_response(),
    }
}

async fn health(State(state): State<AppState>) -> impl IntoResponse {
    use sea_orm::{ConnectionTrait, Statement};

    let mut checks = serde_json::Map::new();

    // DB ping
    let db_ok = state
        .db
        .execute(Statement::from_string(
            state.db.get_database_backend(),
            "SELECT 1".to_string(),
        ))
        .await
        .is_ok();
    checks.insert(
        "database".to_string(),
        serde_json::json!(if db_ok { "ok" } else { "error" }),
    );

    // Filesystem check
    let fs_ok = state.repo_root.exists() && state.repo_root.read_dir().is_ok();
    checks.insert(
        "filesystem".to_string(),
        serde_json::json!(if fs_ok { "ok" } else { "error" }),
    );

    // Prometheus registry check
    let metrics_ok = crate::metrics::REGISTRY.get().is_some();
    checks.insert(
        "metrics".to_string(),
        serde_json::json!(if metrics_ok { "ok" } else { "not_initialized" }),
    );

    // Git availability check (gateway already validates git --version at init)
    let git_ok = rg_git::cli_gateway::global_gateway().is_ok();
    checks.insert(
        "git".to_string(),
        serde_json::json!(if git_ok { "ok" } else { "error" }),
    );

    // SMTP connectivity check (TCP connect with timeout)
    let smtp_ok = if let Some(ref smtp) = state.smtp_config {
        match tokio::time::timeout(
            std::time::Duration::from_secs(3),
            tokio::net::TcpStream::connect((smtp.host.as_str(), smtp.port)),
        )
        .await
        {
            Ok(Ok(_)) => true,
            Ok(Err(_)) | Err(_) => false,
        }
    } else {
        true // skipped if not configured
    };
    checks.insert(
        "smtp".to_string(),
        serde_json::json!(if smtp_ok { "ok" } else { "error" }),
    );

    // Overall: all critical checks must pass
    let overall = if db_ok && fs_ok && git_ok && smtp_ok {
        "ok"
    } else if db_ok || fs_ok {
        "degraded"
    } else {
        "unhealthy"
    };
    let status_code = if db_ok && fs_ok && git_ok && smtp_ok {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };

    (
        status_code,
        axum::Json(serde_json::json!({
            "status": overall,
            "version": env!("CARGO_PKG_VERSION"),
            "phase": 22,
            "checks": checks,
        })),
    )
}

/// GET /api-docs/openapi.json — serve the OpenAPI specification.
async fn openapi_handler() -> impl IntoResponse {
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        openapi::openapi_spec(),
    )
}

/// GET /api-docs/{*tail} — serve Swagger UI static files.
async fn swagger_ui_handler(axum::extract::Path(tail): axum::extract::Path<String>) -> Response {
    let path = if tail.is_empty() { "/" } else { &tail };
    swagger_ui_response(path)
}

/// GET /api-docs[/] — serve Swagger UI index.
async fn swagger_ui_root_handler() -> Response {
    swagger_ui_response("/")
}

fn swagger_ui_response(path: &str) -> Response {
    let config = openapi::swagger_config();

    match utoipa_swagger_ui::serve(path, config) {
        Ok(Some(file)) => (
            StatusCode::OK,
            [(header::CONTENT_TYPE, file.content_type)],
            file.bytes.to_vec(),
        ),
        Ok(None) => (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain".to_string())],
            "Not Found".as_bytes().to_vec(),
        ),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain".to_string())],
            format!("Swagger UI error: {e}").into_bytes(),
        ),
    }
    .into_response()
}

/// API docs endpoint access requires an authenticated JWT/PAT bearer token.
async fn docs_auth_middleware(
    State(state): State<AppState>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    if api::auth::extract_bearer_claims(req.headers(), &state.jwt_secret).is_none() {
        let mut response =
            (StatusCode::UNAUTHORIZED, "api docs requires authentication").into_response();
        if let Ok(challenge) = HeaderValue::from_str("Bearer") {
            response
                .headers_mut()
                .insert(header::WWW_AUTHENTICATE, challenge);
        }
        return response;
    }
    next.run(req).await
}

/// Resolve a Personal Access Token, honouring expiry.
async fn resolve_pat(
    db: &DatabaseConnection,
    token: &str,
) -> Option<rg_db::entities::access_token::Model> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let hash = format!("{:x}", hasher.finalize());

    let tok = rg_db::ops::token_ops::find_by_hash(db, &hash)
        .await
        .ok()??;
    if let Some(expires_at) = tok.expires_at {
        if expires_at < chrono::Utc::now() {
            return None; // expired
        }
    }
    Some(tok)
}

/// Scope required when a PAT is used for a REST request.
///
/// A scope only controls which API family the token may enter. Normal user,
/// repository and administrator authorization is still enforced by handlers.
fn required_pat_scope(path: &str) -> Option<&'static str> {
    if path.starts_with("/api-docs") {
        return None;
    }
    // Axum may expose either the original URI or the path with the nested
    // `/api/v1` prefix stripped, depending on which router layer is running.
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    if path.starts_with("/admin") || path == "/runners/register" {
        return Some("admin");
    }
    if path.starts_with("/users")
        || path.starts_with("/notifications")
        || path.starts_with("/auth")
        || path == "/ws/notifications"
    {
        return Some("user");
    }
    Some("repo")
}

/// Axum middleware: translate a valid Personal Access Token into an equivalent
/// `Authorization: Bearer <JWT>` so the JWT-only API handlers accept PATs.
///
/// Requests already carrying a valid JWT, or no credentials, pass through
/// unchanged. The PAT may be presented as `Bearer <pat>`, `token <pat>`, or
/// HTTP Basic auth (`user:pat`).
async fn pat_auth_middleware(
    State(state): State<AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let required_scope = required_pat_scope(req.uri().path());
    match pat_to_bearer_jwt(&state, req.headers(), required_scope).await {
        Ok(Some(jwt)) => {
            if let Ok(value) = format!("Bearer {jwt}").parse() {
                req.headers_mut().insert(header::AUTHORIZATION, value);
            }
        }
        Ok(None) => {}
        Err(()) => {
            return error::AppError::forbidden("personal access token scope denied").into_response()
        }
    }
    next.run(req).await
}

/// If the Authorization header carries a valid PAT (and not already a valid
/// JWT), return a freshly-minted JWT for the token's owner. Returns the
/// existing JWT for a Basic-auth request that carries one. Otherwise `None`.
async fn pat_to_bearer_jwt(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    required_scope: Option<&str>,
) -> Result<Option<String>, ()> {
    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(None);
    };

    let candidates: Vec<String> = if let Some(t) = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("token "))
    {
        // A valid JWT bearer needs no translation.
        if rg_core::auth::jwt::validate_token(t, &state.jwt_secret).is_some() {
            return Ok(None);
        }
        vec![t.to_string()]
    } else if let Some(encoded) = auth.strip_prefix("Basic ") {
        use base64::Engine as _;
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return Ok(None);
        };
        let Ok(creds) = String::from_utf8(decoded) else {
            return Ok(None);
        };
        let Some((user, pass)) = creds.split_once(':') else {
            return Ok(None);
        };
        // Token may be in either the password (`user:token`) or username field.
        [pass, user]
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|c| c.to_string())
            .collect()
    } else {
        return Ok(None);
    };

    for cand in candidates {
        // A JWT carried via Basic auth: pass it through as a Bearer token.
        if rg_core::auth::jwt::validate_token(&cand, &state.jwt_secret).is_some() {
            return Ok(Some(cand));
        }
        if let Some(pat) = resolve_pat(&state.db, &cand).await {
            if required_scope
                .is_some_and(|scope| !rg_core::auth::pat_scope::has_scope(&pat.scopes, scope))
            {
                return Err(());
            }
            let username = rg_db::ops::user_ops::find_by_id(&state.db, pat.user_id)
                .await
                .ok()
                .flatten()
                .map(|u| u.username)
                .unwrap_or_default();
            if let Ok(jwt) =
                rg_core::auth::jwt::generate_token(pat.user_id, &username, &state.jwt_secret, 1)
            {
                return Ok(Some(jwt));
            }
        }
    }
    Ok(None)
}

/// Extract the authenticated user id from a git-over-HTTP request.
///
/// Supports both JWT session tokens and Personal Access Tokens (PATs),
/// presented either as `Authorization: Bearer <token>` or HTTP Basic auth
/// (`git clone https://user:<token>@host/...`). Returns `None` for anonymous
/// access (public repos still work; private repos are then rejected).
async fn extract_actor_id(
    db: &DatabaseConnection,
    headers: &axum::http::HeaderMap,
    jwt_secret: &str,
) -> Option<i64> {
    let auth_str = headers.get(header::AUTHORIZATION)?.to_str().ok()?;

    if let Some(token) = auth_str.strip_prefix("Bearer ") {
        // JWT session token first, then fall back to a PAT.
        if let Some(claims) = rg_core::auth::jwt::validate_token(token, jwt_secret) {
            return claims.sub.parse().ok();
        }
        return resolve_pat(db, token)
            .await
            .filter(|pat| rg_core::auth::pat_scope::has_scope(&pat.scopes, "repo"))
            .map(|pat| pat.user_id);
    }

    if let Some(encoded) = auth_str.strip_prefix("Basic ") {
        use base64::Engine as _;
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(encoded)
            .ok()?;
        let credentials = String::from_utf8(decoded).ok()?;
        let (username, password) = credentials.split_once(':')?;
        // Git clients carry the token in either the password (`user:token`) or
        // the username (`token:x-oauth-basic`) field — try both, JWT then PAT.
        for candidate in [password, username] {
            if candidate.is_empty() {
                continue;
            }
            if let Some(claims) = rg_core::auth::jwt::validate_token(candidate, jwt_secret) {
                return claims.sub.parse().ok();
            }
            if let Some(pat) = resolve_pat(db, candidate).await {
                if rg_core::auth::pat_scope::has_scope(&pat.scopes, "repo") {
                    return Some(pat.user_id);
                }
            }
        }
    }

    None
}

/// Check repository access for git protocol.
///
/// - upload-pack (clone/fetch): can_read
/// - receive-pack (push): can_write
///
/// Returns Ok(()) if access is granted, or an error response.
///
/// When access is denied for an **anonymous** request (no valid credentials),
/// responds `401 Unauthorized` with a `WWW-Authenticate: Basic` challenge so
/// that `git` prompts for credentials (a username + PAT). An **authenticated**
/// actor that simply lacks permission gets `403 Forbidden`.
async fn check_git_access(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    actor_id: Option<i64>,
    require_write: bool,
) -> Result<(), (StatusCode, [(header::HeaderName, &'static str); 1], String)> {
    let access = if require_write {
        rg_core::repo::service::can_write(db, owner, repo_name, actor_id).await
    } else {
        rg_core::repo::service::can_read(db, owner, repo_name, actor_id).await
    };

    match access {
        Ok(true) => Ok(()),
        // Anonymous + denied → 401 with a Basic challenge so git prompts for
        // credentials. The String body carries the default text/plain type.
        Ok(false) if actor_id.is_none() => Err((
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"IronForge\"")],
            "authentication required".to_string(),
        )),
        // Authenticated but lacking permission → 403.
        Ok(false) => Err((
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "text/plain")],
            "access denied".to_string(),
        )),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            format!("repository not found: {}", e),
        )),
    }
}

/// Git Smart HTTP `/info/refs` endpoint.
///
/// CRITICAL: Content-Type handling (踩坑经验 #6)
///
/// The Git Smart HTTP protocol is VERY sensitive to Content-Type headers.
/// Incorrect Content-Type will cause `git` client to silently fail or
/// report "fatal: protocol error: bad line length character".
///
/// Correct Content-Types:
/// - info/refs response:
///   - upload-pack: `application/x-git-upload-pack-advertisement`
///   - receive-pack: `application/x-git-receive-pack-advertisement`
/// - request body (POST):
///   - upload-pack: `application/x-git-upload-pack-request`
///   - receive-pack: `application/x-git-receive-pack-request`
/// - response body (POST):
///   - upload-pack: `application/x-git-upload-pack-result`
///   - receive-pack: `application/x-git-receive-pack-result`
///
/// Common mistake: Using `text/plain` or wrong subtype will break git clients.
/// Always verify Content-Type matches the Git Smart HTTP spec exactly.
///
/// Strip `.git` suffix from repo name so both `owner/repo.git` and
/// `owner/repo` path formats resolve to the same bare repository.
fn strip_git_suffix(repo: &str) -> String {
    repo.strip_suffix(".git")
        .map(|s| s.to_string())
        .unwrap_or_else(|| repo.to_string())
}

async fn handle_info_refs(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    // Strip .git suffix so both `owner/repo.git` and `owner/repo` work
    let repo = strip_git_suffix(&repo);
    let service = params.get("service").map(|s| s.as_str()).unwrap_or("");

    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            e.to_string(),
        );
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            e.to_string(),
        );
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "repository not found".to_string(),
        );
    }

    // Extract actor from auth header
    let actor_id = extract_actor_id(&state.db, &headers, &state.jwt_secret).await;
    let require_write = service == "git-receive-pack";

    // Check access
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, require_write).await {
        return resp;
    }

    // Check if client wants Protocol V2
    let wants_v2 = git_v2::wants_protocol_v2(&headers);

    match service {
        "git-upload-pack" | "git-receive-pack" => {
            if wants_v2 {
                // Protocol V2: send capability advertisement (refs sent via ls-refs command)
                return match build_v2_capability_advertisement() {
                    Ok(data) => {
                        let content_type = if service == "git-upload-pack" {
                            "application/x-git-upload-pack-advertisement"
                        } else {
                            "application/x-git-receive-pack-advertisement"
                        };
                        (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data)
                    }
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [(header::CONTENT_TYPE, "text/plain")],
                        format!("error: {:#}", e),
                    ),
                };
            }

            // Protocol V1: send full ref advertisement
            let content_type = if service == "git-upload-pack" {
                "application/x-git-upload-pack-advertisement"
            } else {
                "application/x-git-receive-pack-advertisement"
            };

            match build_info_refs(&repo_path, service) {
                Ok(data) => (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain")],
                    format!("error: {:#}", e),
                ),
            }
        }
        _ => (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            "invalid or missing service parameter".to_string(),
        ),
    }
}

fn build_info_refs(repo_path: &std::path::Path, service: &str) -> Result<String> {
    let mut buf = String::new();
    // 踩坑 #1: # service= 行必须用 pkt-line 包裹，不能裸写
    let svc_line = format!("# service={}\n", service);
    buf.push_str(&format!("{:04x}", svc_line.len() + 4));
    buf.push_str(&svc_line);
    buf.push_str("0000");

    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    // Get all references (like git for-each-ref)
    let references = repo.references()?;
    let mut ref_list: Vec<(String, String)> = references
        .all()?
        .filter_map(|r| r.ok())
        .filter_map(|r| {
            let oid = r.target().try_id()?.to_owned();
            let name = String::from_utf8_lossy(r.name().as_bstr()).to_string();
            Some((oid.to_string(), name))
        })
        .collect();

    // Get HEAD SHA
    let head_sha = if let Some(head) = repo.head().ok() {
        head.try_into_referent() // Returns Option<Reference>
            .and_then(|r| r.target().try_id().map(|id| id.to_string()))
    } else {
        None
    };

    if let Some(sha) = &head_sha {
        ref_list.insert(0, (sha.clone(), "HEAD".to_string()));
    }

    let caps = if service == "git-upload-pack" {
        "multi_ack_detailed no-done side-band-64k thin-pack ofs-delta agent=ironforge/0.1"
    } else {
        "report-status report-status-v2 side-band-64k agent=ironforge/0.1"
    };

    if let Some((sha, refname)) = ref_list.first() {
        // First ref line carries capabilities after NUL separator
        // Format: <SHA> <refname>\0<capabilities>\n
        let payload = format!("{} {}\0{}\n", sha, refname, caps);
        let line = format!("{:04x}", payload.len() + 4);
        buf.push_str(&line);
        buf.push_str(&payload);
    } else {
        // Empty repository: send a dummy HEAD line with capabilities
        // Format: <null SHA> HEAD\0<capabilities>\n
        let null_sha = "0000000000000000000000000000000000000000";
        let payload = format!("{} HEAD\0{}\n", null_sha, caps);
        let line = format!("{:04x}", payload.len() + 4);
        buf.push_str(&line);
        buf.push_str(&payload);
    }

    for (sha, refname) in ref_list.iter().skip(1) {
        let payload = format!("{} {}\n", sha, refname);
        let line = format!("{:04x}", payload.len() + 4);
        buf.push_str(&line);
        buf.push_str(&payload);
    }

    buf.push_str("0000");
    Ok(buf)
}

/// Build Protocol V2 capability advertisement.
/// Format: version 2 + capabilities + flush
/// Manual pkt-line construction to avoid async in sync context.
fn build_v2_capability_advertisement() -> Result<String> {
    use std::io::Write;

    let mut buf = Vec::new();

    // 踩坑: Smart HTTP 要求 # service= 行用 pkt-line 包裹 + flush
    let svc_line = format!("# service=git-upload-pack\n");
    let len = svc_line.len() + 4;
    write!(buf, "{:04x}", len)?;
    buf.extend_from_slice(svc_line.as_bytes());
    buf.extend_from_slice(b"0000");

    // Helper to write pkt-line data (踩坑: pkt-line payload 末尾带 \n,
    // 长度头 = payload.len() + 4(头) + 1(\n); 用 write! 不是 writeln!)
    let write_pkt = |buf: &mut Vec<u8>, text: &str| {
        let payload = text.as_bytes();
        let len = payload.len() + 4 + 1; // +4 for hex header, +1 for trailing \n
        write!(buf, "{:04x}{}\n", len, text)?;
        Ok::<(), std::io::Error>(())
    };

    // Protocol version line
    write_pkt(&mut buf, "version 2")?;
    for capability in rg_git::protocol::v2::ADVERTISED_CAPABILITIES {
        write_pkt(&mut buf, capability)?;
    }

    // Flush packet
    buf.extend_from_slice(b"0000");

    Ok(String::from_utf8(buf)?)
}

async fn handle_git_upload_pack(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    // Strip .git suffix so both `owner/repo.git` and `owner/repo` work
    let repo = strip_git_suffix(&repo);
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(e.to_string()),
        )
            .into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(e.to_string()),
        )
            .into_response();
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
            Body::from("repository not found"),
        )
            .into_response();
    }

    // Check read access
    let actor_id = extract_actor_id(&state.db, &headers, &state.jwt_secret).await;
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, false).await {
        return (resp.0, resp.1, Body::from(resp.2)).into_response();
    }

    // Check if client wants Protocol V2
    let wants_v2 = git_v2::wants_protocol_v2(&headers);

    if wants_v2 {
        // Protocol V2: use V2 handler
        let (pipe_read, mut pipe_write) = tokio::io::duplex(body.len() + 1024);
        tokio::spawn(async move {
            let _ = pipe_write.write_all(&body).await;
        });

        let (buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
        // Spawn concurrent reader to prevent duplex deadlock when pack > 64KB
        let reader_task = tokio::spawn(async move {
            let mut buf_reader = buf_reader;
            let mut output = Vec::new();
            let _ = buf_reader.read_to_end(&mut output).await;
            output
        });

        match rg_git::protocol::v2::handle_v2_http(&repo_path, pipe_read, &mut buf_writer).await {
            Ok(()) => {
                let _ = buf_writer.flush().await;
                drop(buf_writer);
                let output = reader_task.await.unwrap_or_default();
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
                    Body::from(output),
                )
                    .into_response()
            }
            Err(e) => {
                drop(buf_writer);
                reader_task.abort();
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain")],
                    Body::from(format!("error: {:#}", e)),
                )
                    .into_response()
            }
        }
    } else {
        // Protocol V1: use V1 handler
        let (pipe_read, mut pipe_write) = tokio::io::duplex(body.len() + 1024);
        tokio::spawn(async move {
            let _ = pipe_write.write_all(&body).await;
        });

        let (buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
        // Spawn concurrent reader to prevent duplex deadlock when pack > 64KB
        let reader_task = tokio::spawn(async move {
            let mut buf_reader = buf_reader;
            let mut output = Vec::new();
            let _ = buf_reader.read_to_end(&mut output).await;
            output
        });

        match rg_git::protocol::upload_pack::handle_upload_pack_http(
            &repo_path,
            pipe_read,
            &mut buf_writer,
        )
        .await
        {
            Ok(()) => {
                let _ = buf_writer.flush().await;
                drop(buf_writer);
                let output = reader_task.await.unwrap_or_default();
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
                    Body::from(output),
                )
                    .into_response()
            }
            Err(e) => {
                drop(buf_writer);
                reader_task.abort();
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain")],
                    Body::from(format!("error: {:#}", e)),
                )
                    .into_response()
            }
        }
    }
}

async fn handle_git_receive_pack(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // Strip .git suffix so both `owner/repo.git` and `owner/repo` work
    let repo = strip_git_suffix(&repo);
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(e.to_string()),
        );
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(e.to_string()),
        );
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            [(
                header::CONTENT_TYPE,
                "application/x-git-receive-pack-result",
            )],
            Body::from("repository not found"),
        );
    }

    // Check write access
    let actor_id = extract_actor_id(&state.db, &headers, &state.jwt_secret).await;
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, true).await {
        return (resp.0, resp.1, Body::from(resp.2));
    }

    let repo_model = match find_repo_by_name(&state.db, &owner, &repo).await {
        Ok(Some(repo_model)) => repo_model,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from("repository not found"),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from(format!("failed to load repository: {:#}", e)),
            );
        }
    };
    let protection_rules =
        match rg_db::ops::protected_branch_ops::list_by_repo(&state.db, repo_model.id).await {
            Ok(rules) => rules,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(
                        header::CONTENT_TYPE,
                        "application/x-git-receive-pack-result",
                    )],
                    Body::from(format!("failed to load branch protections: {:#}", e)),
                );
            }
        };
    let tag_protection_rules =
        match rg_db::ops::protected_tag_ops::list_by_repo(&state.db, repo_model.id).await {
            Ok(rules) => rules,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(
                        header::CONTENT_TYPE,
                        "application/x-git-receive-pack-result",
                    )],
                    Body::from(format!("failed to load tag protections: {:#}", e)),
                );
            }
        };

    let (pipe_read, mut pipe_write) = tokio::io::duplex(body.len() + 1024);
    tokio::spawn(async move {
        let _ = pipe_write.write_all(&body).await;
    });

    let (buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
    // Spawn concurrent reader to prevent duplex deadlock when response > 64KB
    let reader_task = tokio::spawn(async move {
        let mut buf_reader = buf_reader;
        let mut output = Vec::new();
        let _ = buf_reader.read_to_end(&mut output).await;
        output
    });

    let require_signed_refs = signed_commit_required_refs(&protection_rules);
    let mut rejected_refs = branch_protection_rejected_refs(protection_rules, actor_id);
    rejected_refs.extend(tag_protection_rejected_refs(tag_protection_rules, actor_id));
    match rg_git::protocol::receive_pack::handle_receive_pack_http_with_rejections(
        &repo_path,
        pipe_read,
        &mut buf_writer,
        rejected_refs,
        require_signed_refs,
    )
    .await
    {
        Ok(ref_updates) => {
            let _ = buf_writer.flush().await;
            drop(buf_writer);
            let output = reader_task.await.unwrap_or_default();

            // ── Post-push hooks: trigger CI + Webhook ───────────────
            let db = state.db.clone();
            let repo_path_clone = repo_path.clone();
            let repo_root = state.repo_root.clone();
            let owner_clone = owner.clone();
            let repo_clone = repo.clone();
            let docker_enabled = state.docker_enabled;
            let external_runners = state.external_runners;
            let jwt_secret = state.jwt_secret.clone();
            let hub = state.notification_hub.clone();
            let smtp = state.smtp_config.clone();
            let ci_engine = state.ci_engine.clone();
            let external_url = state.external_url.clone();

            tokio::spawn(async move {
                post_push_hooks(
                    &PostPushParams {
                        db: &db,
                        repo_path: &repo_path_clone,
                        repo_root: &repo_root,
                        owner: &owner_clone,
                        repo_name: &repo_clone,
                        docker_enabled,
                        external_runners,
                        jwt_secret: &jwt_secret,
                        notification_hub: &hub,
                        smtp_config: &smtp,
                        ci_engine: &*ci_engine,
                        external_url: external_url.as_deref(),
                    },
                    &ref_updates,
                )
                .await;
            });

            (
                StatusCode::OK,
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from(output),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(format!("error: {:#}", e)),
        ),
    }
}

/// Parameters for post-push hooks.
struct PostPushParams<'a> {
    db: &'a DatabaseConnection,
    repo_path: &'a std::path::Path,
    repo_root: &'a std::path::Path,
    owner: &'a str,
    repo_name: &'a str,
    docker_enabled: bool,
    external_runners: bool,
    jwt_secret: &'a str,
    notification_hub: &'a ws::NotificationHub,
    smtp_config: &'a Option<rg_core::email::SmtpConfig>,
    ci_engine: &'a dyn rg_core::ci::CiTrigger,
    external_url: Option<&'a str>,
}

fn branch_protection_rejected_refs(
    protections: Vec<rg_db::entities::protected_branch::Model>,
    actor_id: Option<i64>,
) -> Vec<(String, String)> {
    protections
        .into_iter()
        .filter_map(|protection| {
            if direct_push_allowed_by_rule(&protection, actor_id) {
                return None;
            }

            let message = if protection.require_pr {
                format!(
                    "push to protected branch '{}' is not allowed; open a pull request instead",
                    protection.branch_name
                )
            } else if !protection.allow_force_push {
                format!(
                    "force push to protected branch '{}' is not allowed",
                    protection.branch_name
                )
            } else {
                return None;
            };

            Some((format!("refs/heads/{}", protection.branch_name), message))
        })
        .collect()
}

fn direct_push_allowed_by_rule(
    protection: &rg_db::entities::protected_branch::Model,
    actor_id: Option<i64>,
) -> bool {
    if let Some(uid) = actor_id {
        if let Some(allowed_json) = &protection.allowed_push_user_ids {
            if let Ok(allowed_ids) = serde_json::from_str::<Vec<i64>>(allowed_json) {
                if allowed_ids.contains(&uid) {
                    return true;
                }
            }
        }
    }

    false
}

fn signed_commit_required_refs(
    protections: &[rg_db::entities::protected_branch::Model],
) -> Vec<String> {
    protections
        .iter()
        .filter(|rule| rule.require_signed_commits)
        .map(|rule| format!("refs/heads/{}", rule.branch_name))
        .collect()
}

fn tag_protection_rejected_refs(
    protections: Vec<rg_db::entities::protected_tag::Model>,
    actor_id: Option<i64>,
) -> Vec<(String, String)> {
    protections
        .into_iter()
        .filter_map(|protection| {
            let allowed = actor_id.is_some_and(|uid| {
                protection
                    .allowed_user_ids
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<i64>>(json).ok())
                    .is_some_and(|ids| ids.contains(&uid))
            });
            (!allowed).then(|| {
                (
                    format!("refs/tags/{}", protection.pattern),
                    format!(
                        "creation or update of protected tag pattern '{}' is not allowed",
                        protection.pattern
                    ),
                )
            })
        })
        .collect()
}

/// Post-push hook: trigger CI pipeline and webhook for push events.
async fn post_push_hooks(
    params: &PostPushParams<'_>,
    ref_updates: &[rg_git::protocol::receive_pack::RefUpdate],
) {
    let PostPushParams {
        db,
        repo_path,
        repo_root,
        owner,
        repo_name,
        docker_enabled,
        external_runners,
        jwt_secret,
        notification_hub,
        smtp_config,
        ci_engine,
        external_url,
    } = params;

    // Find repo_id from DB
    let repo_model = find_repo_by_name(db, owner, repo_name).await;

    let (repo_id, repo_owner_id) = match repo_model {
        Ok(Some(r)) => (r.id, r.owner_id),
        _ => {
            tracing::warn!(owner = %owner, repo = %repo_name, "Post-push: repo not found in DB, skipping hooks");
            return;
        }
    };

    for update in ref_updates {
        if update.status != "ok" {
            continue;
        }

        tracing::info!(
            refname = %update.refname,
            new_sha = %update.new_sha,
            "Post-push: triggering hooks"
        );

        // 0. Branch protection audit: log if push targets a protected branch
        if let Some(branch_name) = update.refname.strip_prefix("refs/heads/") {
            if !update.new_sha.chars().all(|character| character == '0') {
                match rg_db::ops::pull_request_ops::update_open_head_sha(
                    db,
                    repo_id,
                    branch_name,
                    &update.new_sha,
                )
                .await
                {
                    Ok(_) => {
                        if let Err(error) = rg_core::pull_request::try_auto_merges_for_head_commit(
                            db,
                            repo_root,
                            repo_id,
                            &update.new_sha,
                        )
                        .await
                        {
                            tracing::warn!(%error, "auto-merge evaluation after push failed");
                        }
                        if let Err(error) =
                            rg_core::pull_request::merge_queue::process_for_head_commit_with_ci(
                                db,
                                repo_root,
                                repo_id,
                                &update.new_sha,
                                &rg_core::pull_request::merge_queue::MergeQueueCi {
                                    trigger: params.ci_engine,
                                    docker_enabled: params.docker_enabled,
                                    external_runners: params.external_runners,
                                    jwt_secret: Some(params.jwt_secret),
                                    external_url: params.external_url,
                                },
                            )
                            .await
                        {
                            tracing::warn!(%error, "merge queue evaluation after push failed");
                        }
                    }
                    Err(error) => {
                        tracing::warn!(%error, "failed to refresh PR head SHA after push")
                    }
                }
            }
            match rg_db::ops::protected_branch_ops::find_by_repo_and_branch(
                db,
                repo_id,
                branch_name,
            )
            .await
            {
                Ok(Some(_protection)) => {
                    tracing::info!(
                        branch = %branch_name,
                        "Post-push: protected branch update accepted by pre-receive rules"
                    );
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to check branch protection");
                }
                _ => {}
            }
        }

        // 1. Trigger CI pipeline if .ironforge-ci.yml exists
        if ci_engine.has_ci_config(repo_path, &update.new_sha) {
            match ci_engine
                .trigger_pipeline(rg_core::ci::TriggerPipelineParams {
                    db,
                    repo_path,
                    repo_id,
                    commit_sha: &update.new_sha,
                    ref_name: &update.refname,
                    trigger_type: "push",
                    triggered_by: None,
                    docker_enabled: *docker_enabled,
                    external_runners: *external_runners,
                    jwt_secret: Some(jwt_secret),
                    external_url: *external_url,
                })
                .await
            {
                Ok(pipeline_id) => {
                    tracing::info!(pipeline_id, "CI pipeline triggered");

                    // Push real-time notification to repo owner
                    ws::push_notification(
                        notification_hub,
                        repo_owner_id,
                        "ci_triggered",
                        serde_json::json!({
                            "pipeline_id": pipeline_id,
                            "repo": format!("{}/{}", owner, repo_name),
                            "ref": update.refname,
                            "commit": update.new_sha,
                        }),
                    );

                    // Send email notification if SMTP is configured
                    if let Some(smtp) = smtp_config {
                        if let Ok(Some(owner_user)) =
                            rg_db::ops::user_ops::find_by_id(db, repo_owner_id).await
                        {
                            let subject = format!(
                                "[IronForge] CI pipeline #{} triggered for {}/{}",
                                pipeline_id, owner, repo_name
                            );
                            let body = format!(
                                "A CI pipeline has been triggered for repository {}/{} on branch {}.<br/><br/>Commit: {}<br/>Pipeline ID: {}",
                                owner, repo_name, update.refname, update.new_sha, pipeline_id
                            );
                            if let Err(e) = rg_core::email::send_html_notification(
                                smtp,
                                &owner_user.email,
                                &subject,
                                &body,
                                None,
                            )
                            .await
                            {
                                tracing::warn!(error = %e, "Failed to send CI notification email");
                            }
                        }
                    }
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to trigger CI pipeline");
                }
            }
        }

        // 2. Trigger push webhook
        let payload = serde_json::json!({
            "ref": update.refname,
            "before": update.old_sha,
            "after": update.new_sha,
            "repository": {
                "owner": owner,
                "name": repo_name,
            },
        });

        if let Err(e) =
            rg_core::webhook::service::trigger_event(db, repo_id, "push", &payload).await
        {
            tracing::warn!(error = %e, "Failed to trigger push webhook");
        }

        // 3. Trigger branch/tag-specific webhooks
        if let Some(branch_name) = update.refname.strip_prefix("refs/heads/") {
            if update.old_sha.is_empty()
                || update.old_sha == "0000000000000000000000000000000000000000"
            {
                // New branch created
                if let Err(e) =
                    rg_core::webhook::service::trigger_branch_created(db, repo_id, branch_name)
                        .await
                {
                    tracing::warn!(
                        "Failed to trigger branch.created webhook for {}: {e}",
                        branch_name
                    );
                }
            } else if update.new_sha.is_empty()
                || update.new_sha == "0000000000000000000000000000000000000000"
            {
                // Branch deleted
                if let Err(e) =
                    rg_core::webhook::service::trigger_branch_deleted(db, repo_id, branch_name)
                        .await
                {
                    tracing::warn!(
                        "Failed to trigger branch.deleted webhook for {}: {e}",
                        branch_name
                    );
                }
            }
        } else if let Some(tag_name) = update.refname.strip_prefix("refs/tags/") {
            if update.old_sha.is_empty()
                || update.old_sha == "0000000000000000000000000000000000000000"
            {
                // New tag created
                if let Err(e) =
                    rg_core::webhook::service::trigger_tag_created(db, repo_id, tag_name).await
                {
                    tracing::warn!(
                        "Failed to trigger tag.created webhook for {}: {e}",
                        tag_name
                    );
                }
            } else if update.new_sha.is_empty()
                || update.new_sha == "0000000000000000000000000000000000000000"
            {
                // Tag deleted
                if let Err(e) =
                    rg_core::webhook::service::trigger_tag_deleted(db, repo_id, tag_name).await
                {
                    tracing::warn!(
                        "Failed to trigger tag.deleted webhook for {}: {e}",
                        tag_name
                    );
                }
            }
        }

        // Push real-time notification for push event
        ws::push_notification(
            notification_hub,
            repo_owner_id,
            "push",
            serde_json::json!({
                "repo": format!("{}/{}", owner, repo_name),
                "ref": update.refname,
                "commit": update.new_sha,
            }),
        );
    }
}

/// Find a repository by owner name (user or org) and repo name (DB lookup).
async fn find_repo_by_name(
    db: &DatabaseConnection,
    owner: &str,
    name: &str,
) -> anyhow::Result<Option<rg_db::entities::repository::Model>> {
    rg_core::repo::service::find_repo_by_owner_name(db, owner, name).await
}

// ── Runner Watchdog ───────────────────────────────────────

/// Background task that periodically checks for:
/// 1. Stuck jobs (assigned/running for too long) → reset to pending
/// 2. Offline runners (no heartbeat) → mark as offline
async fn run_runner_watchdog(db: DatabaseConnection) {
    // Wait for server to fully start
    tokio::time::sleep(std::time::Duration::from_secs(10)).await;

    loop {
        // Check every 60 seconds
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;

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
