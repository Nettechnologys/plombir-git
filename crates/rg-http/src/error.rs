//! Centralized error handling for IronForge HTTP API.
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
    InternalError(String),
    #[error("{0}")]
    TooManyRequests(String),
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
            Self::InternalError(_) => "INTERNAL_ERROR",
            Self::TooManyRequests(_) => "RATE_LIMITED",
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
            Self::InternalError(_) => StatusCode::INTERNAL_SERVER_ERROR,
            Self::TooManyRequests(_) => StatusCode::TOO_MANY_REQUESTS,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        let status = self.status();
        let code = self.code();

        // H-05: Never expose internal error details to clients.
        // Log the original error for operators, return a generic message.
        let sanitized_message = match &self {
            Self::InternalError(msg) => {
                tracing::error!(error = %msg, "Internal server error returned to client");
                "Internal server error".to_string()
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
        // H-05: Log the full error for operators, store a generic message internally.
        // The IntoResponse impl will also sanitize the client-facing message.
        let full_msg = e.to_string();
        tracing::error!(error = %full_msg, "anyhow error converted to AppError");
        Self::InternalError(full_msg)
    }
}

impl From<sea_orm::DbErr> for AppError {
    fn from(e: sea_orm::DbErr) -> Self {
        // H-05: Log the database error for operators, never expose to clients.
        let full_msg = e.to_string();
        tracing::error!(error = %full_msg, "database error converted to AppError");
        Self::InternalError(full_msg)
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
}
