//! Regression coverage for card_2fc9c77364e8: a *failed* pull-request lookup
//! must not be reported as an *absent* pull request.
//!
//! Every `rg_core::pull_request::get_pr` call site in the HTTP layer used to
//! answer `404` to any error the service returned, and most of them put the
//! error's own text — `db: ...` context and all — into the response body. So a
//! database failure was indistinguishable from a deleted PR: the client's retry
//! logic gave up, the operator saw nothing, and internal detail leaked (H-05).
//!
//! The fix gives "genuinely absent" its own type (`rg_core::error::NotFound`),
//! which is the only thing `From<anyhow::Error> for AppError` turns into a 404.
//!
//! The outage here is simulated by dropping the `pull_requests` table rather
//! than by closing the pool: closing the pool makes the *authentication* lookup
//! fail first, so the request never reaches the `get_pr` call site under test
//! and the test would pass against the unfixed code. Dropping just that one
//! table lets auth and the repo lookup succeed and fails exactly the query the
//! card is about.

mod common;

use common::{create_repo, register_full, spawn_test_app_with_db};
use sea_orm::ConnectionTrait;

/// `GET /repos/{owner}/{name}/pulls/{number}` with a broken `pull_requests`
/// table must be a 5xx, and must not echo the database error to the client.
#[tokio::test]
async fn broken_pr_lookup_is_not_reported_as_a_missing_pr() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "prfail-owner", "prfail@example.com").await;
    create_repo(&base, &token, "prfail-repo").await;

    let url = format!("{base}/api/v1/repos/prfail-owner/prfail-repo/pulls/1");
    let client = reqwest::Client::new();

    // Baseline on a healthy database: the PR really is absent → 404 with the
    // fixed message. Without this the test below cannot tell "we fixed the
    // status" from "this endpoint 500s on everything".
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent PR is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "pull request not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    // Now break exactly the lookup the handler performs. Auth and the repo
    // resolution still work, so the request reaches the `get_pr` call site.
    db.execute_unprepared("DROP TABLE pull_requests")
        .await
        .expect("drop pull_requests");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a failed PR lookup must be a 5xx, not {status} — a 404 tells the \
         client the PR was deleted and puts nothing in the alerts (body: {body})"
    );

    // H-05: the sanitized funnel must be the only path to the client. The
    // unfixed code put `error.to_string()` — i.e. the `db: ...` context that
    // `rg_db::ops` wraps every failure in — straight into the body.
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("pull_requests"),
        "the response body must not carry internal error detail, got: {message}"
    );
}

/// The same distinction on a write path: `POST .../merge` is the only site that
/// runs the branch-protection check on the REST merge route, and it used to
/// swallow a failed `get_pr` entirely (`if let Ok(pr) = ...`) and fall through
/// to the merge with the check skipped.
#[tokio::test]
async fn broken_pr_lookup_on_merge_does_not_skip_the_protection_check() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "mergefail-owner", "mergefail@example.com").await;
    create_repo(&base, &token, "mergefail-repo").await;

    db.execute_unprepared("DROP TABLE pull_requests")
        .await
        .expect("drop pull_requests");

    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/mergefail-owner/mergefail-repo/pulls/1/merge"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .expect("request");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a failed PR lookup on the merge path must be a 5xx, not {status} \
         (body: {body})"
    );
}
