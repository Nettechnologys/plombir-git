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
//! bundle is on disk, it is the SPA shell, which is how a lost package-registry
//! route hands a package client a page of HTML.

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
        assert!(
            !body.contains("api docs requires authentication"),
            "{path} answered with the API-docs gate: {body}"
        );
    }
}

/// The docs gate must still be a gate — moving it from the sub-router onto its
/// four routes is only correct if those routes are still shut.
///
/// The rest of that contract (a JWT and a PAT both open it) is
/// `openapi_docs_auth_tests`; this asserts the one thing that would break if
/// the layer went missing entirely.
#[tokio::test]
async fn the_api_docs_gate_survived_moving_onto_its_routes() {
    let base = spawn_test_app().await;

    for path in ["/api-docs/openapi.json", "/api-docs/"] {
        let response = reqwest::get(format!("{base}{path}")).await.unwrap();
        assert_eq!(response.status(), 401, "{path} must require a bearer token");
    }
}

/// With a bundle on disk an unmatched path is answered by the SPA shell, not by
/// a 404 — the mechanism that turns a lost registry route into "the client got
/// HTML". The test router has to be able to reproduce it, which is why the
/// fallback is part of the shared build rather than production's alone.
#[tokio::test]
async fn an_unmatched_path_falls_through_to_the_spa_shell_when_a_bundle_exists() {
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

    let response = reqwest::get(format!("http://{addr}/api/v1/definitely-not-a-route"))
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        200,
        "with a bundle present the fallback serves the SPA shell"
    );
    let body = response.text().await.unwrap();
    assert!(
        body.contains("forgekeep spa shell"),
        "the unmatched path was answered by something other than index.html: {body}"
    );
}
