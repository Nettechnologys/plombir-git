//! The security headers have to be on the router, not just on a router
//! (card_17ea3d843ca7).
//!
//! `security_headers_middleware` was already covered in `security.rs`, but on a
//! `Router::new()` the test built itself: the behaviour of the layer was proven,
//! the fact that the server carries it was not. Deleting the layer from
//! `build_router` left every test green. These tests drive the router the rest
//! of the suite drives, so the mounting itself is what is under test.
//!
//! The metrics layer has no test of its own on purpose: proving it is mounted
//! means installing the process-global Prometheus registry, and the route-access
//! sweep asserts `GET /metrics` answers 503 precisely because nothing installs
//! it — one test would then decide the other's outcome. Since both routers now
//! share one stack (`routes::apply_middleware`), the metrics layer cannot go
//! missing from the test router without going missing from production too, and
//! that is the guarantee that was wanted.

use crate::common::spawn_test_app;

/// Every header `security_headers_middleware` promises, checked on a live
/// response from the real router.
fn assert_security_headers(headers: &reqwest::header::HeaderMap, what: &str) {
    let header = |name: &str| headers.get(name).and_then(|v| v.to_str().ok());

    assert_eq!(
        header("x-content-type-options"),
        Some("nosniff"),
        "{what}: the security headers layer is not mounted in the router"
    );
    assert_eq!(
        header("x-frame-options"),
        Some("DENY"),
        "{what}: no clickjacking protection"
    );
    assert_eq!(
        header("referrer-policy"),
        Some("strict-origin-when-cross-origin"),
        "{what}: referrer policy missing"
    );

    let csp = headers
        .get("content-security-policy")
        .unwrap_or_else(|| panic!("{what}: no content-security-policy"))
        .to_str()
        .unwrap();
    assert!(
        csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"),
        "{what}: CSP is present but not the one the middleware builds: {csp}"
    );
    assert!(
        csp.contains("'nonce-"),
        "{what}: CSP carries no per-request nonce, so the SPA would need 'unsafe-inline': {csp}"
    );

    assert!(
        headers.get("permissions-policy").is_some(),
        "{what}: permissions policy missing"
    );
    assert!(
        headers.get("cross-origin-opener-policy").is_some(),
        "{what}: cross-origin isolation headers missing"
    );
}

#[tokio::test]
async fn the_router_carries_the_security_headers_layer() {
    let base = spawn_test_app().await;

    let resp = reqwest::get(format!("{base}/health")).await.unwrap();
    assert_eq!(resp.status(), 200);
    assert_security_headers(resp.headers(), "GET /health");
}

/// A request the server refuses still travels back out through the header
/// layer — the session gate sits inside it, so a 401 is headed like a 200.
/// If the layer ever moves inside the gate, this is what notices.
#[tokio::test]
async fn a_rejected_request_is_answered_with_the_security_headers_too() {
    let base = spawn_test_app().await;

    let resp = reqwest::get(format!("{base}/api/v1/users/me"))
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        401,
        "an unauthenticated request to a user route should be rejected"
    );
    assert_security_headers(resp.headers(), "unauthenticated GET /api/v1/users/me");
}
