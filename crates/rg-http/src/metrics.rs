//! Prometheus metrics endpoint for IronForge.
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
    use prometheus::{Histogram, HistogramOpts, IntCounterVec, IntGauge, Opts, Registry};
    use std::sync::OnceLock;

    /// Counter: total HTTP requests by method, route, status.
    pub static REQUEST_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Histogram: request duration (seconds).
    pub static REQUEST_DURATION: OnceLock<Histogram> = OnceLock::new();

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

        let request_duration = Histogram::with_opts(
            HistogramOpts::new(
                "http_request_duration_seconds",
                "HTTP request duration in seconds",
            )
            .buckets(vec![
                0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0, 2.5, 5.0, 10.0,
            ]),
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
    use prometheus::{Histogram, HistogramOpts, IntCounterVec, Registry};
    use std::sync::OnceLock;

    /// Counter: total database queries by operation.
    pub static QUERY_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Histogram: database query duration (seconds).
    pub static QUERY_DURATION: OnceLock<Histogram> = OnceLock::new();

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

        let query_duration = Histogram::with_opts(
            HistogramOpts::new(
                "db_query_duration_seconds",
                "Database query duration in seconds",
            )
            .buckets(vec![0.001, 0.005, 0.01, 0.025, 0.05, 0.1, 0.25, 0.5, 1.0]),
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
    use prometheus::{IntCounterVec, IntGauge, Opts, Registry};
    use std::sync::OnceLock;

    /// Counter: total CI pipelines by status.
    pub static PIPELINE_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Counter: total CI jobs by status.
    pub static JOB_COUNT: OnceLock<IntCounterVec> = OnceLock::new();

    /// Gauge: current running jobs.
    pub static JOBS_RUNNING: OnceLock<IntGauge> = OnceLock::new();

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

        Ok(())
    }
}

/// Initialize the global Prometheus registry.
/// Call this once at server startup.
pub fn init_registry() -> Result<(), prometheus::Error> {
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
                "ironforge_auth_events_total",
                "Auth events (login/register/logout)",
            ),
            &["event", "outcome"],
        )?;
        AUTH_EVENTS
            .set(ae.clone())
            .map_err(|_| prometheus::Error::Msg("AUTH_EVENTS already set".into()))?;
        registry.register(Box::new(ae))?;

        let fl = IntCounterVec::new(
            Opts::new(
                "ironforge_failed_logins_total",
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
            "ironforge_users_registered_total",
            "Total user registrations"
        );
        register_counter!(
            REPOS_CREATED,
            "ironforge_repos_created_total",
            "Total repositories created"
        );
        register_counter!(
            REPOS_DELETED,
            "ironforge_repos_deleted_total",
            "Total repositories deleted"
        );
        register_counter!(
            REPOS_FORKED,
            "ironforge_repos_forked_total",
            "Total repositories forked"
        );
        register_counter!(
            ISSUES_OPENED,
            "ironforge_issues_opened_total",
            "Total issues opened"
        );
        register_counter!(
            ISSUES_CLOSED,
            "ironforge_issues_closed_total",
            "Total issues closed"
        );
        register_counter!(PRS_OPENED, "ironforge_prs_opened_total", "Total PRs opened");
        register_counter!(PRS_MERGED, "ironforge_prs_merged_total", "Total PRs merged");
        register_counter!(STARS_GIVEN, "ironforge_stars_total", "Total stars given");

        let wh = IntCounterVec::new(
            Opts::new(
                "ironforge_webhook_deliveries_total",
                "Total webhook deliveries",
            ),
            &["status"],
        )?;
        WEBHOOK_DELIVERIES
            .set(wh.clone())
            .map_err(|_| prometheus::Error::Msg("WEBHOOK_DELIVERIES already set".into()))?;
        registry.register(Box::new(wh))?;

        let ws = IntGauge::with_opts(Opts::new(
            "ironforge_ws_connections",
            "Active WebSocket connections",
        ))?;
        WS_CONNECTIONS
            .set(ws.clone())
            .map_err(|_| prometheus::Error::Msg("WS_CONNECTIONS already set".into()))?;
        registry.register(Box::new(ws))?;

        let ut = IntGauge::with_opts(Opts::new("ironforge_users", "Total registered users"))?;
        USERS_TOTAL
            .set(ut.clone())
            .map_err(|_| prometheus::Error::Msg("USERS_TOTAL already set".into()))?;
        registry.register(Box::new(ut))?;

        let rt = IntGauge::with_opts(Opts::new(
            "ironforge_repositories",
            "Total non-deleted repositories",
        ))?;
        REPOS_TOTAL
            .set(rt.clone())
            .map_err(|_| prometheus::Error::Msg("REPOS_TOTAL already set".into()))?;
        registry.register(Box::new(rt))?;

        Ok(())
    }
}

/// Helper: record a business event without exposing Prometheus types to callers.
pub mod recorder {
    use super::business;

    /// Record a user registration.
    pub fn user_registered() {
        if let Some(c) = business::USERS_REGISTERED.get() {
            c.inc();
        }
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
}

/// GET /metrics — return Prometheus-formatted metrics.
pub async fn metrics_handler() -> impl IntoResponse {
    let Some(registry) = REGISTRY.get() else {
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
    use axum::body::Body;
    use axum::http::Response;

    #[tokio::test]
    async fn metrics_handler_returns_503_when_registry_is_uninitialized() {
        if REGISTRY.get().is_some() {
            return;
        }

        let response: Response<Body> = metrics_handler().await.into_response();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
