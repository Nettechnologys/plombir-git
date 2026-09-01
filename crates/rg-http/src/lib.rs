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
pub mod route_table;
pub mod security;
pub mod ws;

mod body_limit;
mod git_http;
mod handlers;
mod http_stream;
// Public for the same reason `route_table` is: `required_pat_scope` states
// which token family a route belongs to, and the only way to check that
// statement against the levels the route table declares is for a test to be
// able to call it. Everything else in the module stays `pub(crate)`.
pub mod pat_auth;
mod routes;

use std::net::IpAddr;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use axum::Router;
use rg_core::package_registry::oci::OciStorage;
use sea_orm::DatabaseConnection;

/// Default directory holding the built SPA bundle, relative to the server CWD.
pub const DEFAULT_SPA_BUILD_DIR: &str = handlers::WEB_BUILD_DIR;
/// Default maximum size of one decoded package artifact (512 MiB).
pub const DEFAULT_PACKAGE_UPLOAD_MAX_BYTES: usize = 512 * 1024 * 1024;

/// Shared application state injected into every Axum handler via `State<AppState>`.
#[derive(Clone)]
pub struct AppState {
    pub repo_root: Arc<PathBuf>,
    /// Maximum decoded package artifact size. Protocol envelopes receive only
    /// bounded encoding headroom; this remains the stored-file ceiling.
    pub package_upload_max_bytes: usize,
    /// Directory holding the built SPA bundle. Production uses
    /// [`DEFAULT_SPA_BUILD_DIR`]; tests can inject a temp fixture without
    /// mutating process-global cwd or env.
    pub spa_build_dir: Arc<PathBuf>,
    pub db: DatabaseConnection,
    /// Secret that signs and verifies session JWTs, PAT-derived tokens, CI job
    /// tokens and the short-lived sealed states (passkey ceremonies, SSO).
    pub jwt_secret: Arc<String>,
    /// Secret that encrypts data at rest: TOTP secrets, CI secrets, mirror and
    /// LDAP passwords, SSO client secrets, OAuth tokens.
    ///
    /// **Never reach for `jwt_secret` when you mean this one.** They were the
    /// same value until card_d740512de0a8, and that is exactly the bug: an
    /// operator told to rotate a leaked signing secret silently re-keyed the
    /// whole database, and found out one 500 at a time. `encryption_key`
    /// defaults to `jwt_secret` so existing data stays readable, but it is a
    /// separate knob and rotating either one alone is now a safe operation.
    pub encryption_key: Arc<String>,
    /// This instance's long-lived Ed25519 identity: it signs release provenance
    /// attestations and backs the CI OIDC JWKS.
    ///
    /// **Not derived from `jwt_secret`** — it used to be, and rotating the
    /// signing secret then changed the instance's public identity behind
    /// everyone's back: stored DSSE envelopes stopped verifying and the
    /// published `kid` changed under external verifiers (card_3aecf3708ebe).
    /// It is loaded from the database at startup, so it survives every later
    /// change to either secret; see [`rg_core::auth::instance_key`].
    pub instance_key: Arc<rg_core::auth::instance_key::InstanceKey>,
    /// Optional shared secret for verifying HMAC-SHA256 signatures on *inbound*
    /// external webhooks (defense-in-depth on `/webhooks/external/*`). `None`
    /// (the default) disables signature checking; the endpoints then rely on
    /// JWT/PAT auth alone.
    pub external_webhook_secret: Option<Arc<String>>,
    pub docker_enabled: bool,
    pub external_runners: bool,
    /// Whether imageless CI jobs may run as a shell on the host (default false).
    pub allow_host_runner: bool,
    /// Whether `POST /users/register` accepts new accounts from outside.
    /// Defaults to [`RegistrationMode::Open`] — the historical behaviour — and
    /// is the *only* switch that closes it: `[rate_limit].auth_max` throttles
    /// registration spam but never refuses it. LDAP/SSO auto-provision is a
    /// separate channel this does not touch; see
    /// [`rg_core::user::registration`].
    pub registration: rg_core::user::registration::RegistrationMode,
    /// Exact operator-approved private origins for repository imports. Empty
    /// keeps every user-supplied source behind the normal SSRF guard.
    pub trusted_import_origins: rg_core::import::trust::TrustedImportOrigins,
    /// Exact plaintext HTTP origins allowed to receive import credentials.
    /// Independent from private-origin SSRF trust.
    pub import_transport_policy: rg_core::import::trust::ImportTransportPolicy,
    /// Operator-owned exception for plaintext HTTP mirror transport.
    /// Create/update and every manual/background sync receive the same policy.
    pub mirror_transport_policy: rg_core::mirror::transport::MirrorTransportPolicy,
    /// Operator-owned exception for plaintext outbound webhook transport.
    /// Secure by default; create/update validate explicitly and detached
    /// delivery re-checks the process-wide published copy.
    pub webhook_transport_policy: rg_core::webhook::transport::WebhookTransportPolicy,
    /// Per-process cancellation edge for imports started by this server.
    pub import_workers: rg_core::import::service::ImportWorkerRegistry,
    pub notification_hub: ws::NotificationHub,
    pub smtp_config: Option<rg_core::email::SmtpConfig>,
    /// Backend-neutral durable object storage.
    pub blob_storage: Arc<dyn rg_core::blob_storage::BlobStorage>,
    pub oci_storage: Arc<OciStorage>,
    pub log_write_queue: rg_core::ci::log_write_queue::LogWriteQueue,
    /// Tracker for detached delivery/post-push work owned by this app state.
    pub delivery_tracker: rg_core::task_tracker::TaskTracker,
    /// External-facing base URL for SSO callbacks (None = detect from request).
    pub external_url: Option<String>,
    /// CI job timeout in seconds.
    pub job_timeout_secs: u64,
    /// Wall-clock timeout (seconds) for streaming git operations
    /// (upload-pack / receive-pack). On elapse the `git` subprocess is killed
    /// (via `kill_on_drop`) and the handler returns 504. 0 disables the bound.
    pub git_stream_timeout_secs: u64,
    /// Idle timeout (seconds) for buffering a git **request** body: if no body
    /// frame arrives within this window the buffer aborts with 504. This is the
    /// HTTP transport's slow-drip defense, layered on top of the wall-clock
    /// bound (axum buffers the whole body before the handler runs, so the guard
    /// lives at the buffering step). 0 disables it. Default 30.
    pub git_idle_timeout_secs: u64,
    /// CI engine (M-14: trait object decouples rg-http from rg-ci).
    pub ci_engine: Arc<dyn rg_core::ci::CiTrigger + Send + Sync>,
    /// Whether opt-in Ed25519 provenance attestation of release assets is
    /// enabled. Default `false`: the sign/verify endpoints return 404 until an
    /// operator turns the feature on.
    pub attestation_enabled: bool,
    /// How often an already-open WebSocket re-checks that it may still be open,
    /// in seconds — see [`ws::DEFAULT_WS_SESSION_RECHECK_SECS`], which is the
    /// value every production instance uses. A field rather than a constant so
    /// a test can drive the re-check without sleeping through a live interval.
    pub ws_session_recheck_secs: u64,
    /// This instance's admin-toggled switches (maintenance mode, banner),
    /// backed by the `instance_settings` row in `db`. Lazily loaded, so
    /// `Default::default()` is the correct value at every construction site.
    pub instance_settings: instance::InstanceSettingsCache,
}

impl AppState {
    /// Run every post-push hook for refs this process just moved, detached
    /// through the delivery tracker.
    ///
    /// **Every path that advances a ref must call this**, not just the git
    /// transports. A commit is a commit regardless of what wrote it: the web
    /// editor's `POST /contents/*` moves `refs/heads/<branch>` exactly like a
    /// `git push` does, and until card_13202be354ac it ran none of the
    /// automation — no CI pipeline, no `push` webhook, no open-PR head-SHA
    /// refresh, so merge-queue and auto-merge kept deciding on a commit that
    /// was no longer the head. The same defect had already been fixed twice for
    /// the two git transports (card_b4fefeee8abf); this helper exists so the
    /// next caller inherits the wiring instead of hand-rolling a fourth copy.
    ///
    /// Detached because the client already holds its response, but **tracked**:
    /// `delivery_tracker()` is what the shutdown drain in [`run`] awaits, so a
    /// SIGTERM in the next few seconds cannot sever the work silently
    /// (card_8d4148774f32).
    pub fn spawn_post_push_hooks(
        &self,
        repo_path: PathBuf,
        owner: String,
        repo_name: String,
        pusher_id: Option<i64>,
        ref_updates: Vec<rg_git::protocol::receive_pack::RefUpdate>,
    ) {
        if ref_updates.is_empty() {
            return;
        }

        let db = self.db.clone();
        let context = self.post_push_context();

        self.delivery_tracker.spawn(async move {
            context
                .run(&db, &repo_path, &owner, &repo_name, pusher_id, &ref_updates)
                .await;
        });
    }

    /// This process's post-push wiring, in the owned form the hook runs take.
    pub fn post_push_context(&self) -> rg_core::push_hooks::PostPushContext {
        rg_core::push_hooks::PostPushContext {
            repo_root: self.repo_root.to_path_buf(),
            docker_enabled: self.docker_enabled,
            external_runners: self.external_runners,
            allow_host_runner: self.allow_host_runner,
            jwt_secret: Some(self.jwt_secret.to_string()),
            encryption_key: Some(self.encryption_key.to_string()),
            smtp_config: self.smtp_config.clone(),
            ci_engine: self.ci_engine.clone(),
            external_url: self.external_url.clone(),
            notifier: Some(Arc::new(self.notification_hub.clone())),
            delivery_tracker: self.delivery_tracker.clone(),
        }
    }

    /// Run the post-push hooks for the base-branch moves merges just made.
    ///
    /// A merge advances `refs/heads/<base>` exactly like a `git push` does, so
    /// it owes the same automation — a pipeline on the merge commit, the `push`
    /// webhook, the watch fan-out. `rg-core` performs the merge and *reports*
    /// the move (it has no CI engine or hub of its own); running it is this
    /// layer's job, and every path here that can merge — REST merge, enabling
    /// auto-merge, the merge queue, a review approval, a finished CI job —
    /// hands its moves to this one seam.
    pub fn spawn_merge_push_hooks(
        &self,
        actor_id: Option<i64>,
        merged: Vec<rg_core::pull_request::MergedRef>,
    ) {
        self.post_push_context()
            .spawn_for_merged_refs(&self.db, actor_id, merged);
    }

    /// Evaluate the merges a commit that just became a head unblocks, and run
    /// the hooks for whatever branches they moved.
    ///
    /// The pair belongs together: until card_73a1ec5b32f3 the callers that ran
    /// the evaluation (a finished pipeline, an applied suggestion) threw the ref
    /// moves away, so the most common auto-merge of all — "CI went green, the PR
    /// went in" — landed a merge commit on `main` that no automation ever saw.
    pub async fn evaluate_merges_and_spawn_hooks(
        &self,
        source_repo_id: i64,
        commit_sha: &str,
        actor_id: Option<i64>,
    ) {
        self.post_push_context()
            .evaluate_merges_and_spawn_hooks(&self.db, source_repo_id, commit_sha, actor_id)
            .await;
    }

    /// Trigger the `pull_request` pipeline for a PR that just opened or was
    /// reopened, detached through the delivery tracker.
    ///
    /// The push transports reach the same producer through the post-push hooks
    /// (a pushed head branch synchronises its PRs); this is the other half —
    /// the PR itself appearing is an event too, and it is the one an
    /// `on: pull_request` workflow is written for.
    pub fn spawn_pull_request_ci(
        &self,
        pr: rg_db::entities::pull_request::Model,
        actor_id: Option<i64>,
    ) {
        self.post_push_context()
            .spawn_pull_request_ci(&self.db, pr, actor_id);
    }

    /// This process's CI wiring, in the borrowed form a pipeline-triggering
    /// path takes (the merge queue, the pull-request trigger).
    pub fn pipeline_ci(&self) -> rg_core::pull_request::ci::PipelineCi<'_> {
        rg_core::pull_request::ci::PipelineCi {
            trigger: &*self.ci_engine,
            docker_enabled: self.docker_enabled,
            external_runners: self.external_runners,
            allow_host_runner: self.allow_host_runner,
            jwt_secret: Some(&self.jwt_secret),
            encryption_key: Some(&self.encryption_key),
            external_url: self.external_url.as_deref(),
        }
    }
}

/// HTTP server configuration.
pub struct HttpServerConfig {
    /// Address to listen on (e.g., "0.0.0.0:8080").
    pub listen_addr: String,
    /// Root directory for git repositories.
    pub repo_root: PathBuf,
    /// Database connection.
    pub db: DatabaseConnection,
    /// JWT secret key. Signing only — see [`AppState::encryption_key`] for the
    /// key that opens data at rest.
    pub jwt_secret: String,
    /// At-rest encryption key. See [`AppState::encryption_key`].
    pub encryption_key: String,
    /// This instance's Ed25519 provenance identity, already loaded from the
    /// database. See [`AppState::instance_key`].
    pub instance_key: Arc<rg_core::auth::instance_key::InstanceKey>,
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
    /// Whether self-service registration is accepted. See
    /// [`AppState::registration`].
    pub registration: rg_core::user::registration::RegistrationMode,
    /// Exact operator-approved private origins for repository imports.
    pub trusted_import_origins: rg_core::import::trust::TrustedImportOrigins,
    /// Import credential transport policy. Plain HTTP remains disabled unless
    /// its exact origin was separately named by the instance operator.
    pub import_transport_policy: rg_core::import::trust::ImportTransportPolicy,
    /// Outbound mirror transport policy. Plain HTTP remains disabled unless
    /// `[mirror].allow_insecure_http` was explicitly enabled.
    pub mirror_transport_policy: rg_core::mirror::transport::MirrorTransportPolicy,
    /// Outbound webhook transport policy. Plain HTTP remains disabled unless
    /// `[webhooks].allow_insecure_http` was explicitly enabled.
    pub webhook_transport_policy: rg_core::webhook::transport::WebhookTransportPolicy,
    /// Maximum decoded package artifact size in bytes.
    pub package_upload_max_bytes: usize,
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
    /// TLS configuration: (cert_path, key_path). None = HTTP only.
    pub tls_config: Option<(PathBuf, PathBuf)>,
    /// External-facing base URL (e.g., "https://git.example.com").
    /// Used for SSO callbacks and as the stable WebAuthn relying party.
    pub external_url: Option<String>,
    /// CI job timeout in seconds (default: 3600).
    pub job_timeout_secs: u64,
    /// Wall-clock timeout (seconds) for the streaming git transport
    /// (upload-pack / receive-pack). Bounds a hung or pathologically slow `git`
    /// subprocess so it can't hold a connection + process indefinitely. 0
    /// disables the bound (default: 300).
    pub git_stream_timeout_secs: u64,
    /// Idle timeout (seconds) for buffering a git request body — the HTTP
    /// slow-drip defense layered on top of `git_stream_timeout_secs`. 0 disables
    /// it (default: 30).
    pub git_idle_timeout_secs: u64,
    /// CI engine implementation (M-14: injected from rg-cli, decouples rg-http from rg-ci).
    pub ci_engine: Arc<dyn rg_core::ci::CiTrigger + Send + Sync>,
    /// Graceful-shutdown signal. Flips to `true` on SIGTERM/ctrl_c; the server
    /// then stops accepting connections, drains in-flight requests, and lets the
    /// background workers exit cleanly.
    pub shutdown_rx: tokio::sync::watch::Receiver<bool>,
    /// Grace window (seconds) for draining in-flight requests and the CI-log
    /// queue after the shutdown signal fires before the process is forced down.
    pub shutdown_grace_secs: u64,
    /// Enable opt-in Ed25519 provenance attestation of release assets. Default
    /// `false` (feature off; endpoints 404).
    pub attestation_enabled: bool,
    /// WebSocket notification hub to serve clients from. Pass an existing hub
    /// when another transport in the same process must reach the same clients —
    /// the SSH server's post-push hooks push `ci_triggered` / `push` events
    /// through it, and a hub of their own would fan out to nobody. `None`
    /// creates a private hub (the standalone-HTTP default).
    pub notification_hub: Option<ws::NotificationHub>,
    /// Shared instance settings cache. Pass the same handle to the SSH server
    /// when both transports run in one process, so admin updates take effect on
    /// every write path immediately.
    pub instance_settings: rg_core::instance::InstanceSettingsCache,
}

/// Start the HTTP server and run forever.
pub async fn run(config: HttpServerConfig) -> Result<()> {
    run_with_listener(config, None).await
}

/// Start the plain-HTTP server on a listener that is already bound.
///
/// Callers that bind an ephemeral port can publish the listener's actual
/// address without releasing ownership and asking this function to rebind it.
/// TLS still uses `axum_server`'s own acceptor and is therefore deliberately
/// rejected at this boundary.
pub async fn run_on_listener(
    config: HttpServerConfig,
    listener: tokio::net::TcpListener,
) -> Result<()> {
    if config.tls_config.is_some() {
        anyhow::bail!("a pre-bound HTTP listener cannot be used with TLS");
    }
    run_with_listener(config, Some(listener)).await
}

async fn run_with_listener(
    config: HttpServerConfig,
    prebound_listener: Option<tokio::net::TcpListener>,
) -> Result<()> {
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

    // Detached webhook delivery is triggered below rg-http from many domains,
    // so publish the one-instance-per-process policy before any route or
    // background worker can dispatch. Request-time validation also carries the
    // same value in AppState.
    rg_core::webhook::transport::publish(config.webhook_transport_policy);

    if let Err(error) =
        api::passkeys::warn_about_rp_configuration(&config.db, config.external_url.as_deref()).await
    {
        tracing::warn!(error = %format!("{error:#}"), "could not inspect passkey relying-party configuration at startup");
    }

    let notification_hub = config.notification_hub.unwrap_or_default();

    // ── Initialize Prometheus metrics registry ──────────────────
    metrics::init_registry().expect("Failed to initialize Prometheus metrics registry");

    // Forward core-crate events (webhook deliveries fire from a background task,
    // PR merges funnel through the core service across three paths) into
    // Prometheus via the observer hooks, since `rg-core` sits below the recorder.
    rg_core::metrics_hook::set_webhook_delivery_observer(metrics::recorder::webhook_delivery);

    // Same reason, for the one secret a detached task has to open rather than
    // report: a webhook delivery signs with `webhooks.secret_encrypted`, and it
    // is dispatched from too many call sites to be handed the at-rest key as a
    // parameter. `forgekeep serve` publishes it too; this covers a state built
    // without the full boot.
    rg_core::auth::at_rest_key::publish(&config.encryption_key);

    rg_core::metrics_hook::set_pr_merged_observer(metrics::recorder::pr_merged);
    // Issues and pull requests are counted on their repository-local number
    // allocators, which is the only thing the REST create and the import
    // subsystem have in common — the import owns no handler to be metered in,
    // so a migrated tracker used to move none of these four series
    // (card_4f2a72c62d95).
    rg_core::metrics_hook::set_issue_opened_observer(metrics::recorder::issue_opened);
    rg_core::metrics_hook::set_issue_closed_observer(metrics::recorder::issue_closed);
    rg_core::metrics_hook::set_pr_opened_observer(metrics::recorder::pr_opened);
    rg_core::metrics_hook::set_repo_created_observer(metrics::recorder::repo_created);
    rg_core::metrics_hook::set_repo_deleted_observer(metrics::recorder::repo_deleted);
    rg_core::metrics_hook::set_user_provisioned_observer(metrics::recorder::user_provisioned);
    rg_core::metrics_hook::set_db_backup_observer(metrics::recorder::db_backup);

    // The embedded runner settles its own jobs and pipelines inside `rg-ci`,
    // below the recorder. Without these two the whole CI metric family had a
    // single producer sitting on the external-runner path, so a default
    // instance — `ci.external_runners = false` — emitted none of it while its
    // builds ran (card_e309fbb5a3fd).
    rg_core::metrics_hook::set_ci_job_finished_observer(metrics::recorder::ci_job_finished);
    rg_core::metrics_hook::set_ci_pipeline_finished_observer(
        metrics::recorder::ci_pipeline_finished,
    );

    let blob_storage: Arc<dyn rg_core::blob_storage::BlobStorage> = Arc::new(
        rg_core::blob_storage::LocalBlobStorage::new(config.repo_root.clone()),
    );
    // One registry shape, not two. The branch that used to stand here was
    // selected by a `[server]` key inherited from upstream that reached
    // neither `ServerConfig` nor `forgekeep.example.toml`, so `serve` wrote a
    // `None` literal into it and only a test fixture ever produced anything
    // else — while an OCI error told operators to configure it, which
    // `deny_unknown_fields` would have turned into a refused start
    // (card_04cc26cbe976). `repo_root` is the legacy root because that is
    // where a pre-`BlobStorage` instance keeps `<owner>/<repo>/oci/`, next to
    // `<owner>/<repo>.releases`.
    let oci_storage = Arc::new(OciStorage::from_backend(
        blob_storage.clone(),
        config.repo_root.join("_oci_uploads"),
        Some(config.repo_root.clone()),
    ));

    // Clone DB before it moves into state
    let log_queue_db = config.db.clone();

    let (log_write_queue, log_consumer_handle) =
        rg_core::ci::log_write_queue::LogWriteQueue::spawn_with_shutdown(
            log_queue_db,
            shutdown_rx.clone(),
        );

    let state = AppState {
        repo_root: Arc::new(config.repo_root),
        spa_build_dir: Arc::new(PathBuf::from(DEFAULT_SPA_BUILD_DIR)),
        db: config.db,
        jwt_secret: Arc::new(config.jwt_secret),
        encryption_key: Arc::new(config.encryption_key),
        instance_key: config.instance_key,
        external_webhook_secret: config.external_webhook_secret.map(Arc::new),
        docker_enabled: config.docker_enabled,
        external_runners: config.external_runners,
        allow_host_runner: config.allow_host_runner,
        registration: config.registration,
        trusted_import_origins: config.trusted_import_origins,
        import_transport_policy: config.import_transport_policy,
        mirror_transport_policy: config.mirror_transport_policy,
        webhook_transport_policy: config.webhook_transport_policy,
        import_workers: Default::default(),
        package_upload_max_bytes: config.package_upload_max_bytes,
        notification_hub: notification_hub.clone(),
        smtp_config: config.smtp_config,
        blob_storage,
        oci_storage,
        log_write_queue,
        delivery_tracker: rg_core::task_tracker::delivery_tracker().clone(),
        external_url: config.external_url,
        job_timeout_secs: config.job_timeout_secs,
        git_stream_timeout_secs: config.git_stream_timeout_secs,
        git_idle_timeout_secs: config.git_idle_timeout_secs,
        ci_engine: config.ci_engine,
        attestation_enabled: config.attestation_enabled,
        ws_session_recheck_secs: ws::DEFAULT_WS_SESSION_RECHECK_SECS,
        instance_settings: config.instance_settings,
    };

    // The limiters go to the router and nowhere else. `AppState` used to carry a
    // third clone of the global one that nothing ever read — a handler reaching
    // for "the limiter in the state" would have taken an object whose budget
    // nobody spends, i.e. a limit that limits nothing (card_11cba7708615).
    let app = routes::create_router(state.clone(), rate_limiter, auth_rate_limiter);

    tokio::spawn(api::ci_retention::run_cleanup_loop(
        state.clone(),
        shutdown_rx.clone(),
    ));

    // Spawn runner watchdog background task
    {
        let watchdog_state = state.clone();
        let watchdog_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            run_runner_watchdog(watchdog_state, watchdog_shutdown).await;
        });
    }

    // Restart the pipelines the previous process left unfinished. The cutoff is
    // taken *here*, before anything of this process can create a pipeline, so
    // the sweep can never reach a run this server is itself executing — see
    // `recover_interrupted_pipelines`.
    {
        let recovery_state = state.clone();
        let created_before = chrono::Utc::now().naive_utc();
        tokio::spawn(async move {
            recover_interrupted_pipelines(&recovery_state, created_before).await;
        });
    }

    // Spawn the metrics gauge sink (refreshes entity-count gauges + keeps the
    // db-query series warm at idle).
    {
        let sink_db = state.db.clone();
        let sink_shutdown = shutdown_rx.clone();
        tokio::spawn(async move {
            run_metrics_gauge_sink(sink_db, sink_shutdown).await;
        });
    }

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
        let listener = match prebound_listener {
            Some(listener) => listener,
            None => tokio::net::TcpListener::bind(&config.listen_addr)
                .await
                .with_context(|| format!("failed to bind to {}", config.listen_addr))?,
        };
        let bound_addr = listener
            .local_addr()
            .context("failed to read bound HTTP listener address")?;

        tracing::info!(addr = %bound_addr, "HTTP server listening");

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
    // Webhook deliveries (`webhook::service::trigger_event`), WS notifications
    // (`ws::push_notification`) and the post-push hooks of a `receive-pack`
    // (`git_http`) are spawned detached so handlers don't block on them. The
    // shared tracker lets us await the outstanding
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
    if shutdown_rx.changed().await.is_err() {
        // Coordinator dropped: graceful shutdown should still proceed.
    }
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

    let key = PrivateKeyDer::from_pem_reader(&mut key_reader)
        .with_context(|| format!("failed to parse TLS private key in {}", key_path.display()))?;

    let server_config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .context("failed to build TLS server config")?;

    Ok(Arc::new(server_config))
}

/// Create the Axum router for testing: the production route set, stack and
/// fallback, minus the two rate limiters.
pub fn create_router_for_test(state: AppState) -> Router {
    routes::build_test_router(state)
}

/// Create the production router in tests, rate limiters and all.
///
/// Most integration tests use [`create_router_for_test`], which differs from
/// this one only by those limiters. Serve this one with
/// `into_make_service_with_connect_info`: the limiter layer extracts
/// `ConnectInfo`, which a plain `axum::serve` does not supply.
pub fn create_router_for_test_with_static_files(state: AppState) -> Router {
    // Both limiters disabled (`max_requests = 0`), so the layers are mounted —
    // the `ConnectInfo` extractor with them — but never reject anything.
    create_router_for_test_with_rate_limits(
        state,
        rate_limit::RateLimiter::new(0, 60),
        rate_limit::RateLimiter::new(0, 60),
    )
}

/// Create the production router in tests with both rate limiters supplied.
///
/// The two limiters are the only thing production's stack carries that the test
/// router does not (see `routes::apply_middleware`), which makes them the only
/// thing no ordinary integration test can notice going missing. A test that
/// wants to prove they are mounted builds the router here with a budget small
/// enough to spend, and serves it with
/// `into_make_service_with_connect_info::<SocketAddr>()` — the limiter
/// middleware extracts `ConnectInfo`, which the plain harness does not supply.
pub fn create_router_for_test_with_rate_limits(
    state: AppState,
    rate_limiter: rate_limit::RateLimiter,
    auth_rate_limiter: rate_limit::RateLimiter,
) -> Router {
    routes::create_router(state, rate_limiter, auth_rate_limiter)
}

/// The test router plus the declared access level of every route in it.
///
/// The route-access sweep needs both, and needs them to come from one build —
/// a table assembled separately from the router is a table that can drift.
pub fn create_router_for_test_with_routes(
    state: AppState,
) -> (Router, Vec<route_table::RouteFact>) {
    routes::build_test_router_with_facts(state)
}

// ── Metrics gauge sink ────────────────────────────────────

/// Refresh the entity-count gauges once. The count queries run through
/// [`metrics::time_db`], so a slow count also feeds the `db_query_*` series.
async fn refresh_entity_gauges(db: &DatabaseConnection) {
    match metrics::time_db("user.count_active", rg_db::ops::user_ops::count_active(db)).await {
        Ok(n) => metrics::recorder::set_users_total(n as i64),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "metrics gauge sink: count_active failed")
        }
    }
    match metrics::time_db(
        "repo.count_non_deleted",
        rg_db::ops::repo_ops::count_non_deleted(db),
    )
    .await
    {
        Ok(n) => metrics::recorder::set_repos_total(n as i64),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "metrics gauge sink: count_non_deleted failed")
        }
    }
    // `ci_jobs_running` is sampled here rather than summed from start/finish
    // events, so it is right whichever executor the instance runs — see
    // `recorder::set_ci_jobs_running`.
    match metrics::time_db(
        "pipeline.count_running_jobs",
        rg_db::ops::pipeline_ops::count_running_jobs(db),
    )
    .await
    {
        Ok(n) => metrics::recorder::set_ci_jobs_running(n as i64),
        Err(e) => {
            tracing::warn!(error = %format!("{e:#}"), "metrics gauge sink: count_running_jobs failed")
        }
    }
}

/// Background task that periodically refreshes the entity-count gauges
/// (`forgekeep_users`, `forgekeep_repositories`) so the business dashboard shows
/// live totals without every create/delete handler having to recompute them.
/// Runs once immediately at startup, then every 60s until shutdown.
async fn run_metrics_gauge_sink(
    db: DatabaseConnection,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) {
    loop {
        refresh_entity_gauges(&db).await;
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
            _ = shutdown_rx.changed() => {
                tracing::info!("metrics gauge sink received shutdown, stopping");
                break;
            }
        }
    }
}

// ── Runner Watchdog ───────────────────────────────────────

/// Background task that periodically checks for:
/// 1. Stuck jobs (assigned/running for too long) → reset to pending
/// 2. Offline runners (no heartbeat) → mark as offline
async fn run_runner_watchdog(state: AppState, mut shutdown_rx: tokio::sync::watch::Receiver<bool>) {
    let db = state.db.clone();
    // Startup one-shot: recover import tasks orphaned by the *previous* process.
    // Background imports run as detached `tokio::spawn`s, so a restart/crash
    // leaves any in-flight import stuck in a running status forever (its
    // mark_completed/mark_failed never fires). A short grace (30s) skips any
    // import this fresh process might have just started, so we only touch
    // genuine leftovers from the prior run.
    recover_stuck_imports(&db, 30).await;

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
        //
        // This branch converges on its own: `reset_stuck_job` is one statement,
        // and a job whose reset fails keeps both the status and the stale
        // `updated_at` that `find_stuck_jobs` selects on — so the next tick
        // picks it up again sixty seconds later. Nothing here is delegated.
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
                        tracing::error!(job_id, error = %format!("{e:#}"), "Failed to reset stuck job");
                    } else if job.status == "running" {
                        // Count the attempt's outcome. The job goes back to
                        // `pending` and will be attempted again, so this is one
                        // attempt that ended in a timeout, not one job that
                        // ended — which is what `ci_jobs_total` counts.
                        //
                        // It used to also decrement the running gauge, which
                        // was wrong in both directions: the embedded runner
                        // never incremented, so this walked the gauge below
                        // zero, and the bulk reset in `reset_runner_jobs` is
                        // not itemised, so its running jobs were never settled
                        // at all. The gauge is sampled from the rows now
                        // (card_e309fbb5a3fd), so neither asymmetry can reach
                        // it.
                        crate::metrics::recorder::ci_job_finished("timeout", None);
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
                tracing::error!(error = %format!("{e:#}"), "Runner watchdog: failed to find stuck jobs");
            }
        }

        // 2. Mark runners as offline if no heartbeat for 90 seconds
        match rg_db::ops::pipeline_ops::find_offline_runners(&db, 90).await {
            Ok(offline) => {
                let mut retired = 0usize;
                for runner in &offline {
                    tracing::warn!(
                        runner_id = runner.id,
                        name = %runner.name,
                        "Runner watchdog: marking runner as offline"
                    );
                    // Releasing the jobs and marking the runner offline are one
                    // write, not two. Done separately — status first — a failed
                    // reset left the jobs pinned to a runner that
                    // `find_offline_runners` no longer selects, so this branch
                    // never retried it and the rows waited on the ten-minute
                    // stuck-job sweep above instead (card_4d1d8b9fba56). Now
                    // either both land or neither does, and a runner whose
                    // retirement failed is still `online`/`busy` for the next
                    // tick, sixty seconds later.
                    match rg_db::ops::runner_ops::retire_unreachable_runner(&db, runner.id).await {
                        Ok(_) => retired += 1,
                        Err(e) => tracing::error!(
                            runner_id = runner.id,
                            error = %format!("{e:#}"),
                            "Failed to retire an unreachable runner — it keeps its status and \
                             its jobs, and the next watchdog tick will try again"
                        ),
                    }
                }
                // The runners actually retired, not the ones the query offered:
                // the whole point of the pair above is that a failure leaves the
                // runner where it was, and a summary counting the candidates
                // would report that as work done.
                if retired > 0 {
                    tracing::info!(
                        count = retired,
                        candidates = offline.len(),
                        "Runner watchdog: marked {retired} runners offline"
                    );
                }
            }
            Err(e) => {
                tracing::error!(error = %format!("{e:#}"), "Runner watchdog: failed to find offline runners");
            }
        }

        // 3. Restart the pipelines whose embedded runner died in this process.
        //
        // The reset above only renames the row: `pending` means "waiting for an
        // executor", and on an instance with no external runner nobody is
        // waiting to take it. This is the step that produces one.
        recover_abandoned_pipelines(
            &state,
            chrono::Utc::now().naive_utc()
                - chrono::Duration::seconds(ABANDONED_PIPELINE_GRACE_SECS),
        )
        .await;

        // 4. Recover stuck import tasks (running but no update > 10 min).
        // Catches imports orphaned by an in-process spawn death (e.g. a panic
        // inside run_import that never reaches the mark_failed arm) as well as
        // any restart-leftover the startup sweep missed.
        recover_stuck_imports(&db, 600).await;
    }
}

/// Fail every import task stuck in a running status with no update within
/// `older_than_secs`. The DB write is guarded (`fail_stuck` re-checks status +
/// cutoff atomically), so a task that legitimately completes in the meantime is
/// never clobbered. Mirrors the CI stuck-job recovery, but marks imports
/// `failed` rather than resetting them: an import (clone + full-repo metadata)
/// is far too long to safely retry from a grace window, so we surface the
/// interruption to the user instead.
async fn recover_stuck_imports(db: &DatabaseConnection, older_than_secs: i64) {
    let stuck = match rg_db::ops::import_task_ops::find_stuck(db, older_than_secs).await {
        Ok(stuck) => stuck,
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "Import watchdog: failed to find stuck import tasks");
            return;
        }
    };

    let mut failed = 0usize;
    for task in &stuck {
        tracing::warn!(
            import_task_id = task.id,
            status = %task.status,
            "Import watchdog: failing stuck import task (interrupted by restart)"
        );
        match rg_db::ops::import_task_ops::fail_stuck(
            db,
            task.id,
            older_than_secs,
            "import interrupted by server restart",
        )
        .await
        {
            Ok(true) => failed += 1,
            Ok(false) => {} // task advanced/completed between find and fail — leave it
            Err(e) => {
                tracing::error!(
                    import_task_id = task.id,
                    error = %format!("{e:#}"),
                    "Failed to fail stuck import task"
                );
            }
        }
    }

    if failed > 0 {
        tracing::info!(
            count = failed,
            "Import watchdog: failed {failed} stuck import tasks"
        );
    }
}

/// Restart the pipelines a previous process left unfinished.
///
/// `pipeline_jobs.status = 'pending'` means "waiting for an executor". On an
/// instance with `ci.external_runners = false` the only executor is the
/// embedded runner, and it is not a poller: `spawn_internal_runner` walks one
/// pipeline's stages top to bottom and, once its task ends, nothing ever comes
/// back to that pipeline. So a stop mid-build — planned or not — left the
/// pipeline `running` and its job `pending` forever: the UI showed "building",
/// with no success, no failure and no deadline (card_706418977f45). Imports
/// already had this recovery ([`recover_stuck_imports`]); pipelines did not.
///
/// Two things make re-spawning safe rather than a second execution:
///
/// - `created_before` is the instant this process started, so every pipeline
///   here predates this process and has no runner inside it. See
///   [`rg_db::ops::pipeline_ops::find_interrupted_pipelines`].
/// - The runner is written to resume: `run_pipeline` skips settled stages,
///   `run_stage` skips settled jobs, and every write goes through a
///   `settle_*_if_active` that refuses to move a row that already answered.
///
/// A job the dead process was executing is still `running`/`assigned` in the
/// database — its `hand_job_back` never ran, or never got the chance — and
/// `run_stage` refuses to resume from those statuses. So they are handed back
/// to `pending` first, which is the same write the watchdog would have made ten
/// minutes later.
///
/// This is a startup one-shot on purpose, and not a watchdog tick: mid-life
/// there is no way to tell a pipeline whose runner died from one whose runner
/// is still working, and re-spawning the second runs somebody's deploy twice.
pub async fn recover_interrupted_pipelines(
    state: &AppState,
    created_before: chrono::NaiveDateTime,
) {
    if state.external_runners {
        // The producer exists here: a registered runner polls `pending` jobs and
        // will pick these up on its own. Re-spawning an embedded runner beside
        // it would be the double execution this sweep exists to avoid.
        return;
    }

    let pipelines =
        match rg_db::ops::pipeline_ops::find_interrupted_pipelines(&state.db, created_before).await
        {
            Ok(pipelines) => pipelines,
            Err(error) => {
                tracing::error!(
                    error = %format!("{error:#}"),
                    "CI recovery: failed to look for pipelines interrupted by a restart"
                );
                return;
            }
        };
    if pipelines.is_empty() {
        return;
    }

    let mut resumed = 0usize;
    for pipeline in &pipelines {
        match recover_one_pipeline(state, pipeline).await {
            Ok(()) => resumed += 1,
            Err(error) => tracing::error!(
                pipeline_id = pipeline.id,
                repo_id = pipeline.repo_id,
                error = %format!("{error:#}"),
                "CI recovery: could not resume a pipeline interrupted by a restart — it keeps its \
                 status and no runner is holding it"
            ),
        }
    }

    // The pipelines actually handed to a runner, not the ones the query offered:
    // a resume that failed left the pipeline exactly where it was.
    tracing::info!(
        resumed,
        candidates = pipelines.len(),
        "CI recovery: resumed {resumed} pipelines interrupted by a restart"
    );
}

/// How long a pipeline must have existed before this process will conclude that
/// nothing is executing it.
///
/// The claim in [`recover_abandoned_pipelines`] rests on an in-memory lease, and
/// a pipeline row is committed a moment before its runner takes one. That window
/// is sub-second on every path that opens it — `trigger_pipeline` spawns the
/// runner in the same call that published the graph — so ten minutes is not a
/// measurement, it is refusing to be anywhere near the edge. It also keeps this
/// recovery on the same clock as the stuck-job deadline it follows.
pub const ABANDONED_PIPELINE_GRACE_SECS: i64 = 600;

/// Restart the pipelines whose embedded runner died inside this process.
///
/// [`recover_interrupted_pipelines`] covers a restart and nothing else: it is a
/// startup one-shot, and its safety comes from a cutoff — the instant the
/// process began — that has no mid-life equivalent. But the embedded runner can
/// also die *without* the process dying: a panic inside the spawned task, or the
/// error `runner.run()` only logs. The pipeline then keeps its `running` status
/// with nobody inside it, and on an instance with `ci.external_runners = false`
/// there is no second executor to notice — the build waits for the next restart
/// (card_111ac7923d7c).
///
/// The watchdog's stuck-job reset does not fix this. It renames the row to
/// `pending`, which means "waiting for an executor", and the instance this
/// applies to has none; worse, a runner that died *between* jobs leaves no
/// `running` job at all, so that sweep never even looks.
///
/// What makes re-spawning safe mid-life is
/// [`has_embedded_runner`](rg_core::ci::embedded_runners::has_embedded_runner):
/// a pipeline nothing in this process holds a lease on has no runner inside it,
/// whatever the database still shows. The database alone could not answer it —
/// a stale heartbeat is also what a live runner whose writes are failing
/// produces, and starting a second runner beside that one runs somebody's
/// deploy twice.
///
/// Only `pending` / `running` are considered, which is what excludes the one
/// other way a pipeline sits without a runner legitimately: pausing at a gate
/// sets the pipeline's own status to `manual` or `waiting_approval`
/// ([`try_pause_stage_at_manual`](rg_db::ops::pipeline_ops::try_pause_stage_at_manual)),
/// so a pipeline waiting for a person is never mistaken for an abandoned one.
pub async fn recover_abandoned_pipelines(
    state: &AppState,
    unclaimed_before: chrono::NaiveDateTime,
) {
    if state.external_runners {
        // A registered runner polls `pending` and takes the reclaimed job on its
        // own. Spawning an embedded runner beside it is the double execution
        // this recovery exists to avoid.
        return;
    }

    let pipelines =
        match rg_db::ops::pipeline_ops::find_interrupted_pipelines(&state.db, unclaimed_before)
            .await
        {
            Ok(pipelines) => pipelines,
            Err(error) => {
                tracing::error!(
                    error = %format!("{error:#}"),
                    "CI recovery: failed to look for pipelines whose runner died"
                );
                return;
            }
        };

    let mut resumed = 0usize;
    let mut abandoned = 0usize;
    for pipeline in &pipelines {
        if rg_core::ci::embedded_runners::has_embedded_runner(pipeline.id) {
            continue;
        }
        abandoned += 1;
        match recover_one_pipeline(state, pipeline).await {
            Ok(()) => resumed += 1,
            Err(error) => tracing::error!(
                pipeline_id = pipeline.id,
                repo_id = pipeline.repo_id,
                error = %format!("{error:#}"),
                "CI recovery: could not restart a pipeline whose runner died — it keeps its \
                 status and no runner is holding it"
            ),
        }
    }

    // The pipelines actually handed to a runner, not the ones the query offered:
    // a pipeline whose runner is alive was never abandoned, and a resume that
    // failed left the pipeline exactly where it was.
    if abandoned > 0 {
        tracing::warn!(
            resumed,
            abandoned,
            "CI recovery: restarted {resumed} of {abandoned} pipelines whose embedded runner had \
             died"
        );
    }
}

/// Hand one interrupted pipeline back to the embedded runner.
async fn recover_one_pipeline(
    state: &AppState,
    pipeline: &rg_db::entities::pipeline::Model,
) -> Result<()> {
    let jobs = rg_db::ops::pipeline_ops::list_jobs_by_pipeline(&state.db, pipeline.id)
        .await
        .context("list the jobs of an interrupted pipeline")?;
    for job in &jobs {
        if matches!(job.status.as_str(), "assigned" | "running") {
            rg_db::ops::pipeline_ops::hand_back_active_job(&state.db, job.id)
                .await
                .with_context(|| {
                    format!(
                        "hand job {} back after the process that held it stopped",
                        job.id
                    )
                })?;
        }
    }

    let repo = rg_db::ops::repo_ops::find_by_id(&state.db, pipeline.repo_id)
        .await
        .context("load the repository of an interrupted pipeline")?
        .context("repository of an interrupted pipeline no longer exists")?;
    let storage_owner = recovery_storage_owner(state, &repo).await?;
    let repo_path = state
        .repo_root
        .join(format!("{storage_owner}/{}.git", repo.name));

    state
        .ci_engine
        .resume_pipeline(rg_core::ci::ResumePipelineParams {
            db: &state.db,
            repo_path: &repo_path,
            repo_id: repo.id,
            pipeline_id: pipeline.id,
            docker_enabled: state.docker_enabled,
            external_runners: state.external_runners,
            allow_host_runner: state.allow_host_runner,
            jwt_secret: Some(&state.jwt_secret),
            encryption_key: Some(&state.encryption_key),
            external_url: state.external_url.as_deref(),
        })
        .await
        .context("resume an interrupted pipeline")
}

/// The on-disk owner directory of a repository, resolved without a route to
/// read it from: the organization's name for an org repository, otherwise the
/// owning user's username. The handler-side twin is `resolve_repo_storage_owner`
/// in `api::ci`, which takes the owner straight off the URL.
async fn recovery_storage_owner(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
) -> Result<String> {
    if let Some(org_id) = repo.org_id {
        return rg_db::ops::org_ops::get_org(&state.db, org_id)
            .await
            .context("load the organization owning an interrupted pipeline's repository")?
            .map(|org| org.name)
            .context("organization owning an interrupted pipeline's repository no longer exists");
    }
    rg_db::ops::user_ops::find_by_id(&state.db, repo.owner_id)
        .await
        .context("load the user owning an interrupted pipeline's repository")?
        .map(|user| user.username)
        .context("user owning an interrupted pipeline's repository no longer exists")
}

#[cfg(test)]
mod ci_jobs_running_gauge_tests {
    use sea_orm::{NotSet, Set};

    /// A migrated database holding one pipeline with one job, returned with the
    /// job's id. Enough to move a row in and out of `running`, which is all the
    /// gauge reads.
    async fn database_with_one_job() -> (sea_orm::DatabaseConnection, i64) {
        let db =
            rg_db::connect_with_pool("sqlite::memory:", rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
                .await
                .expect("connect the gauge fixture database");
        rg_db::run_migrations(&db)
            .await
            .expect("migrate the gauge fixture database");
        let user = rg_db::ops::user_ops::create_user(
            &db,
            "gauge-owner",
            "gauge-owner@example.test",
            "unused",
            "Gauge Owner",
        )
        .await
        .expect("create the fixture owner");
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            rg_db::entities::repository::ActiveModel {
                id: NotSet,
                owner_id: Set(user.id),
                name: Set("gauge".into()),
                description: Set(None),
                is_private: Set(true),
                default_branch: Set("main".into()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .expect("create the fixture repository");
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            repo.id,
            "0123456789abcdef0123456789abcdef01234567",
            "refs/heads/main",
            "manual",
            Some(user.id),
        )
        .await
        .expect("create the fixture pipeline");
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "test", 0)
            .await
            .expect("create the fixture stage");
        let job = rg_db::ops::pipeline_ops::create_job(
            &db, stage.id, "build", "echo hi", None, None, None, None, None, None, false, None,
            None, None,
        )
        .await
        .expect("create the fixture job");
        (db, job.id)
    }

    fn gauge() -> i64 {
        crate::metrics::ci::JOBS_RUNNING
            .get()
            .expect("the CI gauge is registered")
            .get()
    }

    /// The gauge has to answer for the executor a default instance actually
    /// runs. It used to be summed by hand from the external-runner `start_job`
    /// handler, which meant zero while an embedded build ran — and the watchdog
    /// decremented for jobs nothing had counted, which walks an `IntGauge`
    /// below zero (card_e309fbb5a3fd). Sampling the rows answers for both
    /// executors, because both write the same `running` status.
    #[tokio::test]
    async fn the_gauge_follows_the_running_rows_and_never_goes_below_zero() {
        // The registry is process-global and installing it is idempotent. The
        // gauge below is process-global too, and this stays the only test in
        // this binary that samples or moves it — a second one would need the
        // two to take a lock, the way the registration-counter readers in
        // `api::sso` do (card_00b2bd65060e).
        crate::metrics::init_registry().expect("install the metrics registry");
        let (db, job_id) = database_with_one_job().await;

        super::refresh_entity_gauges(&db).await;
        assert_eq!(gauge(), 0, "no job is running yet");

        rg_db::ops::pipeline_ops::start_job_if_active(
            &db,
            job_id,
            Some(chrono::Utc::now().naive_utc()),
        )
        .await
        .expect("start the fixture job");
        super::refresh_entity_gauges(&db).await;
        assert_eq!(
            gauge(),
            1,
            "a job the embedded runner marked `running` must be visible in the gauge"
        );

        // What the watchdog does to a job whose runner went quiet: count the
        // attempt as a timeout, then hand the row back to `pending`. The count
        // is an event and the gauge is state, so the event must not move the
        // gauge at all — asserted here, BEFORE the next sample, because a
        // sample would paper over any amount of hand-summing in between.
        crate::metrics::recorder::ci_job_finished("timeout", None);
        assert_eq!(
            gauge(),
            1,
            "counting a job's outcome must not move the running gauge — that is \
             the hand-summing whose two halves never matched"
        );

        rg_db::ops::pipeline_ops::reset_stuck_job(&db, job_id)
            .await
            .expect("reset the fixture job the way the watchdog does");
        super::refresh_entity_gauges(&db).await;
        assert_eq!(
            gauge(),
            0,
            "a watchdog reset must return the gauge to zero, not past it"
        );

        // The asymmetry that walked an `IntGauge` negative: outcomes settled
        // for jobs nothing ever counted — every embedded one, and every job of
        // the un-itemised bulk reset in `reset_runner_jobs`.
        crate::metrics::recorder::ci_job_finished("timeout", None);
        crate::metrics::recorder::ci_job_finished("timeout", None);
        assert_eq!(
            gauge(),
            0,
            "settling a job the gauge never counted must not walk it negative"
        );
        super::refresh_entity_gauges(&db).await;
        assert_eq!(gauge(), 0, "and the next sample must still read zero");
    }
}
