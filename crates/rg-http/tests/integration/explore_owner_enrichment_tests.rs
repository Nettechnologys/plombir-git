//! Regression coverage for card_f15f12e055d0: `GET /api/v1/repos/explore`
//! enriches every row with its owner's name, and used to collapse a failed
//! lookup into the literal string `"unknown"`.
//!
//! The collapse was `find_by_id(..).await.ok().flatten()`: a dead `users` table
//! and a deleted account produced the same answer, and it arrived under a `200`.
//! A client that links to `/{owner_name}/{name}` therefore advertised a live
//! account as an unknown one, with nothing in the response saying the listing
//! was degraded and no reason for anyone to retry.
//!
//! Why this needs a test of its own rather than a row in
//! [`super::failure_semantics_sweep_tests`]: that sweep keeps the `users` table
//! alive on **both** database passes — `session_standing_middleware` reads it on
//! every authenticated request, so a total outage answers `503` before any
//! handler runs. The one fault that reaches this collapse is exactly the one the
//! sweep cannot inject, and its healthy/broken differential would have read
//! `200`/`200` and called the route untouched.

use sea_orm::ConnectionTrait;

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn explore(base: &str) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/explore"))
        .send()
        .await
        .expect("explore request");
    let status = resp.status();
    let body = resp.json::<serde_json::Value>().await.expect("json body");
    (status, body)
}

/// Pull the single row of a one-repository listing, so a page that silently
/// went empty fails here instead of passing the assertions below vacuously.
fn only_row(body: &serde_json::Value) -> &serde_json::Value {
    let rows = body["data"].as_array().expect("explore data array");
    assert_eq!(
        rows.len(),
        1,
        "the fixture publishes exactly one public repository; without a row the \
         enrichment branch never runs and this test proves nothing (body: {body})"
    );
    &rows[0]
}

/// The whole point of the field: an account that exists is named.
#[tokio::test]
async fn explore_names_the_owner_of_a_public_repository() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "explown", "explown@example.com").await;
    create_repo(&base, &token, "explown-repo").await;

    let (status, body) = explore(&base).await;
    assert_eq!(status, 200, "baseline: a healthy explore answers");
    assert_eq!(
        only_row(&body)["owner_name"].as_str(),
        Some("explown"),
        "a live account must be listed under its own name (body: {body})"
    );
}

/// The honest absence. An account deleted after its repository was published
/// leaves a row nobody owns, and the chosen contract is an explicit `null`:
/// there is no owner to name, and the client renders its own placeholder rather
/// than being handed one that looks like a username.
#[tokio::test]
async fn explore_answers_null_for_a_repository_whose_owner_is_gone() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "explgone", "explgone@example.com").await;
    create_repo(&base, &token, "explgone-repo").await;

    // The repository row deliberately stays: this is the dangling owner, not a
    // cascade. Foreign keys are switched off on this one connection so the
    // delete lands the same way an out-of-band cleanup would leave it.
    db.execute_unprepared(&format!(
        "PRAGMA foreign_keys = OFF;\nDELETE FROM users WHERE id = {user_id};"
    ))
    .await
    .expect("delete owner row");

    let (status, body) = explore(&base).await;
    assert_eq!(status, 200, "an absent owner is not a server failure");
    let row = only_row(&body);
    assert!(
        row["owner_name"].is_null(),
        "a repository whose owner is gone must say so with null, not with a \
         name-shaped placeholder (row: {row})"
    );
}

/// The failure. The repository list has already succeeded, so the response is
/// half-built — and that is precisely when inventing the missing half is most
/// convincing. It must fail instead.
#[tokio::test]
async fn explore_fails_when_the_owner_lookup_cannot_run() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "explbork", "explbork@example.com").await;
    create_repo(&base, &token, "explbork-repo").await;

    let (status, body) = explore(&base).await;
    assert_eq!(status, 200, "baseline: the listing works before the fault");
    assert_eq!(
        only_row(&body)["owner_name"].as_str(),
        Some("explbork"),
        "baseline: the owner is named before the fault (body: {body})"
    );

    // Only `users` is taken away. `repositories` stays, so `list_public_paginated`
    // still succeeds and the request reaches the enrichment — the fault the
    // route-wide sweep cannot inject.
    db.execute_unprepared("PRAGMA foreign_keys = OFF;\nDROP TABLE users;")
        .await
        .expect("drop users");

    let (status, body) = explore(&base).await;
    assert!(
        status.is_server_error(),
        "a users lookup that could not run must fail the response, not ship a \
         placeholder owner under a 200 (status: {status}, body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("no such table"),
        "the explore failure must not carry internal error detail, got: {message}"
    );
}
