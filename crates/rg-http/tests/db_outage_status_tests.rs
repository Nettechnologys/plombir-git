//! Regression coverage for card_d44bb577daf5: HTTP handlers must classify a
//! database outage as 503 (retryable) rather than collapsing it into a 500.
//!
//! Before the fix, handlers converted `sea_orm::DbErr` by hand through
//! `AppError::internal(error)` (always 500), bypassing the `From<DbErr>`
//! classification in `error.rs` that maps connection-level failures to 503.
//! This test drives a real handler with a closed connection pool and asserts
//! the response is `503 SERVICE_UNAVAILABLE`, proving the handler now routes
//! the error through `AppError::from`.

mod common;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;

use common::{build_test_app_state, setup_test_db};

/// A connection-level `DbErr` raised inside `runners::download_workspace`
/// (the handler named in the card) must surface as 503, not 500.
#[tokio::test]
async fn db_outage_in_handler_returns_503_not_500() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let state = build_test_app_state(db.clone(), repo_root);

    // Simulate a database outage: close the shared connection pool. Every
    // subsequent query from the handler's cloned handle fails at the
    // connection level (pool closed) — exactly the retryable-outage case.
    db.close().await.expect("close pool");

    // The very first thing `download_workspace` does is a DB lookup, so the
    // closed pool short-circuits into the error arm we converted.
    let response = rg_http::api::runners::download_workspace(State(state), Path((1_i64, 1_i64)))
        .await
        .into_response();

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "database outage must map to 503, not 500"
    );
}

/// Peripheral of the same class (card_2e5ab5773b6f): the package-registry
/// handlers converted DB errors through a local `err()` helper (which stringifies
/// into `AppError::internal` → 500) instead of `AppError::from`, and their shared
/// `resolve_repo` gateway used `.map_err(AppError::internal)`. A DB outage on a
/// packages route (here `list_packages`, whose first action is the `resolve_repo`
/// DB lookup) must now surface as 503, not 500.
#[tokio::test]
async fn db_outage_in_packages_handler_returns_503_not_500() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let state = build_test_app_state(db.clone(), repo_root);

    // Close the pool to simulate a connection-level outage.
    db.close().await.expect("close pool");

    let response = rg_http::api::packages::list_packages(
        State(state),
        axum::http::HeaderMap::new(),
        Path(("owner".to_string(), "repo".to_string(), "cargo".to_string())),
    )
    .await
    .into_response();

    assert_eq!(
        response.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "database outage on a packages route must map to 503, not 500"
    );
}
