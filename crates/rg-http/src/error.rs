//! Centralized error handling for Plombir Git HTTP API.
//!
//! All API handlers should return `AppError` variants instead of ad-hoc
//! `(StatusCode, Json)` tuples.

use axum::body::Body;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use serde::Serialize;
use std::fmt;
use std::io;
use std::path::Path;

/// The repository row resolved in the database, but the bare git directory under
/// `repo_root` is not usable. That is an operator/storage fault, never a 404.
pub(crate) fn repository_storage_missing(repo_path: &Path) -> anyhow::Error {
    let error = io::Error::new(io::ErrorKind::NotFound, "repository directory is missing");
    repository_storage_probe_error(repo_path, error)
}

pub(crate) fn repository_storage_probe_error(repo_path: &Path, error: io::Error) -> anyhow::Error {
    rg_core::platform::fs::path_error(
        "repository directory",
        repo_path,
        &error,
        rg_core::platform::fs::REPO_ROOT_HINT,
    )
}

/// Same storage-fault shape for paths that exist but `gix`/`git` cannot open as
/// a repository. The client-facing 5xx body stays sanitized; the path and
/// `repo_root` hint live in the operator log.
pub(crate) fn repository_storage_open_error(
    repo_path: &Path,
    error: impl fmt::Display,
) -> anyhow::Error {
    repository_storage_probe_error(
        repo_path,
        io::Error::other(format!("failed to open repository: {error}")),
    )
}

pub(crate) fn ensure_repository_storage(repo_path: &Path) -> anyhow::Result<()> {
    match repo_path.try_exists() {
        Ok(true) => Ok(()),
        Ok(false) => Err(repository_storage_missing(repo_path)),
        Err(error) => Err(repository_storage_probe_error(repo_path, error)),
    }
}

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
    #[error("{0}")]
    PayloadTooLarge(String),
    /// 502 — a host this instance does not own failed to answer: an identity
    /// provider that timed out, an upstream API that returned a `5xx`. The
    /// request was well-formed and no edit to it can help, so it must not be
    /// a `4xx`; and it is not our bug, so `500` would send the operator
    /// looking in the wrong process.
    #[error("{0}")]
    BadGateway(String),
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
            Self::PayloadTooLarge(_) => "PAYLOAD_TOO_LARGE",
            Self::BadGateway(_) => "UPSTREAM_ERROR",
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
            Self::PayloadTooLarge(_) => StatusCode::PAYLOAD_TOO_LARGE,
            Self::BadGateway(_) => StatusCode::BAD_GATEWAY,
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
            Self::BadGateway(msg) => {
                tracing::error!(error = %msg, "Upstream failure returned to client");
                "Upstream service is unavailable".to_string()
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

/// Put the API's JSON envelope on a refusal no handler produced.
///
/// Every `/api/v1` answer a client can act on arrives as [`ErrorResponse`] —
/// that is the contract the frontend parses and the one the published OpenAPI
/// document describes. Axum itself can refuse a request before a handler runs:
/// malformed JSON and query/path values are `400 text/plain`, an unsupported
/// JSON content type is `415 text/plain`, and a method no route accepts is an
/// empty `405`. A body limit adds the fourth shape, `413 text/plain`. None can
/// be fixed in the handler because none reaches it.
///
/// [`crate::route_table::DeclaredBodyLimit`] carries the number the route
/// declared, so the message names the limit rather than restating the status.
///
/// Only the statuses Axum's router and extractors generate are rewritten, and
/// only when the response does not already carry a non-empty `error.code`.
/// Checking the structure, not just `Content-Type`, matters: older middleware
/// also emitted JSON, but in an incompatible `{"error":"..."}` shape. This
/// boundary is deliberate: package protocols also live below `/api/v1`, and
/// some of their `404` bodies are specified as plain text. A handler that
/// already raised an [`AppError`] likewise keeps its more specific code and
/// message.
///
/// The original response parts are retained. In particular, a `405` keeps its
/// `Allow` header and a rate/availability refusal would keep `Retry-After`; only
/// the two entity headers are replaced along with the body.
pub(crate) async fn api_rejection_envelope(response: Response) -> Response {
    const MAX_ERROR_BODY_BYTES: usize = 64 * 1024;

    let status = response.status();
    if !matches!(
        status,
        StatusCode::BAD_REQUEST
            | StatusCode::METHOD_NOT_ALLOWED
            | StatusCode::PAYLOAD_TOO_LARGE
            | StatusCode::UNSUPPORTED_MEDIA_TYPE
    ) {
        return response;
    }

    let content_is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .is_some_and(|value| value.starts_with("application/json"));
    let (mut parts, original_body) = response.into_parts();
    let original_body = match axum::body::to_bytes(original_body, MAX_ERROR_BODY_BYTES).await {
        Ok(body) => Some(body),
        Err(error) => {
            tracing::warn!(%status, %error, "could not inspect non-success API response body");
            None
        }
    };

    let already_an_envelope = content_is_json
        && original_body.as_ref().is_some_and(|body| {
            serde_json::from_slice::<serde_json::Value>(body)
                .ok()
                .is_some_and(|json| {
                    json.pointer("/error/code")
                        .and_then(serde_json::Value::as_str)
                        .is_some_and(|code| !code.is_empty())
                })
        });
    if already_an_envelope {
        return Response::from_parts(
            parts,
            Body::from(original_body.expect("body inspected above")),
        );
    }

    let (code, message) = match status {
        StatusCode::BAD_REQUEST => (
            "BAD_REQUEST",
            "request body or parameters could not be parsed".to_string(),
        ),
        StatusCode::METHOD_NOT_ALLOWED => (
            "METHOD_NOT_ALLOWED",
            "request method is not allowed for this endpoint".to_string(),
        ),
        StatusCode::PAYLOAD_TOO_LARGE => (
            "PAYLOAD_TOO_LARGE",
            match parts
                .extensions
                .get::<crate::route_table::DeclaredBodyLimit>()
            {
                Some(limit) => format!("request body exceeds this endpoint's limit of {limit}"),
                None => "request body exceeds the limit this endpoint declares".to_string(),
            },
        ),
        StatusCode::UNSUPPORTED_MEDIA_TYPE => (
            "UNSUPPORTED_MEDIA_TYPE",
            "request content type is not supported".to_string(),
        ),
        _ => unreachable!("non-rejection statuses returned before the response body was consumed"),
    };

    let body = serde_json::to_vec(&ErrorResponse {
        error: ErrorBody {
            code,
            message,
            request_id: None,
        },
    })
    .expect("ErrorResponse contains only serializable primitives");
    parts.headers.remove(header::CONTENT_LENGTH);
    parts.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json"),
    );
    Response::from_parts(parts, Body::from(body))
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
        // underlying `sea_orm::DbErr` with `.context("db: ...")`. A database
        // that was unreachable — or a transaction that lost to a concurrent
        // writer until its retry budget ran out — must still be a retryable
        // 503 on such a path, not a 500. `downcast_ref` sees through the
        // `.context()` layers to the original `DbErr`, so classify it exactly
        // like the direct `From<DbErr>` path.
        //
        // The contention half matters most here rather than on the raw `DbErr`
        // path: `rg_db::contention::retry_transaction` is what a contended
        // write meets first, and what it hands back once its attempts are spent
        // is precisely this shape — the backend's refusal under a
        // `db: <what> after N concurrent conflicts` context. A 500 there tells
        // the client not to repeat a request that would succeed the moment the
        // writer ahead of it commits.
        if let Some(db_err) = e.downcast_ref::<sea_orm::DbErr>() {
            if Self::is_db_retryable(db_err) {
                // Keep the full anyhow context chain in the operator log; the
                // IntoResponse impl still sanitizes the client-facing message.
                // `{:#}` is load-bearing: `to_string()` renders only the
                // outermost `.context(...)`, so the `DbErr` we just downcast to
                // would never reach the log.
                let full_msg = format!("{e:#}");
                tracing::error!(error = %full_msg, "transient database failure via anyhow (unreachable or contended), returning 503");
                return Self::ServiceUnavailable(full_msg);
            }
        }

        // A concurrent upload of the same LFS object still holds its publication
        // lease. Nothing is wrong with this request and nothing is wrong with the
        // server — the object is simply being written by somebody else right now,
        // and the caller should come back rather than give up. `503` says that;
        // the `500` this would otherwise be says the opposite.
        if let Some(busy) = e.downcast_ref::<rg_core::lfs::service::LfsPublicationBusy>() {
            let full_msg = format!("{e:#}");
            tracing::warn!(oid = %busy.oid, waited_seconds = busy.waited_seconds, error = %full_msg, "LFS object is being published by a concurrent upload, returning 503");
            return Self::ServiceUnavailable(full_msg);
        }

        // A host we do not own failed to answer. Sits above the four
        // client-fault branches below for the same reason the outage branch
        // does: a call that never completed must not be reported as an absent
        // resource or a malformed request, whatever context got layered on top.
        // The `502` says which process to look in — theirs, not ours — and
        // leaves the request retryable, which is the only useful thing a
        // client or a proxy can do about it.
        //
        // `{:#}` keeps the transport error the marker was layered over: the
        // marker's own `Display` names the provider, and the cause under it
        // names the timeout / TLS failure / status. `IntoResponse` sanitizes
        // the client-facing body, so none of it leaks (H-05).
        if e.downcast_ref::<rg_core::error::UpstreamUnavailable>()
            .is_some()
        {
            let full_msg = format!("{e:#}");
            tracing::error!(error = %full_msg, "upstream dependency did not answer, returning 502");
            return Self::BadGateway(full_msg);
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

        // A valid CI file may select no jobs for the requested ref. Automatic
        // producers treat that as "no pipeline", while manual/retry requests
        // need a precise 400 instead of the generic 500 fallback. It is not an
        // `InvalidRequest`: classifying it as one would make push/PR paths
        // publish a terminal configuration-failure graph for valid `only:`.
        if let Some(no_match) = e.downcast_ref::<rg_core::ci::NoMatchingCiJobs>() {
            return Self::BadRequest(no_match.to_string());
        }

        // The mirror of the branch above, for the client's half of the split: a
        // service that rejected the *request* carries
        // `rg_core::error::InvalidRequest`, and only that may become a 400. An
        // upload handler cannot tell "this file type is not allowed" from "the
        // blob store refused the write" once both are flattened into an
        // `anyhow::Error`, and calling the second one a bad request stops the
        // client from ever retrying a failure it did not cause.
        //
        // `to_string()` (not `{:#}`) for the same reason as `NotFound`: the
        // type's own `Display` is the fixed rule text that reaches the client,
        // with no path or errno from the `.context(…)` layers above it.
        if let Some(invalid) = e.downcast_ref::<rg_core::error::InvalidRequest>() {
            return Self::BadRequest(invalid.to_string());
        }

        // The request decoded correctly, but the represented file/artifact is
        // larger than the business API accepts. Keep this distinct from both
        // malformed input (400) and the transport/extractor ceiling (also 413,
        // but raised before the handler) so service-level callers cannot bypass
        // the size contract by reaching rg-core directly.
        if let Some(too_large) = e.downcast_ref::<rg_core::error::PayloadTooLarge>() {
            return Self::PayloadTooLarge(too_large.to_string());
        }

        // The state half of the same split: "this pull request is closed",
        // "another merge is already running", "the merge conflicts". None of
        // those is a malformed request — answering 400 tells the client to fix
        // something that was already correct, and answering 400 to the storage
        // failure sitting next to them hides it completely.
        if let Some(conflict) = e.downcast_ref::<rg_core::error::Conflict>() {
            return Self::Conflict(conflict.to_string());
        }

        // And the policy half: a branch-protection rule that refuses the merge
        // is a 403, whereas the *check* failing is ours and stays a 5xx.
        if let Some(forbidden) = e.downcast_ref::<rg_core::error::Forbidden>() {
            return Self::Forbidden(forbidden.to_string());
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
        // Two things make a database failure worth coming back for: the
        // database was unreachable (pool acquire timed out / closed, connection
        // dropped), or the transaction lost a race against another writer and
        // its retry budget ran out. Both are transient and both are somebody
        // else's timing rather than this request's fault, so 503 — the status
        // that says "try again" — is the honest answer, and it is what LBs and
        // clients act on. Everything else stays 500: a constraint violation or
        // a type mismatch is a bug, and repeating it changes nothing.
        if Self::is_db_retryable(&e) {
            tracing::error!(error = %full_msg, "transient database failure (unreachable or contended), returning 503");
            Self::ServiceUnavailable(full_msg)
        } else {
            tracing::error!(error = %full_msg, "database error converted to AppError");
            Self::InternalError(full_msg)
        }
    }
}

impl AppError {
    /// Whether a `sea_orm::DbErr` is something the caller should come back for
    /// (retryable → 503) rather than a fault in the request or in us (→ 500).
    ///
    /// Two kinds qualify, and they are transient for different reasons:
    ///
    /// * A connection-level failure — the pool was closed, an acquire timed
    ///   out, the connection dropped. The database itself is unreachable.
    /// * A transaction the backend refused because somebody else was writing:
    ///   `SQLITE_BUSY` / `BUSY_SNAPSHOT`, a PostgreSQL serialization failure or
    ///   deadlock, a MySQL lock-wait victim. `rg_db::is_retryable_transaction_error`
    ///   is the same predicate every retry loop in the workspace already turns
    ///   on, so a request that reaches a handler's `?` *after* its retry budget
    ///   ran out is classified by exactly what made it retry in the first place.
    ///
    /// Statement-level faults — a constraint violation, a type mismatch, a
    /// missing table — stay 500: those are bugs, and coming back changes
    /// nothing. Note that this leaves `DbErr::Exec`/`Query` on both sides of the
    /// line: the variant says which statement failed, only the backend's own
    /// error code says whether repeating it can help.
    ///
    /// Shared by the `From<DbErr>` and `From<anyhow::Error>` conversions so a
    /// transient failure classifies identically whether the handler surfaced
    /// the raw `DbErr` or an anyhow-wrapped one.
    ///
    /// `pub(crate)` so the transports that must keep their own error envelope,
    /// and therefore can't route through `AppError`, can reuse the exact same
    /// predicate — see `oci::oci_status_for`, `git_http::git_db_status` and the
    /// runner heartbeat middleware.
    pub(crate) fn is_db_retryable(e: &sea_orm::DbErr) -> bool {
        use sea_orm::DbErr;
        matches!(e, DbErr::Conn(_) | DbErr::ConnectionAcquire(_))
            || rg_db::is_retryable_transaction_error(e)
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

    pub fn payload_too_large(msg: impl std::fmt::Display) -> Self {
        Self::PayloadTooLarge(msg.to_string())
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

    /// Losing a race for an object nobody has done anything wrong with is a
    /// "come back", not a "we broke". `500` would tell git-lfs to give up on an
    /// upload that succeeds the moment the other publisher lets go.
    #[test]
    fn a_contended_lfs_publication_maps_to_503() {
        let busy = rg_core::lfs::service::LfsPublicationBusy {
            oid: "b".repeat(64),
            waited_seconds: 120,
        };
        let err: AppError = anyhow::Error::from(busy).context("store LFS object").into();
        assert_eq!(err.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    /// The other side of the retryable line, and the reason
    /// [`AppError::is_db_retryable`] asks the backend rather than the variant:
    /// `Exec` / `Query` carry *both* a contended transaction and a constraint
    /// violation, so widening the predicate to the whole variant would answer
    /// "come back later" to a request that will fail identically forever.
    /// A predicate mutated to accept everything reddens here.
    #[test]
    fn db_statement_error_stays_500() {
        // Exec/Query are statement-level (constraint/type/logic) — a bug here,
        // not a transient failure: no backend contention code under them.
        let err: AppError =
            DbErr::Exec(RuntimeErr::Internal("UNIQUE constraint failed".into())).into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(err.code(), "INTERNAL_ERROR");

        let err: AppError = DbErr::Query(RuntimeErr::Internal("no such column: x".into())).into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let err: AppError = DbErr::RecordNotInserted.into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);

        // And through the funnel every handler's `?` actually takes, wrapped in
        // the `.context(...)` layers `rg_db::ops` adds.
        let err: AppError = anyhow::Error::new(DbErr::Exec(RuntimeErr::Internal(
            "UNIQUE constraint failed: users.username".into(),
        )))
        .context("db: create user")
        .into();
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
        assert!(
            logged.contains("creating repository \"acme/widgets\""),
            "{logged}"
        );
        assert!(logged.contains("db: failed to insert repo"), "{logged}");
        assert!(
            logged.contains("UNIQUE constraint failed: repos.name"),
            "{logged}"
        );
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
        assert!(
            logged.contains("db: failed to list pending jobs"),
            "{logged}"
        );
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

    #[test]
    fn missing_repository_storage_names_the_path_and_repo_root_remedy() {
        let repo_path = std::path::Path::new("/srv/plombir-git/repos/acme/widgets.git");
        let error = repository_storage_missing(repo_path);
        let message = error.to_string();

        assert!(
            message.contains("/srv/plombir-git/repos/acme/widgets.git"),
            "{message}"
        );
        assert!(
            message.contains("repository directory is missing"),
            "{message}"
        );
        assert!(message.contains("[server].repo_root"), "{message}");
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

    /// The mirror of the test above, and the reason `InvalidRequest` exists: on
    /// the 400 branch the message is not a log line, it is the answer. A service
    /// that checked the caller's own file and knows exactly what is wrong with
    /// it — `rg_ci`'s config reader is the case this was written for — has to
    /// get that text out through the same funnel that sanitizes the 5xx bodies,
    /// or the caller is told "Internal server error" about their own typo.
    #[tokio::test]
    async fn invalid_request_reaches_the_client_as_a_400_that_keeps_its_reason() {
        use axum::response::IntoResponse;

        let err: AppError = rg_core::error::invalid_request(
            "job 'build' uses unsupported when: 'allways'; supported values are 'on_success' and 'manual'",
        )
        .context("failed to trigger pipeline")
        .into();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "BAD_REQUEST");

        let body = axum::body::to_bytes(err.into_response().into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        // Which job, which field, which value, and what is allowed instead —
        // all four have to survive the render, not just the status code.
        for expected in ["build", "when", "allways", "on_success"] {
            assert!(body.contains(expected), "missing {expected:?}: {body}");
        }
        assert!(
            !body.contains("Internal server error"),
            "a rejected request must not be reported as a server fault: {body}"
        );
    }

    #[tokio::test]
    async fn no_matching_ci_jobs_is_a_precise_manual_request_400() {
        use axum::response::IntoResponse;

        let err: AppError =
            anyhow::Error::new(rg_core::ci::NoMatchingCiJobs::new("refs/heads/feature")).into();
        assert_eq!(err.status(), StatusCode::BAD_REQUEST);
        assert_eq!(err.code(), "BAD_REQUEST");

        let body = axum::body::to_bytes(err.into_response().into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        for expected in ["refs/heads/feature", "only"] {
            assert!(body.contains(expected), "missing {expected:?}: {body}");
        }
        assert!(!body.contains("Internal server error"), "{body}");
    }

    /// The other half of that split, on the same shape of message. Without the
    /// `InvalidRequest` marker the identical text is *ours*, and the sanitizer
    /// is right to withhold it: a failure to read the repository must not be
    /// dressed up as advice about the caller's file.
    #[tokio::test]
    async fn an_unmarked_error_of_the_same_shape_stays_a_sanitized_500() {
        use axum::response::IntoResponse;

        let err: AppError = anyhow::anyhow!("failed to read CI config object .plombir-git-ci.yml")
            .context("failed to trigger pipeline")
            .into();
        assert_eq!(err.status(), StatusCode::INTERNAL_SERVER_ERROR);

        let body = axum::body::to_bytes(err.into_response().into_body(), 64 * 1024)
            .await
            .unwrap();
        let body = String::from_utf8(body.to_vec()).unwrap();
        assert!(body.contains("Internal server error"), "{body}");
        assert!(!body.contains(".plombir-git-ci.yml"), "{body}");
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

    /// A state problem is a 409 and keeps its own text — the client is told what
    /// to wait for, not that its request was malformed.
    #[test]
    fn core_conflict_maps_to_409_with_a_fixed_message() {
        let err = anyhow::Error::from(rg_core::error::Conflict::new(
            "another merge attempt is already in progress",
        ))
        .context("db: claim merge for pr 7");
        let app_err: AppError = err.into();

        assert_eq!(app_err.status(), StatusCode::CONFLICT);
        let AppError::Conflict(message) = &app_err else {
            panic!("expected Conflict, got {app_err:?}");
        };
        assert_eq!(message, "another merge attempt is already in progress");
        assert!(!message.contains("db:"), "{message}");
    }

    /// A policy refusal is a 403 and, like the other two markers, reaches the
    /// client as the rule's own text rather than the caller's context chain.
    #[test]
    fn core_forbidden_maps_to_403_with_a_fixed_message() {
        let err = anyhow::Error::from(rg_core::error::Forbidden::new(
            "branch 'main' requires all status checks to pass",
        ))
        .context("db: find pipeline for status check");
        let app_err: AppError = err.into();

        assert_eq!(app_err.status(), StatusCode::FORBIDDEN);
        let AppError::Forbidden(message) = &app_err else {
            panic!("expected Forbidden, got {app_err:?}");
        };
        assert_eq!(message, "branch 'main' requires all status checks to pass");
        assert!(!message.contains("db:"), "{message}");
    }

    /// Same ordering guarantee the `NotFound` branch has: an outage wrapped in a
    /// state or policy marker is still an outage. A 409 or 403 on a dead
    /// database would put the blame on the caller and nothing in the alerts.
    #[test]
    fn db_outage_wrapped_in_conflict_or_forbidden_context_stays_503() {
        let err = anyhow::Error::from(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
            .context(rg_core::error::Conflict::new("merge already in progress"));
        let app_err: AppError = err.into();
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);

        let err = anyhow::Error::from(DbErr::ConnectionAcquire(ConnAcquireErr::Timeout))
            .context(rg_core::error::Forbidden::new("branch is protected"));
        let app_err: AppError = err.into();
        assert_eq!(app_err.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn service_unavailable_response_does_not_leak_detail() {
        use axum::response::IntoResponse;
        let err = AppError::ServiceUnavailable("postgres://user:pw@host down".to_string());
        let resp = err.into_response();
        assert_eq!(resp.status(), StatusCode::SERVICE_UNAVAILABLE);
    }
}

#[cfg(test)]
mod api_rejection_envelope_tests {
    use super::*;
    use crate::route_table::DeclaredBodyLimit;

    async fn body_json(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).expect("the API envelope is JSON")
    }

    /// The response tower-http really writes: `413`, `text/plain`, and a body
    /// that names neither the endpoint nor the limit.
    fn transport_refusal(limit: Option<usize>) -> Response {
        let mut response = (
            StatusCode::PAYLOAD_TOO_LARGE,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "length limit exceeded",
        )
            .into_response();
        if let Some(bytes) = limit {
            response.extensions_mut().insert(DeclaredBodyLimit(bytes));
        }
        response
    }

    #[tokio::test]
    async fn a_transport_refusal_answers_in_the_api_envelope_and_names_the_limit() {
        let response = api_rejection_envelope(transport_refusal(Some(512 * 1024 * 1024))).await;

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "PAYLOAD_TOO_LARGE");
        assert!(
            body["error"]["message"]
                .as_str()
                .unwrap()
                .contains("512 MiB"),
            "the client is owed the limit it has to fit under, got: {body}"
        );
    }

    /// Without the stamp the envelope is still owed — the number is what is
    /// missing, not the contract.
    #[tokio::test]
    async fn a_refusal_carrying_no_declared_limit_still_gets_the_envelope() {
        let response = api_rejection_envelope(transport_refusal(None)).await;

        let body = body_json(response).await;
        assert_eq!(body["error"]["code"], "PAYLOAD_TOO_LARGE");
        assert!(!body["error"]["message"].as_str().unwrap().is_empty());
    }

    /// A handler that refused the payload itself already speaks the contract,
    /// and its message is more specific than anything this layer could write.
    #[tokio::test]
    async fn a_handlers_own_413_is_left_alone() {
        let handler_refusal =
            AppError::PayloadTooLarge("blob exceeds the 1 MiB blob API ceiling".to_string())
                .into_response();

        let response = api_rejection_envelope(handler_refusal).await;

        let body = body_json(response).await;
        assert_eq!(
            body["error"]["message"],
            "blob exceeds the 1 MiB blob API ceiling"
        );
    }

    #[tokio::test]
    async fn axum_router_and_extractor_refusals_receive_stable_codes() {
        for (status, expected_code) in [
            (StatusCode::BAD_REQUEST, "BAD_REQUEST"),
            (StatusCode::METHOD_NOT_ALLOWED, "METHOD_NOT_ALLOWED"),
            (StatusCode::UNSUPPORTED_MEDIA_TYPE, "UNSUPPORTED_MEDIA_TYPE"),
        ] {
            let refusal = (
                status,
                [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
                "axum rejection",
            )
                .into_response();

            let response = api_rejection_envelope(refusal).await;

            assert_eq!(response.status(), status);
            let body = body_json(response).await;
            assert_eq!(body["error"]["code"], expected_code);
            assert!(!body["error"]["message"].as_str().unwrap().is_empty());
        }
    }

    #[tokio::test]
    async fn a_method_refusal_keeps_allow_while_replacing_entity_headers() {
        let refusal = (
            StatusCode::METHOD_NOT_ALLOWED,
            [
                (header::CONTENT_TYPE, "text/plain; charset=utf-8"),
                (header::ALLOW, "POST"),
            ],
            "",
        )
            .into_response();

        let response = api_rejection_envelope(refusal).await;

        assert_eq!(response.headers().get(header::ALLOW).unwrap(), "POST");
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert!(response.headers().get(header::CONTENT_LENGTH).is_none());
    }

    #[tokio::test]
    async fn statuses_outside_axums_generic_refusals_pass_through_untouched() {
        let created = (StatusCode::CREATED, Body::from("{}")).into_response();

        let response = api_rejection_envelope(created).await;

        assert_eq!(response.status(), StatusCode::CREATED);

        let package_protocol_not_found = (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain; charset=utf-8")],
            "crate not found",
        )
            .into_response();
        let response = api_rejection_envelope(package_protocol_not_found).await;
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain; charset=utf-8"
        );
    }
}
