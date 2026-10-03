//! Error-envelope selection for middleware mounted above every protocol router.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// Answer a pre-router refusal in the envelope of the subtree it interrupted.
///
/// Middleware on the assembled router runs before Axum chooses the REST, OCI,
/// or git handler. A response built there therefore has to make that choice
/// itself: REST clients consume [`crate::error::ErrorResponse`], registry
/// clients consume the distribution-spec `{errors:[...]}` body, and git-HTTP
/// clients expect plain text. The API response stays lazy because constructing
/// an [`crate::error::AppError`] may log a sanitized internal failure, and that
/// side effect belongs only to the API branch that actually returns it.
pub(crate) fn pre_router_refusal_response(
    path: &str,
    status: StatusCode,
    api_response: impl FnOnce() -> Response,
    oci_code: &str,
    message: &str,
) -> Response {
    if crate::routes::is_inside(path, "/api/v1") {
        let response = api_response();
        debug_assert_eq!(response.status(), status);
        return response;
    }

    if path == "/v2" || path == "/v2/" || crate::routes::is_inside(path, "/v2") {
        return crate::oci::oci_refusal_response(status, oci_code, message);
    }

    let mut response = (status, message.to_string()).into_response();
    if status == StatusCode::UNAUTHORIZED {
        response.headers_mut().insert(
            header::WWW_AUTHENTICATE,
            HeaderValue::from_static("Basic realm=\"Plombir Git\""),
        );
    }
    response
}
