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
//! means installing the process-global Prometheus registry, and a process-global
//! installation is a decision one test makes for every other test in the binary
//! — which is exactly how three of them ended up passing only when run alone
//! (card_00b2bd65060e). The route-access sweep now makes that decision on
//! purpose and judges `GET /metrics` under it, and the tests that read metric
//! counters live in their own binary (`tests/metrics`). Since both routers here
//! share one stack (`routes::apply_middleware`), the metrics layer cannot go
//! missing from the test router without going missing from production too, and
//! that is the guarantee that was wanted.

use std::sync::Arc;

use crate::common::{build_test_app_state, setup_test_db, spawn_test_app, wait_for_listener};

/// Every header `security_headers_middleware` promises, checked on a live
/// response from the real router.
pub(crate) fn assert_security_headers(headers: &reqwest::header::HeaderMap, what: &str) {
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

fn extract_between<'a>(haystack: &'a str, start_marker: &str, end_marker: &str) -> &'a str {
    let start = haystack
        .find(start_marker)
        .unwrap_or_else(|| panic!("missing marker {start_marker:?} in {haystack}"))
        + start_marker.len();
    let rest = &haystack[start..];
    let end = rest.find(end_marker).unwrap_or_else(|| {
        panic!("missing marker {end_marker:?} after {start_marker:?} in {haystack}")
    });
    &rest[..end]
}

/// Both routers carry the SPA fallback now (card_dd8497e4fd58), but this test
/// stays on the production one: the nonce it checks is written by a layer in
/// the production stack, so driving that stack is what makes the assertion
/// about production. It catches both halves of the contract: fallback mounted,
/// and the exact CSP nonce injected into `index.html`.
#[tokio::test]
async fn spa_fallback_uses_the_same_nonce_in_html_and_csp() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");

    let spa_build_dir = dir.path().join("spa-build");
    std::fs::create_dir_all(&spa_build_dir).expect("create SPA build dir");
    std::fs::write(
        spa_build_dir.join("index.html"),
        r#"<!doctype html><script>window.__fk=1</script><script type="module">boot()</script>"#,
    )
    .expect("write SPA fixture");

    let mut state = build_test_app_state(db, repo_root);
    state.spa_build_dir = Arc::new(spa_build_dir);
    let app = rg_http::create_router_for_test_with_static_files(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    wait_for_listener(&addr.to_string()).await;

    let response = reqwest::get(format!("http://{addr}/dashboard"))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "production SPA fallback must answer client-side routes"
    );
    let csp = response
        .headers()
        .get("content-security-policy")
        .expect("fallback response carries CSP")
        .to_str()
        .unwrap()
        .to_string();
    let body = response.text().await.unwrap();

    let csp_nonce = extract_between(&csp, "'nonce-", "'");
    let html_nonce = extract_between(&body, "nonce=\"", "\"");
    assert_eq!(
        html_nonce, csp_nonce,
        "the browser only runs the SPA bootstrap when body and header nonces match"
    );
    assert_eq!(
        body.matches(&format!("nonce=\"{html_nonce}\"")).count(),
        2,
        "every bootstrap script tag in index.html must receive the CSP nonce"
    );
}

/// The same contract on `/` — the path the previous test could not have caught.
///
/// `ServeDir` used to answer the root as a *directory* by handing out
/// `build/index.html` off disk, before the handler that injects the nonce ever
/// ran. The shell arrived intact but un-nonced, the browser refused to execute
/// the SvelteKit bootstrap under `script-src 'self' 'nonce-...'`, and the
/// landing page of the whole app rendered blank — with a 200, no failed
/// request, and nothing in the server log. `/dashboard` matched no file on
/// disk, so it fell through to the fallback and looked healthy throughout.
/// Hence a separate test pinned to `/` rather than one more client-side route.
#[tokio::test]
async fn the_root_path_serves_the_shell_with_a_nonce_not_the_raw_file() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");

    let spa_build_dir = dir.path().join("spa-build");
    std::fs::create_dir_all(&spa_build_dir).expect("create SPA build dir");
    std::fs::write(
        spa_build_dir.join("index.html"),
        r#"<!doctype html><script>window.__fk=1</script><script type="module">boot()</script>"#,
    )
    .expect("write SPA fixture");

    let mut state = build_test_app_state(db, repo_root);
    state.spa_build_dir = Arc::new(spa_build_dir);
    let app = rg_http::create_router_for_test_with_static_files(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
        )
        .await
        .unwrap();
    });
    wait_for_listener(&addr.to_string()).await;

    let response = reqwest::get(format!("http://{addr}/")).await.unwrap();
    assert_eq!(response.status(), 200, "the root path must serve the shell");
    let csp = response
        .headers()
        .get("content-security-policy")
        .expect("root response carries CSP")
        .to_str()
        .unwrap()
        .to_string();
    let body = response.text().await.unwrap();

    let csp_nonce = extract_between(&csp, "'nonce-", "'");
    let html_nonce = extract_between(&body, "nonce=\"", "\"");
    assert_eq!(
        html_nonce, csp_nonce,
        "a shell served off disk carries no nonce, and the browser then blocks the bootstrap"
    );
    assert_eq!(
        body.matches(&format!("nonce=\"{html_nonce}\"")).count(),
        2,
        "every bootstrap script tag on the landing page must receive the CSP nonce"
    );
}
