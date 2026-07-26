//! Regression coverage for card_7b22d6892212: a *failed* review lookup must not
//! be reported as an *absent* review.
//!
//! Same defect as `pr_lookup_failure_status_tests` and
//! `issue_lookup_failure_status_tests`, one service further along:
//! `rg_core::review::service` flattened "no such review" and "the query failed"
//! into one `anyhow::Error`, and the handlers answered `404` to both — passing
//! the error's own `db: ...` text to the client along the way (H-05).
//!
//! Each test drops exactly the table the endpoint under test reads, so
//! authentication, the repo resolution and the PR lookup all still succeed and
//! only the query the card is about fails. Closing the pool instead would fail
//! authentication first and the assertions would pass unfixed.

use crate::common::{register_full, spawn_test_app_with_db};
use chrono::Utc;
use sea_orm::{ConnectionTrait, Set};

async fn insert_pr(db: &sea_orm::DatabaseConnection, repo_id: i64, author_id: i64, number: i64) {
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(number),
            title: Set(format!("PR {number}")),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some("0".repeat(40))),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            merged_at: Set(None),
            closed_at: Set(None),
        },
    )
    .await
    .expect("insert pr");
}

async fn repo_with_pr(prefix: &str) -> (String, sea_orm::DatabaseConnection, String) {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, &format!("{prefix}-owner"), &format!("{prefix}@example.com")).await;
    let repo_id = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": format!("{prefix}-repo")}))
        .send()
        .await
        .expect("create repo")
        .json::<serde_json::Value>()
        .await
        .expect("json")["id"]
        .as_i64()
        .expect("repo id");
    insert_pr(&db, repo_id, user_id, 1).await;
    (base, db, token)
}

fn assert_broken_lookup(status: reqwest::StatusCode, body: &serde_json::Value, what: &str) {
    assert!(
        status.is_server_error(),
        "a failed {what} lookup must be a 5xx, not {status} — a 404 tells the \
         client the row was deleted and puts nothing in the alerts (body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("pr_reviews") && !message.contains("review_comments"),
        "the {what} response body must not carry internal error detail, got: {message}"
    );
}

/// `GET .../reviews/{id}` and `POST .../reviews/{id}/dismiss` both go through
/// `rg_core::review::service::get_review`.
#[tokio::test]
async fn broken_review_lookup_is_not_reported_as_a_missing_review() {
    let (base, db, token) = repo_with_pr("reviewfail").await;
    let client = reqwest::Client::new();
    let pr_url = format!("{base}/api/v1/repos/reviewfail-owner/reviewfail-repo/pulls/1");

    // Baseline on a healthy database: the review really is absent → 404 with the
    // fixed message, so the assertions below cannot be satisfied by an endpoint
    // that 500s on everything.
    let resp = client
        .get(format!("{pr_url}/reviews/1"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent review is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "review not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    db.execute_unprepared("DROP TABLE pr_reviews")
        .await
        .expect("drop pr_reviews");

    let resp = client
        .get(format!("{pr_url}/reviews/1"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_broken_lookup(status, &body, "review");

    let resp = client
        .post(format!("{pr_url}/reviews/1/dismiss"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"message": "stale"}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_broken_lookup(status, &body, "review dismissal");
}

/// `PATCH .../comments/{id}/resolution` resolves the thread root, which used to
/// discard the error entirely (`Err(_) => not_found("review thread not found")`).
#[tokio::test]
async fn broken_thread_root_lookup_is_not_reported_as_a_missing_thread() {
    let (base, db, token) = repo_with_pr("threadfail").await;
    let client = reqwest::Client::new();
    let url = format!(
        "{base}/api/v1/repos/threadfail-owner/threadfail-repo/pulls/1/comments/1/resolution"
    );

    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"resolved": true}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent thread is still a 404");

    db.execute_unprepared("DROP TABLE review_comments")
        .await
        .expect("drop review_comments");

    let resp = client
        .patch(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"resolved": true}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_broken_lookup(status, &body, "review thread");
}
