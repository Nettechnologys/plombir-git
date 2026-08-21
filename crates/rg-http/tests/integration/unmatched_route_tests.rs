//! What answers a path no route claims (card_dd8497e4fd58).
//!
//! `build_docs_routes` used to attach its authentication gate with
//! `Router::layer`, which wraps a router's fallback along with its routes. That
//! sub-router is merged into the tree, so its wrapped fallback became the
//! tree's, and every unmatched path answered `401 api docs requires
//! authentication`. Production hid it behind the SPA fallback; the test router,
//! which had no fallback at all, showed it — so a route that had gone missing
//! looked like an authorization problem to whoever was debugging one, and any
//! future "unknown path → 404" test would have passed for the wrong reason.
//!
//! Both halves are pinned here: the status an unmatched path answers with, and
//! the production behaviour behind it — an unmatched path is not a 404 when a
//! bundle is on disk, it is the SPA shell.
//!
//! That shell used to be the answer *everywhere*, `/api/v1/...` and `/v2/...`
//! included, which is how a lost package-registry route handed a package client
//! a page of HTML (card_df4547b3c6f8). A path inside a protocol subtree now
//! answers in that protocol's own envelope instead, and the tests below hold
//! both ends: the API and the registry refuse, the pages still load. A fix that
//! simply deleted the fallback would pass one end and fail the other.

use std::sync::Arc;

use crate::common::{build_test_app_state, setup_test_db, spawn_test_app, wait_for_listener};

#[tokio::test]
async fn an_unmatched_path_is_not_an_authorization_problem() {
    let base = spawn_test_app().await;

    for path in ["/api/v1/definitely-not-a-route", "/not-a-route-either"] {
        let response = reqwest::get(format!("{base}{path}")).await.unwrap();
        let status = response.status();
        let body = response.text().await.unwrap();

        assert_eq!(
            status, 404,
            "{path} is claimed by no route and must answer as such, got {status}: {body}"
        );
        // The body the gate would answer with, whatever gate it is: the layer
        // said "api docs requires authentication", `AuthUser` says
        // "authentication required". The status above is the real assertion;
        // this one names the symptom so a regression reads as itself.
        assert!(
            !body.contains("authentication"),
            "{path} answered with an authentication gate: {body}"
        );
    }
}

/// The docs gate must still be a gate — moving it off the sub-router, first
/// onto its four routes and then into the handlers' signatures, is only correct
/// if those routes are still shut.
///
/// The rest of that contract (a cookie session, a JWT and a PAT all open it) is
/// `openapi_docs_auth_tests`; this asserts the one thing that would break if
/// the gate went missing entirely.
#[tokio::test]
async fn the_api_docs_gate_survived_moving_onto_its_routes() {
    let base = spawn_test_app().await;

    for path in ["/api-docs/openapi.json", "/api-docs/"] {
        let response = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert_eq!(response.status(), 401, "{path} must require a session");
    }
}

/// A server with a real bundle behind it, which is the only configuration in
/// which the shell can be handed out at all — and therefore the only one in
/// which "the API answers with a page" can be observed or refuted.
async fn app_with_a_spa_bundle() -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create test repo root");

    let spa_build_dir = dir.path().join("spa-build");
    std::fs::create_dir_all(&spa_build_dir).expect("create SPA build dir");
    std::fs::write(
        spa_build_dir.join("index.html"),
        r#"<!doctype html><title>forgekeep spa shell</title>"#,
    )
    .expect("write SPA fixture");

    let mut state = build_test_app_state(db, repo_root);
    state.spa_build_dir = Arc::new(spa_build_dir);
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;

    format!("http://{addr}")
}

/// With a bundle on disk a page the server does not route is still the SPA
/// shell — the client-side router owns those paths, and taking the fallback
/// away to sharpen the API's diagnostics would break every deep link.
#[tokio::test]
async fn a_page_the_server_does_not_route_is_still_the_spa_shell() {
    let base = app_with_a_spa_bundle().await;

    for path in ["/dashboard", "/definitely-not-a-route"] {
        let response = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert_eq!(
            response.status(),
            200,
            "{path} must be answered by the SPA shell"
        );
        let body = response.text().await.unwrap();
        assert!(
            body.contains("forgekeep spa shell"),
            "{path} was answered by something other than index.html: {body}"
        );
    }
}

/// The other end, and the defect itself: a path inside a protocol subtree is
/// answered in that protocol's envelope even with a bundle sitting right there
/// ready to be served. Asserted with the bundle present on purpose — without it
/// the shell cannot be handed out anyway, so a green run would prove nothing.
#[tokio::test]
async fn a_path_no_route_claims_under_a_protocol_prefix_never_answers_with_the_shell() {
    let base = app_with_a_spa_bundle().await;

    // The REST API: `AppError`'s JSON envelope, the one every other error on
    // `/api/v1` arrives in.
    let response = reqwest::get(format!("{base}/api/v1/no-such-endpoint"))
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    assert_eq!(
        response
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .map(|value| value.starts_with("application/json")),
        Some(true),
        "an API client was handed something other than JSON"
    );
    let body: serde_json::Value = response.json().await.expect("a JSON error body");
    assert_eq!(body["error"]["code"], "NOT_FOUND", "body: {body}");

    // The registry: docker and podman read `{errors:[{code,message}]}` and
    // nothing else, so the API's own envelope would be no better than the HTML.
    let response = reqwest::get(format!("{base}/v2/no/such/registry/path"))
        .await
        .unwrap();
    assert_eq!(response.status(), 404);
    let body: serde_json::Value = response.json().await.expect("a JSON error body");
    assert_eq!(body["errors"][0]["code"], "UNSUPPORTED", "body: {body}");
}
