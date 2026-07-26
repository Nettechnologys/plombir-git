//! Regression coverage for card_52bc6f5e394d: a *failed* issue lookup must not
//! be reported as an *absent* issue.
//!
//! The same defect the PR endpoints had (see `pr_lookup_failure_status_tests`),
//! one service over: `rg_core::issue::get_issue` reported "no such issue" and
//! "the query failed" as the same flattened `anyhow::Error`, and every HTTP call
//! site answered `404` to both — most of them echoing the error's own text, i.e.
//! the `db: ...` context `rg_db::ops` wraps every failure in, straight into the
//! response body (H-05).
//!
//! The outage is simulated by dropping the `issues` table rather than by closing
//! the pool: a closed pool fails the *authentication* lookup first, so the
//! request never reaches the call site under test and the assertions would pass
//! against the unfixed code. Dropping that one table leaves auth and the repo
//! resolution working and fails exactly the query these endpoints are about.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use sea_orm::ConnectionTrait;

/// `GET /repos/{owner}/{name}/issues/{number}` — the read path.
#[tokio::test]
async fn broken_issue_lookup_is_not_reported_as_a_missing_issue() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "issuefail-owner", "issuefail@example.com").await;
    create_repo(&base, &token, "issuefail-repo").await;

    let url = format!("{base}/api/v1/repos/issuefail-owner/issuefail-repo/issues/1");
    let client = reqwest::Client::new();

    // Baseline on a healthy database: the issue really is absent → 404 with the
    // fixed message. Without it the assertion below cannot tell "we fixed the
    // status" from "this endpoint 500s on everything".
    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent issue is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "issue not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    // Break exactly the lookup the handler performs.
    db.execute_unprepared("DROP TABLE issues")
        .await
        .expect("drop issues");

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
        "a failed issue lookup must be a 5xx, not {status} — a 404 tells the \
         client the issue was deleted and puts nothing in the alerts (body: {body})"
    );

    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("issues"),
        "the response body must not carry internal error detail, got: {message}"
    );
}

/// `PATCH /repos/{owner}/{name}/issues/{number}` — the write path resolves the
/// issue first to authorize the edit, and used to call that resolution's failure
/// a 404 as well.
#[tokio::test]
async fn broken_issue_lookup_on_update_is_not_reported_as_a_missing_issue() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "issuepatch-owner", "issuepatch@example.com").await;
    create_repo(&base, &token, "issuepatch-repo").await;

    db.execute_unprepared("DROP TABLE issues")
        .await
        .expect("drop issues");

    let resp = reqwest::Client::new()
        .patch(format!(
            "{base}/api/v1/repos/issuepatch-owner/issuepatch-repo/issues/1"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({"title": "renamed"}))
        .send()
        .await
        .expect("request");

    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a failed issue lookup on the update path must be a 5xx, not {status} \
         (body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("issues"),
        "the response body must not carry internal error detail, got: {message}"
    );
}

/// `GET /repos/{owner}/{name}/issues/{number}/labels` and the time-tracking
/// endpoints resolve the issue through the same service call, and collapsed its
/// failure the same way.
#[tokio::test]
async fn broken_issue_lookup_on_derived_endpoints_is_not_a_404() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "issueaux-owner", "issueaux@example.com").await;
    create_repo(&base, &token, "issueaux-repo").await;

    let client = reqwest::Client::new();
    let prefix = format!("{base}/api/v1/repos/issueaux-owner/issueaux-repo/issues/1");

    // Baseline: both really are 404 while the database is healthy.
    for suffix in ["/labels", "/time"] {
        let resp = client
            .get(format!("{prefix}{suffix}"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        assert_eq!(
            resp.status(),
            404,
            "an absent issue is still a 404 on {suffix}"
        );
    }

    db.execute_unprepared("DROP TABLE issues")
        .await
        .expect("drop issues");

    for suffix in ["/labels", "/time"] {
        let resp = client
            .get(format!("{prefix}{suffix}"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert!(
            status.is_server_error(),
            "a failed issue lookup on {suffix} must be a 5xx, not {status} (body: {body})"
        );
        let message = body["error"]["message"].as_str().unwrap_or_default();
        assert!(
            !message.contains("db:") && !message.contains("issues"),
            "the {suffix} response body must not carry internal error detail, got: {message}"
        );
    }
}
