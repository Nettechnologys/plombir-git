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
            tracing::warn!(command = %command, ?timeout, "git command timed out, returning 504");
            return Self::Timeout(format!("git command timed out after {timeout:?}"));
        }

        // H-05: Log the full error for operators, store a generic message internally.
        // The IntoResponse impl will also sanitize the client-facing message.
        let full_msg = e.to_string();
        tracing::error!(error = %full_msg, "anyhow error converted to AppError");
        Self::InternalError(full_msg)
    }
}

impl From<sea_orm::DbErr> for AppError {
    fn from(e: sea_orm::DbErr) -> Self {
        use sea_orm::DbErr;
        // H-05: Log the database error for operators, never expose to clients.
        let full_msg = e.to_string();
        // Connection-level failures mean the database itself is unreachable
        // (pool acquire timed out / closed, or the connection dropped) — a
        // transient, retryable outage. Surface it as 503 so LBs and clients
        // retry instead of treating an outage as a fatal 500. Statement-level
        // errors (Exec/Query/constraint/type) stay 500 — those are bugs, not
        // outages, and retrying them won't help.
        match &e {
            DbErr::Conn(_) | DbErr::ConnectionAcquire(_) => {
                tracing::error!(error = %full_msg, "database unavailable (connection-level error), returning 503");
                Self::ServiceUnavailable(full_msg)
            }
            _ => {
                tracing::error!(error = %full_msg, "database error converted to AppError");
                Self::InternalError(full_msg)
            }
        }
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

    #[test]
    fn service_unavailable_response_does_not_leak_detail() {
        use axum::response::IntoResponse;
        let err = AppError::ServiceUnavailable("postgres://user:pw@host down".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}
