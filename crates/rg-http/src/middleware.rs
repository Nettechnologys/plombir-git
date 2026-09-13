//! Shared HTTP middleware for ForgeKeep.

use axum::body::{to_bytes, Body};
use axum::extract::{MatchedPath, Request, State};
use axum::http::Method;
use axum::http::{header, HeaderName, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Json, Response};
use std::time::Instant;

/// Per-request unique ID, generated from `X-Request-Id` header or a fresh UUID.
#[derive(Clone, Debug)]
pub struct RequestId(pub String);

static X_REQUEST_ID: HeaderName = HeaderName::from_static("x-request-id");

/// HTTP metrics middleware - records request count, duration, and in-flight count.
///
/// Uses `MatchedPath` to normalize route templates like `/users/{id}` instead of
/// concrete paths like `/users/123` to avoid label cardinality explosion.
pub async fn http_metrics_middleware(request: Request, next: Next) -> Response {
    use crate::metrics::http_requests::{IN_FLIGHT, REQUEST_COUNT, REQUEST_DURATION};

    let start = Instant::now();

    // Increment in-flight counter
    if let Some(g) = IN_FLIGHT.get() {
        g.inc();
    }

    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let response = next.run(request).await;

    let elapsed = start.elapsed().as_secs_f64();
    let status = response.status().as_u16().to_string();

    // Decrement in-flight counter
    if let Some(g) = IN_FLIGHT.get() {
        g.dec();
    }

    // Record metrics
    if let Some(c) = REQUEST_COUNT.get() {
        c.with_label_values(&[method.as_str(), &route, &status])
            .inc();
    }
    if let Some(h) = REQUEST_DURATION.get() {
        h.with_label_values(&[&route]).observe(elapsed);
    }

    response
}

/// Middleware that generates (or propagates) a per-request unique ID.
///
/// - If the client sends `X-Request-Id`, that value is reused.
/// - Otherwise, a fresh v4 UUID is generated.
/// - The ID is stored in request extensions and added to the response header.
/// - For 4xx/5xx JSON error responses, the request_id is also injected into the body.
pub async fn request_id_middleware(mut request: Request, next: Next) -> Response {
    let request_id = request
        .headers()
        .get(&X_REQUEST_ID)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    let request_id = if request_id.is_empty() {
        uuid::Uuid::new_v4().to_string()
    } else {
        request_id.to_string()
    };

    // Store in extensions so handlers can access it if needed
    request
        .extensions_mut()
        .insert(RequestId(request_id.clone()));

    let mut response = next.run(request).await;

    // Add X-Request-Id to response header
    if let Ok(val) = request_id.parse() {
        response.headers_mut().insert(X_REQUEST_ID.clone(), val);
    }

    // Inject request_id into JSON error response bodies
    if (response.status().is_client_error() || response.status().is_server_error())
        && is_json_response(&response)
    {
        return inject_request_id(response, &request_id).await;
    }

    response
}

fn is_json_response(response: &Response) -> bool {
    response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .map(|ct| ct.starts_with("application/json"))
        .unwrap_or(false)
}

/// Inject request_id into a JSON response body. Returns the (possibly modified) response.
/// If body reading or JSON parsing fails, returns the response with the original body preserved
/// when possible, or empty body on stream errors.
async fn inject_request_id(response: Response, request_id: &str) -> Response {
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(b) => b,
        Err(_) => return Response::from_parts(parts, Body::empty()),
    };

    let mut json: serde_json::Value = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => {
            // Not valid JSON, restore original body
            let body = Body::from(bytes);
            return Response::from_parts(parts, body);
        }
    };

    // Inject request_id into error body if present
    if let Some(error_obj) = json.get_mut("error") {
        if let Some(obj) = error_obj.as_object_mut() {
            obj.insert(
                "request_id".to_string(),
                serde_json::Value::String(request_id.to_string()),
            );
        }
    }

    let modified = serde_json::to_vec(&json).unwrap_or_else(|_| bytes.to_vec());
    parts.headers.insert(
        header::CONTENT_LENGTH,
        HeaderValue::from_str(&modified.len().to_string()).unwrap_or_else(|_| HeaderValue::from(0)),
    );
    Response::from_parts(parts, Body::from(modified))
}

/// Whether the request is read-only even when its HTTP method normally is not.
///
/// Git Smart HTTP and LFS put their operation in the path or JSON body, so
/// treating every POST as a write blocks clone/fetch and LFS downloads. The LFS
/// batch handler performs the second half of the decision after Axum has parsed
/// the body: this outer layer must let the shared batch endpoint through without
/// buffering an untrusted body ahead of the ordinary limits and access gates.
fn is_read_request(method: &Method, path: &str) -> bool {
    method == Method::GET
        || method == Method::HEAD
        || method == Method::OPTIONS
        || (method == Method::POST && is_protocol_read_post(path))
}

fn is_protocol_read_post(path: &str) -> bool {
    let segments = path
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>();
    matches!(
        segments.as_slice(),
        [_, _, "git-upload-pack"]
            | ["git", _, _, "git-upload-pack"]
            | ["api", "v1", "repos", _, _, "lfs", "objects", "batch"]
    )
}

/// Build the canonical response for a write rejected by maintenance mode.
pub(crate) fn maintenance_response() -> Response {
    with_maintenance_retry_after(maintenance_api_response())
}

fn maintenance_api_response() -> Response {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        Json(crate::error::ErrorResponse {
            error: crate::error::ErrorBody {
                code: "MAINTENANCE_MODE",
                message: "Instance is in maintenance mode. Read-only access only.".to_string(),
                request_id: None,
            },
        }),
    )
        .into_response()
}

fn maintenance_response_for_path(path: &str) -> Response {
    let message = "Instance is in maintenance mode. Read-only access only.";
    let response = crate::refusal::pre_router_refusal_response(
        path,
        StatusCode::SERVICE_UNAVAILABLE,
        maintenance_api_response,
        "UNAVAILABLE",
        message,
    );
    with_maintenance_retry_after(response)
}

fn with_maintenance_retry_after(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::RETRY_AFTER, HeaderValue::from_static("120"));
    response
}

/// Maintenance mode middleware — rejects mutating requests when the instance
/// is in read-only maintenance mode. Safe HTTP methods, protocol requests whose
/// operation is read-only, and the admin panel are always allowed.
///
/// A rejection is a `503 Service Unavailable`, not a 200 carrying an error body:
/// the request did not happen, and a client — a browser, `git`, a CI runner —
/// decides whether to retry from the status line, not by parsing the body. The
/// `Retry-After` hint is deliberately conservative; the instance cannot know how
/// long the maintenance will last, only that retrying immediately is pointless.
pub async fn maintenance_middleware(
    State(state): State<crate::AppState>,
    request: Request,
    next: Next,
) -> Response {
    let settings = state.instance_settings.get(&state.db).await;
    if settings.maintenance_mode {
        let method = request.method();
        let path = request.uri().path();

        let is_admin = path.starts_with("/api/v1/admin/");

        if !is_read_request(method, path) && !is_admin {
            return maintenance_response_for_path(path);
        }
    }
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::is_read_request;
    use axum::http::Method;

    #[test]
    fn protocol_intent_not_post_alone_decides_maintenance_reads() {
        for path in [
            "/git/alice/repo/git-upload-pack",
            "/alice/repo.git/git-upload-pack",
            "/api/v1/repos/alice/repo/lfs/objects/batch",
        ] {
            assert!(
                is_read_request(&Method::POST, path),
                "{path} must reach the protocol-aware handler"
            );
        }

        for path in [
            "/git/alice/repo/git-receive-pack",
            "/alice/repo.git/git-receive-pack",
            "/api/v1/admin/git-upload-pack",
            "/unrelated/lfs/objects/batch",
            "/api/v1/users/login",
            "/api/v1/repos",
        ] {
            assert!(
                !is_read_request(&Method::POST, path),
                "{path} must remain blocked as a write"
            );
        }
    }
}
