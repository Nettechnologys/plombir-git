//! Prometheus metrics endpoint for Plombir Git.
//!
//! Provides `/metrics` endpoint returning metrics in Prometheus text format.
//! Uses the `prometheus` crate (default-features = false to avoid OpenSSL).

use std::sync::OnceLock;

use axum::http::StatusCode;
use axum::response::IntoResponse;
use prometheus::{Registry, TextEncoder};

/// Global Prometheus registry (lazy-initialized).
pub static REGISTRY: OnceLock<Registry> = OnceLock::new();

/// HTTP request metrics.
pub mod http_requests {
    use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, IntGauge, Opts, Registry};
    use std::sync::OnceLock;

    /// Counter: total HTTP requests by method, route, status.
    pub static REQUEST_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Histogram: request duration (seconds) by route.
    ///
    /// Labelled by `route` — the same normalized `MatchedPath` template the
    /// counter uses — because every consumer of this series groups by it:
    /// `SlowRequestDuration` reports `{{ $labels.route }}`, and the latency
    /// panels legend by route. Without the label those queries collapse to one
    /// global series and the alert fires saying "P95 on  exceeds 1s".
    pub static REQUEST_DURATION: OnceLock<HistogramVec> = OnceLock::new();

    /// Gauge: current in-flight requests.
    pub static IN_FLIGHT: OnceLock<IntGauge> = OnceLock::new();

    /// Register all HTTP request metrics with the registry.
    pub fn register(registry: &Registry) -> Result<(), prometheus::Error> {
        let request_count = IntCounterVec::new(
            prometheus::Opts::new("http_requests_total", "Total HTTP requests"),
            &["method", "route", "status"],
        )?;
        REQUEST_COUNT
            .set(request_count.clone())
            .map_err(|_| prometheus::Error::Msg("REQUEST_COUNT already set".into()))?;
        registry.register(Box::new(request_count))?;

        let request_duration = HistogramVec::new(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "HTTP request duration in seconds",
            )
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
            &["route"],
        )?;
        REQUEST_DURATION
            .set(request_duration.clone())
            .map_err(|_| prometheus::Error::Msg("REQUEST_DURATION already set".into()))?;
        registry.register(Box::new(request_duration))?;

        let in_flight = IntGauge::with_opts(Opts::new(
            "http_requests_in_flight",
            "Current in-flight HTTP requests",
        ))?;
        IN_FLIGHT
            .set(in_flight.clone())
            .map_err(|_| prometheus::Error::Msg("IN_FLIGHT already set".into()))?;
        registry.register(Box::new(in_flight))?;

        Ok(())
    }
}

/// Database metrics.
pub mod db {
    use prometheus::{HistogramOpts, HistogramVec, IntCounterVec, Registry};
    use std::sync::OnceLock;

    /// Counter: total database queries by operation.
    pub static QUERY_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Histogram: database query duration (seconds) by operation.
    ///
    /// Same label as the counter next to it: `SlowDatabaseQueries` exists to
    /// answer *which* operation got slow, and the DB latency panel legends by
    /// operation. The label vocabulary is the curated set passed to
    /// [`super::time_db`], so the cardinality is the same one `db_queries_total`
    /// already carries.
    pub static QUERY_DURATION: OnceLock<HistogramVec> = OnceLock::new();

    /// Register all database metrics with the registry.
    pub fn register(registry: &Registry) -> Result<(), prometheus::Error> {
        let query_count = IntCounterVec::new(
            prometheus::Opts::new("db_queries_total", "Total database queries"),
            &["operation"],
        )?;
        QUERY_COUNT
            .set(query_count.clone())
            .map_err(|_| prometheus::Error::Msg("QUERY_COUNT already set".into()))?;
        registry.register(Box::new(query_count))?;

        let query_duration = HistogramVec::new(
            HistogramOpts::new(
                "db_query_duration_seconds",
                "Database query duration in seconds",
            )
            .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]),
            &["operation"],
        )?;
        QUERY_DURATION
            .set(query_duration.clone())
            .map_err(|_| prometheus::Error::Msg("QUERY_DURATION already set".into()))?;
        registry.register(Box::new(query_duration))?;

        Ok(())
    }
}

/// Git operation metrics.
pub mod git {
    use prometheus::{Histogram, HistogramOpts, IntCounterVec, Registry};
    use std::sync::OnceLock;

    /// Counter: total Git operations by type (clone, push, pull).
    pub static OPERATION_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Histogram: Git operation duration (seconds).
    pub static OPERATION_DURATION: OnceLock<Histogram> = OnceLock::new();

    /// Register all Git metrics with the registry.
    pub fn register(registry: &Registry) -> Result<(), prometheus::Error> {
        let operation_count = IntCounterVec::new(
            prometheus::Opts::new("git_operations_total", "Total Git operations"),
            &["operation"],
        )?;
        OPERATION_COUNT
            .set(operation_count.clone())
            .map_err(|_| prometheus::Error::Msg("OPERATION_COUNT already set".into()))?;
        registry.register(Box::new(operation_count))?;

        let operation_duration = Histogram::with_opts(
            HistogramOpts::new(
                "git_operation_duration_seconds",
                "Git operation duration in seconds",
            )
            .buckets(vec![0.1, 0.5, 1.0, 2.5, 5.0, 10.0, 30.0, 60.0, 300.0]),
        )?;
        OPERATION_DURATION
            .set(operation_duration.clone())
            .map_err(|_| prometheus::Error::Msg("OPERATION_DURATION already set".into()))?;
        registry.register(Box::new(operation_duration))?;

        Ok(())
    }
}

/// CI/CD metrics.
pub mod ci {
    use prometheus::{Histogram, HistogramOpts, IntCounterVec, IntGauge, Opts, Registry};
    use std::sync::OnceLock;

    /// Counter: total CI pipelines by status.
    pub static PIPELINE_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Counter: total CI jobs by status.
    pub static JOB_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Gauge: current running jobs.
    pub static JOBS_RUNNING: OnceLock<IntGauge> = OnceLock::new();

    /// Histogram: CI job execution duration (seconds), from runner start to
    /// finish.
    pub static JOB_DURATION: OnceLock<Histogram> = OnceLock::new();

    /// Register all CI metrics with the registry.
    pub fn register(registry: &Registry) -> Result<(), prometheus::Error> {
        let pipeline_count = IntCounterVec::new(
            prometheus::Opts::new("ci_pipelines_total", "Total CI pipelines"),
            &["status"],
        )?;
        PIPELINE_COUNT
            .set(pipeline_count.clone())
            .map_err(|_| prometheus::Error::Msg("PIPELINE_COUNT already set".into()))?;
        registry.register(Box::new(pipeline_count))?;

        let job_count = IntCounterVec::new(
            prometheus::Opts::new("ci_jobs_total", "Total CI jobs"),
            &["status"],
        )?;
        JOB_COUNT
            .set(job_count.clone())
            .map_err(|_| prometheus::Error::Msg("JOB_COUNT already set".into()))?;
        registry.register(Box::new(job_count))?;

        let jobs_running =
            IntGauge::with_opts(Opts::new("ci_jobs_running", "Current running CI jobs"))?;
        JOBS_RUNNING
            .set(jobs_running.clone())
            .map_err(|_| prometheus::Error::Msg("JOBS_RUNNING already set".into()))?;
        registry.register(Box::new(jobs_running))?;

        let job_duration = Histogram::with_opts(
            HistogramOpts::new(
                "ci_job_duration_seconds",
                "CI job execution duration in seconds",
            )
            .buckets(vec![
                1.0, 5.0, 15.0, 30.0, 60.0, 120.0, 300.0, 600.0, 1800.0, 3600.0,
            ]),
        )?;
        JOB_DURATION
            .set(job_duration.clone())
            .map_err(|_| prometheus::Error::Msg("JOB_DURATION already set".into()))?;
        registry.register(Box::new(job_duration))?;

        Ok(())
    }
}

/// Serialises [`init_registry`] so the whole installation is one step.
///
/// The registry and every metric handle under it are separate `OnceLock`s, and
/// `REGISTRY` is set *last*. Without this lock a second caller that arrives
/// while the first is half-way through walks into `REQUEST_COUNT already set` —
/// a message about the group, not about the registry — and leaves with an error
/// that reads like a defect in the metrics themselves. Which is exactly how a
/// second server built in one process (or a second test) failed.
static INIT: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// Bring the global Prometheus registry up, once per process.
///
/// Idempotent on purpose: the registry is process-global, more than one
/// component may need it standing, and "somebody already did it" is the
/// outcome the caller wanted rather than a failure. What is *not* idempotent —
/// and what the lock above exists for — is the half-installed state in between.
pub fn init_registry() -> Result<(), prometheus::Error> {
    let _guard = INIT.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    if REGISTRY.get().is_some() {
        return Ok(());
    }

    let registry = Registry::new();

    // Register all metric groups
    http_requests::register(&registry)?;
    db::register(&registry)?;
    git::register(&registry)?;
    ci::register(&registry)?;
    business::register(&registry)?;
    rate_limit::register(&registry)?;
    security::register(&registry)?;

    REGISTRY
        .set(registry)
        .map_err(|_| prometheus::Error::Msg("Registry already initialized".into()))?;

    Ok(())
}

/// Rate limit metrics (Phase 22-D).
pub mod rate_limit {
    use prometheus::{IntCounter, Opts, Registry};
    use std::sync::OnceLock;

    /// Counter: total rate-limit blocked requests.
    pub static BLOCKED: OnceLock<IntCounter> = OnceLock::new();

    pub fn register(registry: &Registry) -> Result<(), prometheus::Error> {
        let c = IntCounter::with_opts(Opts::new(
            "rate_limit_blocks_total",
            "Total requests blocked by rate limiter",
        ))?;
        BLOCKED
            .set(c.clone())
            .map_err(|_| prometheus::Error::Msg("rate_limit::BLOCKED already set".into()))?;
        registry.register(Box::new(c))?;
        Ok(())
    }
}

/// Security metrics (Phase 22-D).
pub mod security {
    use prometheus::{IntCounterVec, Opts, Registry};
    use std::sync::OnceLock;

    /// Counter: auth-related events by outcome.
    pub static AUTH_EVENTS: OnceLock<IntCounterVec> = OnceLock::new();

    /// Counter: failed login attempts by reason.
    pub static FAILED_LOGINS: OnceLock<IntCounterVec> = OnceLock::new();

    pub fn register(registry: &Registry) -> Result<(), prometheus::Error> {
        let ae = IntCounterVec::new(
            Opts::new(
                "plombir_git_auth_events_total",
                "Auth events (login/register/provision/mfa/logout)",
            ),
            &["event", "outcome"],
        )?;
        AUTH_EVENTS
            .set(ae.clone())
            .map_err(|_| prometheus::Error::Msg("AUTH_EVENTS already set".into()))?;
        registry.register(Box::new(ae))?;

        let fl = IntCounterVec::new(
            Opts::new(
                "plombir_git_failed_logins_total",
                "Failed login attempts by reason",
            ),
            &["reason"],
        )?;
        FAILED_LOGINS
            .set(fl.clone())
            .map_err(|_| prometheus::Error::Msg("FAILED_LOGINS already set".into()))?;
        registry.register(Box::new(fl))?;

        Ok(())
    }
}

/// Business-level metrics (Phase 22-C).
/// Tracks entity counts that matter for the product: users, repos, issues, PRs, etc.
pub mod business {
    use prometheus::{IntCounter, IntCounterVec, IntGauge, Opts, Registry};
    use std::sync::OnceLock;

    /// Counter: total user registrations.
    pub static USERS_REGISTERED: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total repositories created.
    pub static REPOS_CREATED: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total repositories deleted.
    pub static REPOS_DELETED: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total repositories forked.
    pub static REPOS_FORKED: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total issues opened.
    pub static ISSUES_OPENED: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total issues closed.
    pub static ISSUES_CLOSED: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total PRs opened.
    pub static PRS_OPENED: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total PRs merged.
    pub static PRS_MERGED: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total stars (thumbs up).
    pub static STARS_GIVEN: OnceLock<IntCounter> = OnceLock::new();

    /// Counter: total webhook deliveries by status.
    pub static WEBHOOK_DELIVERIES: OnceLock<IntCounterVec> = OnceLock::new();

    /// Gauge: currently active websocket connections.
    pub static WS_CONNECTIONS: OnceLock<IntGauge> = OnceLock::new();

    /// Gauge: total registered users.
    pub static USERS_TOTAL: OnceLock<IntGauge> = OnceLock::new();

    /// Gauge: total non-deleted repositories.
    pub static REPOS_TOTAL: OnceLock<IntGauge> = OnceLock::new();

    /// Counter: scheduled database backup runs by status.
    pub static DB_BACKUPS: OnceLock<IntCounterVec> = OnceLock::new();

    /// Gauge: Unix timestamp of the last *successful* scheduled database
    /// backup. A counter alone cannot answer "when was the last backup?" —
    /// which is the only question that matters — so the alert
    /// (`BackupTooOld` in `deploy/prometheus/alerts.yml`) reads this.
    /// Stays 0 until the first successful run, so "never backed up" and
    /// "backed up long ago" are both alertable.
    pub static DB_BACKUP_LAST_SUCCESS: OnceLock<IntGauge> = OnceLock::new();

    /// Register all business metrics with the registry.
    pub fn register(registry: &Registry) -> Result<(), prometheus::Error> {
        macro_rules! register_counter {
            ($static:ident, $name:expr, $help:expr) => {{
                let c = IntCounter::with_opts(Opts::new($name, $help))?;
                $static.set(c.clone()).map_err(|_| {
                    prometheus::Error::Msg(concat!(stringify!($static), " already set").into())
                })?;
                registry.register(Box::new(c))?;
            }};
        }

        register_counter!(
            USERS_REGISTERED,
            "plombir_git_users_registered_total",
            "Total user accounts created (self-service registration + LDAP/SSO auto-provision)"
        );
        register_counter!(
            REPOS_CREATED,
            "plombir_git_repos_created_total",
            "Total repositories created"
        );
        register_counter!(
            REPOS_DELETED,
            "plombir_git_repos_deleted_total",
            "Total repositories deleted"
        );
        register_counter!(
            REPOS_FORKED,
            "plombir_git_repos_forked_total",
            "Total repositories forked"
        );
        register_counter!(
            ISSUES_OPENED,
            "plombir_git_issues_opened_total",
            "Total issues opened"
        );
        register_counter!(
            ISSUES_CLOSED,
            "plombir_git_issues_closed_total",
            "Total issues closed"
        );
        register_counter!(
            PRS_OPENED,
            "plombir_git_prs_opened_total",
            "Total PRs opened"
        );
        register_counter!(
            PRS_MERGED,
            "plombir_git_prs_merged_total",
            "Total PRs merged"
        );
        register_counter!(STARS_GIVEN, "plombir_git_stars_total", "Total stars given");

        let wh = IntCounterVec::new(
            Opts::new(
                "plombir_git_webhook_deliveries_total",
                "Total webhook deliveries",
            ),
            &["status"],
        )?;
        WEBHOOK_DELIVERIES
            .set(wh.clone())
            .map_err(|_| prometheus::Error::Msg("WEBHOOK_DELIVERIES already set".into()))?;
        registry.register(Box::new(wh))?;

        let ws = IntGauge::with_opts(Opts::new(
            "plombir_git_ws_connections",
            "Active WebSocket connections",
        ))?;
        WS_CONNECTIONS
            .set(ws.clone())
            .map_err(|_| prometheus::Error::Msg("WS_CONNECTIONS already set".into()))?;
        registry.register(Box::new(ws))?;

        let ut = IntGauge::with_opts(Opts::new("plombir_git_users", "Total registered users"))?;
        USERS_TOTAL
            .set(ut.clone())
            .map_err(|_| prometheus::Error::Msg("USERS_TOTAL already set".into()))?;
        registry.register(Box::new(ut))?;

        let rt = IntGauge::with_opts(Opts::new(
            "plombir_git_repositories",
            "Total non-deleted repositories",
        ))?;
        REPOS_TOTAL
            .set(rt.clone())
            .map_err(|_| prometheus::Error::Msg("REPOS_TOTAL already set".into()))?;
        registry.register(Box::new(rt))?;

        let bk = IntCounterVec::new(
            Opts::new(
                "plombir_git_db_backups_total",
                "Scheduled database backup runs by status",
            ),
            &["status"],
        )?;
        DB_BACKUPS
            .set(bk.clone())
            .map_err(|_| prometheus::Error::Msg("DB_BACKUPS already set".into()))?;
        registry.register(Box::new(bk))?;

        let bt = IntGauge::with_opts(Opts::new(
            "plombir_git_db_backup_last_success_timestamp_seconds",
            "Unix timestamp of the last successful scheduled database backup (0 = never)",
        ))?;
        DB_BACKUP_LAST_SUCCESS
            .set(bt.clone())
            .map_err(|_| prometheus::Error::Msg("DB_BACKUP_LAST_SUCCESS already set".into()))?;
        registry.register(Box::new(bt))?;

        Ok(())
    }
}

/// Helper: record a business event without exposing Prometheus types to callers.
pub mod recorder {
    use super::{business, ci, db, git, security};
    use std::time::Duration;

    /// Record a completed database query: bump the per-operation counter and
    /// observe its duration. `operation` is a coarse *logical* label
    /// (e.g. `"repo.find_by_path"`), never raw SQL — keep its cardinality low.
    /// Covers both the Ok and Err path of the timed future: a slow *failing*
    /// query is still real signal for the `SlowDatabaseQueries` alert.
    pub fn db_query(operation: &str, duration: Duration) {
        if let Some(c) = db::QUERY_COUNT.get() {
            c.with_label_values(&[operation]).inc();
        }
        if let Some(h) = db::QUERY_DURATION.get() {
            h.with_label_values(&[operation])
                .observe(duration.as_secs_f64());
        }
    }

    /// Record an auth-related event by outcome, e.g.
    /// `auth_event("login", "success")` / `auth_event("register", "failure")`.
    /// Keep both labels low-cardinality (a fixed vocabulary of verbs/outcomes).
    pub fn auth_event(event: &str, outcome: &str) {
        if let Some(c) = security::AUTH_EVENTS.get() {
            c.with_label_values(&[event, outcome]).inc();
        }
    }

    /// Record a failed login attempt by coarse reason
    /// (e.g. `"invalid_credentials"`, `"account_locked"`, `"mfa"`).
    pub fn failed_login(reason: &str) {
        if let Some(c) = security::FAILED_LOGINS.get() {
            c.with_label_values(&[reason]).inc();
        }
    }

    /// Record a completed Git transport operation (label: "fetch" for
    /// upload-pack / clone / pull, "push" for receive-pack). Increments the
    /// per-operation counter and observes its duration.
    pub fn git_operation(operation: &str, duration: Duration) {
        if let Some(c) = git::OPERATION_COUNT.get() {
            c.with_label_values(&[operation]).inc();
        }
        if let Some(h) = git::OPERATION_DURATION.get() {
            h.observe(duration.as_secs_f64());
        }
    }

    /// Publish how many CI jobs are executing right now.
    ///
    /// Sampled from the `running` rows by the gauge sink rather than summed by
    /// hand from start/finish events. Hand-summing needed every executor to
    /// increment and every exit to decrement exactly once, and neither held:
    /// only the external-runner `start_job` handler ever incremented, so the
    /// gauge read zero on the default configuration while builds ran, and the
    /// watchdog decremented for any job it reset out of `running` — including
    /// the embedded ones nothing had counted, which walks an `IntGauge` below
    /// zero (card_e309fbb5a3fd). A sampled count has neither failure mode and
    /// is right on an instance running either executor, or both.
    pub fn set_ci_jobs_running(count: i64) {
        if let Some(g) = ci::JOBS_RUNNING.get() {
            g.set(count);
        }
    }

    /// Record that a CI job reached a terminal status: counts the outcome by
    /// status (e.g. "success" / "failed" / "timeout") and — when the runner
    /// measured one — observes the execution duration.
    ///
    /// Both executors reach this: the external runner through its `finish`
    /// handler, the embedded one through
    /// [`rg_core::metrics_hook::record_ci_job_finished`]. The
    /// currently-running gauge is not this function's business — see
    /// [`set_ci_jobs_running`].
    pub fn ci_job_finished(status: &str, duration: Option<Duration>) {
        if let Some(c) = ci::JOB_COUNT.get() {
            c.with_label_values(&[status]).inc();
        }
        if let (Some(h), Some(d)) = (ci::JOB_DURATION.get(), duration) {
            h.observe(d.as_secs_f64());
        }
    }

    /// Record that a CI pipeline reached a terminal status (e.g. "success" /
    /// "failed").
    ///
    /// Reached by both executors, for the same reason as [`ci_job_finished`].
    pub fn ci_pipeline_finished(status: &str) {
        if let Some(c) = ci::PIPELINE_COUNT.get() {
            c.with_label_values(&[status]).inc();
        }
    }

    /// Record a self-service user registration (the `/users/register` handler).
    pub fn user_registered() {
        if let Some(c) = business::USERS_REGISTERED.get() {
            c.inc();
        }
    }

    /// Record a user account auto-provisioned by an external identity source
    /// (LDAP / SSO first-login) rather than self-service registration.
    ///
    /// Bumps the same `plombir_git_users_registered_total` counter — so it stays a
    /// true "accounts created" total that tracks the `plombir_git_users` gauge
    /// instead of silently undercounting directory-backed deployments — and
    /// records provenance via `plombir_git_auth_events_total{event="provision",
    /// outcome=<source>}`. `source` must be a low-cardinality literal
    /// (`"ldap"` / `"sso"`).
    pub fn user_provisioned(source: &str) {
        if let Some(c) = business::USERS_REGISTERED.get() {
            c.inc();
        }
        auth_event("provision", source);
    }

    /// Record a first login an identity provider was not allowed to turn into
    /// an account (`sso_providers.auto_provision` / `allowed_email_domains`).
    ///
    /// Deliberately **not** `auth_event("provision", …)`: that series spends its
    /// `outcome` label on the *source* (`"ldap"` / `"sso"`), so a refusal filed
    /// there would be counted as an account created by a provider named
    /// "refused". It gets its own event, with `reason` carrying the rule that
    /// refused (`"auto_provision_disabled"` / `"email_domain_not_allowed"` /
    /// `"email_not_verified"`).
    pub fn provisioning_refused(reason: &str) {
        auth_event("provision_refused", reason);
    }

    /// Record a repository created.
    pub fn repo_created() {
        if let Some(c) = business::REPOS_CREATED.get() {
            c.inc();
        }
    }

    /// Record a repository deleted.
    pub fn repo_deleted() {
        if let Some(c) = business::REPOS_DELETED.get() {
            c.inc();
        }
    }

    /// Record a repository forked.
    pub fn repo_forked() {
        if let Some(c) = business::REPOS_FORKED.get() {
            c.inc();
        }
    }

    /// Record an issue opened.
    pub fn issue_opened() {
        if let Some(c) = business::ISSUES_OPENED.get() {
            c.inc();
        }
    }

    /// Record an issue closed.
    pub fn issue_closed() {
        if let Some(c) = business::ISSUES_CLOSED.get() {
            c.inc();
        }
    }

    /// Record a PR opened.
    pub fn pr_opened() {
        if let Some(c) = business::PRS_OPENED.get() {
            c.inc();
        }
    }

    /// Record a PR merged.
    pub fn pr_merged() {
        if let Some(c) = business::PRS_MERGED.get() {
            c.inc();
        }
    }

    /// Record a star given.
    pub fn star_given() {
        if let Some(c) = business::STARS_GIVEN.get() {
            c.inc();
        }
    }

    /// Record a webhook delivery by status (success/failed).
    pub fn webhook_delivery(success: bool) {
        if let Some(c) = business::WEBHOOK_DELIVERIES.get() {
            let status = if success { "success" } else { "failed" };
            c.with_label_values(&[status]).inc();
        }
    }

    /// Increment WebSocket connections gauge.
    pub fn ws_connected() {
        if let Some(g) = business::WS_CONNECTIONS.get() {
            g.inc();
        }
    }

    /// Decrement WebSocket connections gauge.
    pub fn ws_disconnected() {
        if let Some(g) = business::WS_CONNECTIONS.get() {
            g.dec();
        }
    }

    /// Set total users gauge.
    pub fn set_users_total(count: i64) {
        if let Some(g) = business::USERS_TOTAL.get() {
            g.set(count);
        }
    }

    /// Set total repositories gauge.
    pub fn set_repos_total(count: i64) {
        if let Some(g) = business::REPOS_TOTAL.get() {
            g.set(count);
        }
    }

    /// Record a finished scheduled database backup run.
    ///
    /// A successful run also stamps
    /// `plombir_git_db_backup_last_success_timestamp_seconds`, because the alert
    /// worth having is "the last backup is older than N hours", and a counter
    /// that stops increasing is indistinguishable from a quiet instance.
    pub fn db_backup(success: bool) {
        if let Some(c) = business::DB_BACKUPS.get() {
            let status = if success { "success" } else { "failed" };
            c.with_label_values(&[status]).inc();
        }
        if success {
            if let Some(g) = business::DB_BACKUP_LAST_SUCCESS.get() {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_secs() as i64)
                    .unwrap_or(0);
                g.set(now);
            }
        }
    }
}

/// Time an async database operation and record it under `operation` when it
/// completes (both Ok and Err paths — a slow *failing* query is still signal).
///
/// The `db_queries_total` / `db_query_duration_seconds` series are populated
/// from a **curated set of hot call-sites** wrapped in this helper, not from
/// every query in the codebase — sea-orm exposes no per-query hook and the
/// `DatabaseConnection` is passed by reference to ~40 ops modules, so a total
/// interceptor would be a cross-crate rewrite. The wrapped set is representative
/// enough to keep the DB panels non-flat and the QPS/latency alerts live; treat
/// `db_queries_total` as a lower bound on true query volume.
pub async fn time_db<F, T>(operation: &'static str, fut: F) -> T
where
    F: std::future::Future<Output = T>,
{
    let start = std::time::Instant::now();
    let out = fut.await;
    recorder::db_query(operation, start.elapsed());
    out
}

/// GET /metrics — return Prometheus-formatted metrics.
/// Who may read `GET /metrics` — `[observability].metrics_enabled` and
/// `metrics_token`.
///
/// The endpoint sits on the main HTTP port, so behind the reverse proxy the
/// deployment guide recommends it was on the internet: business counters, the
/// rate of failed logins, traffic per route (card_ab8a1ca92a56). With a token
/// set, only a scraper that presents it reads anything; switched off, the route
/// answers 404 like a route that does not exist.
#[derive(Debug, Clone)]
pub struct MetricsAccess {
    /// Whether the endpoint answers at all.
    pub enabled: bool,
    /// SHA-256 of the bearer token a scraper must present, when one is set.
    /// Only the digest is kept, so the comparison below is between two values
    /// of one fixed length and cannot leak the token's length or a prefix.
    token_digest: Option<[u8; 32]>,
}

impl Default for MetricsAccess {
    /// The historical behaviour: on, and open to anyone who reaches it.
    fn default() -> Self {
        Self {
            enabled: true,
            token_digest: None,
        }
    }
}

impl MetricsAccess {
    /// `enabled`, guarded by `token` when there is one.
    pub fn new(enabled: bool, token: Option<&str>) -> Self {
        Self {
            enabled,
            token_digest: token.map(digest),
        }
    }

    fn admits(&self, headers: &axum::http::HeaderMap) -> bool {
        let Some(expected) = self.token_digest else {
            return true;
        };
        headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| value.strip_prefix("Bearer "))
            .is_some_and(|presented| digest(presented.trim()) == expected)
    }
}

fn digest(token: &str) -> [u8; 32] {
    use sha2::Digest as _;
    sha2::Sha256::digest(token.as_bytes()).into()
}

pub async fn metrics_handler(
    axum::extract::State(state): axum::extract::State<crate::AppState>,
    headers: axum::http::HeaderMap,
) -> axum::response::Response {
    gate(&state.metrics_access, &headers).unwrap_or_else(|| render(REGISTRY.get()))
}

/// The refusal for a request [`MetricsAccess`] does not admit, if any.
fn gate(
    access: &MetricsAccess,
    headers: &axum::http::HeaderMap,
) -> Option<axum::response::Response> {
    if !access.enabled {
        return Some((StatusCode::NOT_FOUND, "Not Found").into_response());
    }
    if !access.admits(headers) {
        return Some(
            (
                StatusCode::UNAUTHORIZED,
                [(axum::http::header::WWW_AUTHENTICATE, "Bearer")],
                "a bearer token is required to read metrics",
            )
                .into_response(),
        );
    }
    None
}

/// The handler's whole body, with the registry passed in rather than read from
/// the process.
///
/// Split out so the "no registry" branch can be tested by a test that names the
/// case instead of one that happens to run before whoever installs the registry
/// — the previous test began with `if REGISTRY.get().is_some() { return }` and
/// therefore asserted nothing at all in any process where something did
/// (card_00b2bd65060e).
fn render(registry: Option<&Registry>) -> axum::response::Response {
    let Some(registry) = registry else {
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            [(axum::http::header::CONTENT_TYPE, "text/plain")],
            "Metrics registry not initialized".to_string(),
        )
            .into_response();
    };

    let encoder = TextEncoder::new();
    let metric_families = registry.gather();

    match encoder.encode_to_string(&metric_families) {
        Ok(body) => (
            StatusCode::OK,
            [(
                axum::http::header::CONTENT_TYPE,
                "text/plain; version=0.1.0; charset=utf-8",
            )],
            body,
        )
            .into_response(),
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(axum::http::header::CONTENT_TYPE, "text/plain")],
            format!("Error encoding metrics: {e}"),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metrics_are_unavailable_until_a_registry_is_installed() {
        assert_eq!(
            render(None).status(),
            StatusCode::SERVICE_UNAVAILABLE,
            "a scrape before the registry is up has to say so, not answer an empty document"
        );
    }

    #[test]
    fn an_installed_registry_is_scraped() {
        init_registry().expect("install the registry");
        let response = render(REGISTRY.get());

        assert_eq!(response.status(), StatusCode::OK);
    }

    /// The second caller is the case: installing is one step, and finding it
    /// already done is success rather than `REQUEST_COUNT already set`.
    #[test]
    fn installing_the_registry_twice_is_not_an_error() {
        init_registry().expect("install the registry");
        init_registry().expect("a second installation must be a no-op, not a group-level error");
    }

    fn bearer(token: &str) -> axum::http::HeaderMap {
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        headers
    }

    /// card_ab8a1ca92a56: with a token configured, a scrape without it — or
    /// with any other — is a 401; with it, the scrape goes through.
    #[test]
    fn a_metrics_token_admits_only_its_bearer() {
        let access = MetricsAccess::new(true, Some("scrape-secret"));
        let status = |headers: &axum::http::HeaderMap| gate(&access, headers).map(|r| r.status());
        assert_eq!(
            status(&axum::http::HeaderMap::new()),
            Some(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            status(&bearer("scrape-secre")),
            Some(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(
            status(&bearer("scrape-secret-and-more")),
            Some(StatusCode::UNAUTHORIZED)
        );
        assert_eq!(status(&bearer("scrape-secret")), None);
    }

    #[test]
    fn switched_off_metrics_are_not_found_and_the_default_is_open() {
        let off = MetricsAccess::new(false, None);
        assert_eq!(
            gate(&off, &axum::http::HeaderMap::new()).map(|r| r.status()),
            Some(StatusCode::NOT_FOUND)
        );
        assert!(gate(&MetricsAccess::default(), &axum::http::HeaderMap::new()).is_none());
    }
}
