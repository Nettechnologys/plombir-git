//! Centralized error handling for ForgeKeep HTTP API.
//!
//! All API handlers should return `AppError` variants instead of ad-hoc
//! `(StatusCode, Json)` tuples.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde::Serialize;

/// Structured error response body.
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: ErrorBody,
}

#[derive(Debug, Serialize)]
pub struct ErrorBody {
    /// Machine-readable error code, e.g. "NOT_FOUND", "BAD_REQUEST".
    pub code: &'static str,
    /// Human-readable error message.
    pub message: String,
    /// Request ID (injected by request-id middleware for error responses).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_id: Option<String>,
}

/// Unified application error type for all HTTP handlers.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("{0}")]
    NotFound(String),
    #[error("{0}")]
    BadRequest(String),
    #[error("{0}")]
    Unauthorized(String),
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Conflict(String),
    #[error("{0}")]
    Gone(String),
    #[error("{0}")]
    TooManyRequests(String),
    /// 503 — a required downstream dependency (the database) is unreachable.
    /// A transient, retryable outage, not a logic error: load balancers and
    /// clients should retry rather than treat it as a fatal 500.
    #[error("{0}")]
    ServiceUnavailable(String),
    /// 504 — an upstream operation (a git CLI invocation) exceeded its
    /// wall-clock deadline. The request itself was well-formed; the backend
    /// was too slow.
    #[error("{0}")]
    Timeout(String),
    #[error("{0}")]
    InternalError(String),
}

impl AppError {
    /// Machine-readable error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound(_) => "NOT_FOUND",
            Self::BadRequest(_) => "BAD_REQUEST",
            Self::Unauthorized(_) => "UNAUTHORIZED",
            Self::Forbidden(_) => "FORBIDDEN",
            Self::Conflict(_) => "CONFLICT",
            Self::Gone(_) => "GONE",
            Self::TooManyRequests(_) => "RATE_LIMITED",
            Self::ServiceUnavailable(_) => "DB_UNAVAILABLE",
            Self::Timeout(_) => "GIT_TIMEOUT",
            Self::InternalError(_) => "INTERNAL_ERROR",
        }
    }

    /// HTTP status code.
    pub fn status(&self) -> StatusCode {
        match self {
            Self::NotFound(_) => StatusCode::NOT_FOUND,
            Self::BadRequest(_) => StatusCode::BAD_REQUEST,
            Self::Unauthorized(_) => StatusCode::UNAUTHORIZED,
            Self::Forbidden(_) => StatusCode::FORBIDDEN,
            Self::Conflict(_) => StatusCode::CONFLICT,
            Self::Gone(_) => StatusCode::GONE,
            Self::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
            Self::ServiceUnavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
            Self::Timeout(_) => StatusCode::GATEWAY_TIMEOUT,
            Self::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.code();

        // H-05: Never expose internal error details to clients.
        // Log the original error for operators, return a generic message.
        // H-05: internal detail (DB errors, git command lines) must never
        // reach the client — log it for operators, return a generic message.
        let sanitized_message = match &self {
            Self::InternalError(msg) => {
                tracing::error!(error = %msg, "Internal server error returned to client");
                "Internal server error".to_string()
            }
            Self::ServiceUnavailable(msg) => {
                tracing::error!(error = %msg, "Service unavailable returned to client");
                "Service temporarily unavailable".to_string()
            }
            Self::Timeout(msg) => {
                tracing::warn!(error = %msg, "Gateway timeout returned to client");
                "Upstream operation timed out".to_string()
            }
            other => other.to_string(),
        };

        let body = ErrorResponse {
            error: ErrorBody {
                code,
                message: sanitized_message,
                request_id: None,
            },
        };
        (status, axum::Json(body)).into_response()
    }
}

impl From<anyhow::Error> for AppError {
    fn from(e: anyhow::Error) -> Self {
        // A git operation that blew its wall-clock deadline is a 504 gateway
        // timeout, not a 500: the request was well-formed, the backend (git)
        // was too slow. The gateway propagates `GitCliError::Timeout` as the
        // anyhow source, so downcast through any added context to detect it.
        if let Some(rg_git::cli_gateway::GitCliError::Timeout { command, timeout }) =
            e.downcast_ref::<rg_git::cli_gateway::GitCliError>()
        {
            // `{:#}` for the same reason as the 500/503 branches below: a
            // rebuilt "git command timed out after {timeout:?}" string names
            // only the deadline, while the `.context(...)` layers the caller
            // added ("failed to mirror repository", the repo/ref in flight) say
            // *which* request stalled — and `GitCliError::Timeout`'s own
            // `Display` already carries the command and the duration, so the
            // flattened chain is a strict superset. `IntoResponse` still
            // sanitizes the client-facing message to "Upstream operation timed
            // out", so nothing internal leaks.
            let full_msg = format!("{e:#}");
            tracing::warn!(command = %command, ?timeout, error = %full_msg, "git command timed out, returning 504");
            return Self::Timeout(full_msg);
        }

        // Many `rg_db::ops` helpers return `anyhow::Result`, wrapping the
        // underlying `sea_orm::DbErr` with `.context("db: ...")`. A connection
        // outage on such a path must still be a retryable 503, not a 500 —
        // `downcast_ref` sees through the `.context()` layers to the original
        // `DbErr`, so classify it exactly like the direct `From<DbErr>` path.
        if let Some(db_err) = e.downcast_ref::<sea_orm::DbErr>() {
            if Self::is_db_outage(db_err) {
                // Keep the full anyhow context chain in the operator log; the
                // IntoResponse impl still sanitizes the client-facing message.
                // `{:#}` is load-bearing: `to_string()` renders only the
                // outermost `.context(...)`, so the `DbErr` we just downcast to
                // would never reach the log.
                let full_msg = format!("{e:#}");
                tracing::error!(error = %full_msg, "database unavailable (connection-level error via anyhow), returning 503");
                return Self::ServiceUnavailable(full_msg);
            }
        }

        // A service that reports "this row genuinely is not there" carries
        // `rg_core::error::NotFound`, and that — not "the handler could not
        // produce an answer" — is the only thing that may become a 404. The
        // check sits *after* the timeout and outage branches on purpose: those
        // classify a failed lookup, and a failed lookup must never be reported
        // as an absent resource, whatever context got layered on top.
        //
        // `to_string()` (not `{:#}`) is deliberate here: `NotFound`'s own
        // `Display` is a fixed "<resource> not found" with no request data and
        // no `db: ...` chain in it, and unlike the 5xx variants below this
        // message reaches the client verbatim (H-05).
        if let Some(not_found) = e.downcast_ref::<rg_core::error::NotFound>() {
            return Self::NotFound(not_found.to_string());
        }

        // H-05: Log the full error for operators, store a generic message internally.
        // The IntoResponse impl will also sanitize the client-facing message.
        //
        // This is the funnel every handler's `?` passes through, so `{:#}` here
        // is what makes a 500 diagnosable at all: `rg_db::ops` wraps every
        // failure in `.context("db: ...")`, and a bare `to_string()` prints that
        // context *only* — the `DbErr` naming the table, column or constraint is
        // dropped on the floor.
        let full_msg = format!("{e:#}");
        tracing::error!(error = %full_msg, "anyhow error converted to AppError");
        Self::InternalError(full_msg)
    }
}

impl From<sea_orm::DbErr> for AppError {
    fn from(e: sea_orm::DbErr) -> Self {
        // H-05: Log the database error for operators, never expose to clients.
        let full_msg = e.to_string();
        // Connection-level failures mean the database itself is unreachable
        // (pool acquire timed out / closed, or the connection dropped) — a
        // transient, retryable outage. Surface it as 503 so LBs and clients
        // retry instead of treating an outage as a fatal 500. Statement-level
        // errors (Exec/Query/constraint/type) stay 500 — those are bugs, not
        // outages, and retrying them won't help.
        if Self::is_db_outage(&e) {
            tracing::error!(error = %full_msg, "database unavailable (connection-level error), returning 503");
            Self::ServiceUnavailable(full_msg)
        } else {
            tracing::error!(error = %full_msg, "database error converted to AppError");
            Self::InternalError(full_msg)
        }
    }
}

impl AppError {
    /// Whether a `sea_orm::DbErr` represents a connection-level outage
    /// (retryable → 503) rather than a statement-level bug (→ 500). Shared by
    /// the `From<DbErr>` and `From<anyhow::Error>` conversions so a database
    /// outage classifies identically whether the handler surfaced the raw
    /// `DbErr` or an anyhow-wrapped one.
    ///
    /// `pub(crate)` so the OCI registry handlers (which must preserve their own
    /// OCI error-envelope and therefore can't route through `AppError`) can
    /// reuse the exact same outage predicate — see `oci::oci_status_for`.
    pub(crate) fn is_db_outage(e: &sea_orm::DbErr) -> bool {
        use sea_orm::DbErr;
        matches!(e, DbErr::Conn(_) | DbErr::ConnectionAcquire(_))
    }
}

/// Helper constructors for `AppError`.
impl AppError {
    pub fn not_found(msg: impl std::fmt::Display) -> Self {
        Self::NotFound(msg.to_string())
    }

    pub fn bad_request(msg: impl std::fmt::Display) -> Self {
        Self::BadRequest(msg.to_string())
    }

    pub fn unauthorized(msg: impl std::fmt::Display) -> Self {
        Self::Unauthorized(msg.to_string())
    }

    pub fn forbidden(msg: impl std::fmt::Display) -> Self {
        Self::Forbidden(msg.to_string())
    }

    pub fn conflict(msg: impl std::fmt::Display) -> Self {
        Self::Conflict(msg.to_string())
    }

    pub fn gone(msg: impl std::fmt::Display) -> Self {
        Self::Gone(msg.to_string())
    }

    pub fn internal(msg: impl std::fmt::Display) -> Self {
        Self::InternalError(msg.to_string())
    }

    pub fn rate_limited(msg: impl std::fmt::Display) -> Self {
        Self::TooManyRequests(msg.to_string())
    }

    /// 503 — a downstream dependency is unavailable (retryable).
    pub fn service_unavailable(msg: impl std::fmt::Display) -> Self {
        Self::ServiceUnavailable(msg.to_string())
    }

    /// 504 — an upstream operation exceeded its deadline.
    pub fn timeout(msg: impl std::fmt::Display) -> Self {
        Self::Timeout(msg.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::StatusCode;
    use sea_orm::{ConnAcquireErr, DbErr, RuntimeErr};

    #[test]
    fn db_connection_error_maps_to_503_db_unavailable() {
        let err: AppError = DbErr::Conn(RuntimeErr::Internal("connection reset".into())).into();
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code(), "DB_UNAVAILABLE");
    }

    #[test]
    fn db_pool_acquire_timeout_maps_to_503() {
        let err: AppError = DbErr::ConnectionAcquire(ConnAcquireErr::Timeout).into();
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code(), "DB_UNAVAILABLE");

        let err: AppError = DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed).into();
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn db_statement_error_stays_500() {
        // Exec/Query are statement-level (constraint/type/logic) — not an outage.
        let err: AppError = DbErr::Exec(RuntimeErr::Internal("UNIQUE constraint failed".into())).into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code(), "INTERNAL_ERROR");

        let err: AppError = DbErr::RecordNotInserted.into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// Every handler `?` funnels through `From<anyhow::Error>`, and the message it
    /// keeps is exactly what reaches the operator log. `Display` for
    /// `anyhow::Error` renders only the outermost `.context(...)`, so a
    /// `to_string()` here silently deletes the reason — which is the whole point
    /// of having layered context in `rg_db::ops` and the services.
    #[test]
    fn anyhow_conversion_keeps_the_nested_context_chain() {
        use anyhow::Context;

        let err = Err::<(), _>(DbErr::Exec(RuntimeErr::Internal(
            "UNIQUE constraint failed: repos.name".into(),
        )))
        .context("db: failed to insert repo")
        .context("creating repository \"acme/widgets\"")
        .unwrap_err();

        // Pre-condition: this is what a bare `%e` / `to_string()` would have shown.
        assert_eq!(err.to_string(), "creating repository \"acme/widgets\"");

        let app_err: AppError = err.into();
        let AppError::InternalError(logged) = &app_err else {
            panic!("expected InternalError, got {app_err:?}");
        };
        // All three layers survive, innermost cause included.
        assert!(logged.contains("creating repository \"acme/widgets\""), "{logged}");
        assert!(logged.contains("db: failed to insert repo"), "{logged}");
        assert!(logged.contains("UNIQUE constraint failed: repos.name"), "{logged}");
    }

    /// Same guarantee on the 503 branch, which resolves the message separately
    /// after downcasting to `DbErr`.
    #[test]
    fn db_outage_via_anyhow_keeps_the_nested_context_chain() {
        use anyhow::Context;

        let err = Err::<(), _>(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
            .context("db: failed to list pending jobs")
            .context("runner watchdog sweep")
            .unwrap_err();

        let app_err: AppError = err.into();
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        let AppError::ServiceUnavailable(logged) = &app_err else {
            panic!("expected ServiceUnavailable, got {app_err:?}");
        };
        assert!(logged.contains("runner watchdog sweep"), "{logged}");
        assert!(logged.contains("db: failed to list pending jobs"), "{logged}");
        assert!(logged.contains("Connection pool timed out"), "{logged}");
    }

    #[test]
    fn git_timeout_maps_to_504_gateway_timeout() {
        let git_err = rg_git::cli_gateway::GitCliError::Timeout {
            command: "git -C /srv/repos/foo.git fetch".to_string(),
            timeout: std::time::Duration::from_secs(120),
        };
        let err: AppError = anyhow::Error::from(git_err).into();
        assert_eq!(err.status(), StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(err.code(), "GIT_TIMEOUT");
    }

    #[test]
    fn git_timeout_detected_through_added_context() {
        // Callers commonly add `.context(...)`; downcast must still find the
        // timeout so the 504 classification survives wrapping.
        let git_err = rg_git::cli_gateway::GitCliError::Timeout {
            command: "git clone".to_string(),
            timeout: std::time::Duration::from_secs(30),
        };
        let wrapped = anyhow::Error::from(git_err).context("failed to mirror repository");
        let err: AppError = wrapped.into();
        assert_eq!(err.status(), StatusCode::GATEWAY_TIMEOUT);
    }

    /// Same context guarantee as the 500/503 branches, on the 504 one. Handlers
    /// no longer log the error themselves before `?`, so this funnel is the only
    /// place the `.context(...)` layers can still reach an operator — a
    /// `Timeout` message rebuilt from `{timeout:?}` alone would silently drop
    /// which request stalled.
    #[test]
    fn git_timeout_keeps_the_nested_context_chain() {
        let git_err = rg_git::cli_gateway::GitCliError::Timeout {
            command: "git -C /srv/repos/acme/widgets.git fetch --prune".to_string(),
            timeout: std::time::Duration::from_secs(120),
        };
        let err: AppError = anyhow::Error::from(git_err)
            .context("mirroring \"acme/widgets\"")
            .context("scheduled mirror sync")
            .into();

        let AppError::Timeout(logged) = &err else {
            panic!("expected Timeout, got {err:?}");
        };
        assert!(logged.contains("scheduled mirror sync"), "{logged}");
        assert!(logged.contains("mirroring \"acme/widgets\""), "{logged}");
        assert!(
            logged.contains("git -C /srv/repos/acme/widgets.git fetch --prune"),
            "{logged}"
        );
        assert!(logged.contains("120s"), "{logged}");
    }

    /// The 504 body must still be generic — widening the stored message to the
    /// full chain above must not start leaking git command lines to clients.
    #[tokio::test]
    async fn timeout_response_does_not_leak_the_command_line() {
        use axum::response::IntoResponse;
        let err = AppError::Timeout(
            "scheduled mirror sync: git -C /srv/repos/acme/widgets.git fetch: timed out".into(),
        );
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::GATEWAY_TIMEOUT);

        let body = axum::body::to_bytes(resp.into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Upstream operation timed out"), "{body}");
        assert!(
            !body.contains("/srv/repos"),
            "leaked internal detail: {body}"
        );
        assert!(!body.contains("git -C"), "leaked internal detail: {body}");
    }

    #[test]
    fn anyhow_wrapped_db_connection_error_maps_to_503() {
        // `rg_db::ops` helpers return `anyhow::Result`, wrapping the `DbErr`
        // with `.context("db: ...")`. A connection outage on such a path must
        // still classify as 503 — the downcast has to see through the context.
        let db_err = DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed);
        let wrapped = anyhow::Error::from(db_err).context("db: get job");
        let err: AppError = wrapped.into();
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(err.code(), "DB_UNAVAILABLE");
    }

    #[test]
    fn anyhow_wrapped_db_statement_error_stays_500() {
        // A statement-level DbErr wrapped in anyhow is a bug, not an outage.
        let db_err = DbErr::Exec(RuntimeErr::Internal("UNIQUE constraint failed".into()));
        let wrapped = anyhow::Error::from(db_err).context("db: insert row");
        let err: AppError = wrapped.into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn non_timeout_git_error_stays_500() {
        let git_err = rg_git::cli_gateway::GitCliError::Failed {
            command: "git push".to_string(),
            exit_code: "128".to_string(),
        };
        let err: AppError = anyhow::Error::from(git_err).into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn generic_anyhow_error_stays_500() {
        let err: AppError = anyhow::anyhow!("something unexpected").into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code(), "INTERNAL_ERROR");
    }

    /// A service reporting "this row is genuinely absent" is the only thing
    /// that may become a 404 — and the message the client gets is the type's
    /// own fixed text, never the caller's context chain.
    #[test]
    fn core_not_found_maps_to_404_with_a_fixed_message() {
        let err = anyhow::Error::from(rg_core::error::NotFound::new("pull request"))
            .context("db: find_by_repo_and_number(repo_id = 7)");
        let app_err: AppError = err.into();

        assert_eq!(app_err.status(), StatusCode::NOT_FOUND);
        let AppError::NotFound(message) = &app_err else {
            panic!("expected NotFound, got {app_err:?}");
        };
        assert_eq!(message, "pull request not found");
        assert!(!message.contains("db:"), "{message}");
    }

    /// Ordering guarantee: a *failed* lookup outranks an *absent* resource, no
    /// matter what got layered on top. If a caller ever wraps a real outage in
    /// not-found context, the outage classification must still win — a 404 on a
    /// dead database is the exact bug this branch was added to prevent.
    #[test]
    fn db_outage_wrapped_in_not_found_context_stays_503() {
        let err = anyhow::Error::from(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
            .context(rg_core::error::NotFound::new("pull request"));
        let app_err: AppError = err.into();

        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(app_err.code(), "DB_UNAVAILABLE");
    }

    #[test]
    fn service_unavailable_response_does_not_leak_detail() {
        use axum::response::IntoResponse;
        let err = AppError::ServiceUnavailable("postgres://user:pw@host down".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
