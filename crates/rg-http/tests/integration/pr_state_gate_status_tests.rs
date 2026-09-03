//! Regression coverage for card_78afc7626c8f: two pull-request routes answered
//! `400` to "this PR is not open" while every other route answered `409`.
//!
//! `POST .../pulls/{n}/ci-approval` and the `draft` branch of
//! `PATCH .../pulls/{n}` both refused a non-open PR through
//! `rg_core::error::invalid_request`. The predicate is identical to the one
//! `merge_pr`, `enable_auto_merge`, the merge queue's `enqueue` and
//! `submit_review` already reject with `Conflict`, and the difference is not
//! cosmetic: `400` tells a client the request is unfixable and must not be
//! repeated, `409` tells it the state will change and the identical request
//! will then succeed. Reopening the PR is exactly that state change.
//!
//! The third test holds the other side of the line. The neighbouring refusal in
//! the same handler — a PR with no head commit to approve — describes the
//! *resource*, not a state anyone can wait out, and stays a `400`. A fix that
//! swept the whole function into `Conflict` would pass the first two tests and
//! fail this one.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use chrono::Utc;
use sea_orm::Set;

async fn insert_pr(
    db: &sea_orm::DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    state: &str,
    head_sha: Option<String>,
) {
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("PR 1".to_string()),
            body: Set(None),
            state: Set(state.to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(head_sha),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
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

/// `(base_url, pr_url, token)` for a repository owned by `{prefix}-owner`
/// holding a single PR in `state`.
async fn setup(prefix: &str, state: &str, head_sha: Option<String>) -> (String, String) {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(
        &base,
        &format!("{prefix}-owner"),
        &format!("{prefix}@example.com"),
    )
    .await;
    let repo_id = create_repo(&base, &token, &format!("{prefix}-repo")).await;
    insert_pr(&db, repo_id, user_id, state, head_sha).await;
    let pr_url = format!("{base}/api/v1/repos/{prefix}-owner/{prefix}-repo/pulls/1");
    (pr_url, token)
}

async fn body_message(response: reqwest::Response) -> String {
    let body: serde_json::Value = response.json().await.expect("json body");
    body["error"]["message"]
        .as_str()
        .unwrap_or_default()
        .to_string()
}

/// Approving CI on a closed PR is a state problem: reopen it and the identical
/// request succeeds.
#[tokio::test]
async fn approving_ci_on_a_closed_pr_is_a_conflict_not_a_bad_request() {
    let (pr_url, token) = setup("ciapprclosed", "closed", Some("0".repeat(40))).await;

    let resp = reqwest::Client::new()
        .post(format!("{pr_url}/ci-approval"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let message = body_message(resp).await;

    assert_eq!(
        status, 409,
        "CI approval on a closed PR must be a 409 (body: {message})"
    );
    assert!(
        message.contains("only an open pull request can have its CI approved"),
        "the 409 must name the state that refused, got: {message}"
    );
    assert!(
        message.contains("closed"),
        "the 409 must say which state the PR is in, got: {message}"
    );
}

/// Same predicate, same answer, on the other route that had it wrong.
#[tokio::test]
async fn changing_draft_status_on_a_closed_pr_is_a_conflict_not_a_bad_request() {
    let (pr_url, token) = setup("draftclosed", "closed", Some("0".repeat(40))).await;

    let resp = reqwest::Client::new()
        .patch(&pr_url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"draft": true}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let message = body_message(resp).await;

    assert_eq!(
        status, 409,
        "a draft change on a closed PR must be a 409 (body: {message})"
    );
    assert!(
        message.contains("only an open pull request can change draft status"),
        "the 409 must name the state that refused, got: {message}"
    );
    assert!(
        message.contains("closed"),
        "the 409 must say which state the PR is in, got: {message}"
    );
}

/// The refusal next door in the same handler is genuinely the caller's problem
/// — an open PR that has no commit to approve — and stays a 400.
#[tokio::test]
async fn a_pr_with_no_head_commit_is_still_a_bad_request() {
    let (pr_url, token) = setup("ciapprnohead", "open", None).await;

    let resp = reqwest::Client::new()
        .post(format!("{pr_url}/ci-approval"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let message = body_message(resp).await;

    assert_eq!(
        status, 400,
        "a PR with no head commit describes the resource, not a state to wait out (body: {message})"
    );
    assert!(
        message.contains("no head commit to approve"),
        "the 400 must name the reason, got: {message}"
    );
}
