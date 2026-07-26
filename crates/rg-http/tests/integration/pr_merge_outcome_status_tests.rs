//! Regression coverage for card_820920d8b843: `POST .../pulls/{n}/merge` used to
//! answer `400` to every way a merge can fail.
//!
//! A closed PR, a draft, a racing merge attempt and a merge conflict are all
//! *states* — the request was well-formed and may succeed later, so `409` is the
//! honest answer. A database failure inside the merge is ours and belongs in the
//! 5xx range; reported as `400` it told the client to fix a request that was
//! never wrong, and carried the `db: ...` context (or a git command line) into
//! the response body along the way (H-05).
//!
//! The same handler had a second collapse one arm up: any error from
//! `check_merge_allowed` became `403`, so a broken `protected_branches` table
//! read as "you are not allowed to merge".
//!
//! The conflict case itself lives in `pr_merge_strategy_tests`, which needs a
//! seeded git repository; these tests cover the outcomes that are decided before
//! any git work happens.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use chrono::Utc;
use sea_orm::{ConnectionTrait, Set};

async fn insert_pr(
    db: &sea_orm::DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    number: i64,
    state: &str,
) {
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(number),
            title: Set(format!("PR {number}")),
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

async fn setup(prefix: &str, state: &str) -> (String, sea_orm::DatabaseConnection, String) {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(
        &base,
        &format!("{prefix}-owner"),
        &format!("{prefix}@example.com"),
    )
    .await;
    let repo_id = create_repo(&base, &token, &format!("{prefix}-repo")).await;
    insert_pr(&db, repo_id, user_id, 1, state).await;
    (base, db, token)
}

async fn post_merge(base: &str, prefix: &str, token: &str, strategy: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{prefix}-owner/{prefix}-repo/pulls/1/merge"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"strategy": strategy}))
        .send()
        .await
        .expect("request")
}

/// A PR that is not open cannot be merged *right now* — that is a 409, not a
/// malformed request.
#[tokio::test]
async fn merging_a_closed_pr_is_a_conflict_not_a_bad_request() {
    let (base, _db, token) = setup("mergeclosed", "closed").await;

    let resp = post_merge(&base, "mergeclosed", &token, "merge").await;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        status, 409,
        "merging a closed PR must be a 409 (body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("not in 'open' state"),
        "the 409 must say what the state problem is, got: {message}"
    );
}

/// An unparseable strategy really is the caller's mistake — the one outcome on
/// this route that stays a 400.
#[tokio::test]
async fn an_unknown_merge_strategy_is_still_a_bad_request() {
    let (base, _db, token) = setup("mergestrategy", "open").await;

    let resp = post_merge(&base, "mergestrategy", &token, "teleport").await;
    assert_eq!(
        resp.status(),
        400,
        "an unknown strategy is the client's mistake and stays a 400"
    );
}

/// A failed branch-protection *check* is not a refusal to merge: with
/// `protected_branches` gone the handler used to answer `403` and put the
/// database error in the body.
#[tokio::test]
async fn a_broken_protection_check_is_not_reported_as_a_refusal() {
    let (base, db, token) = setup("mergeprot", "open").await;

    db.execute_unprepared("DROP TABLE protected_branches")
        .await
        .expect("drop protected_branches");

    let resp = post_merge(&base, "mergeprot", &token, "merge").await;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a failed protection check must be a 5xx, not {status} — a 403 blames \
         the caller for an outage and puts nothing in the alerts (body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:") && !message.contains("protected_branches"),
        "the response body must not carry internal error detail, got: {message}"
    );
}

/// A merge that gets past the state checks and then fails on storage is a 5xx —
/// here the repository directory the merge needs does not exist on disk, which
/// is a server-side inconsistency and used to be reported as a 400 with the
/// absolute path in the body.
#[tokio::test]
async fn a_merge_that_fails_on_storage_is_not_reported_as_a_bad_request() {
    let (base, _db, token) = setup("mergestore", "open").await;

    let resp = post_merge(&base, "mergestore", &token, "merge").await;
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a merge that fails on storage must be a 5xx, not {status} (body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("/") && !message.contains(".git"),
        "the response body must not carry the storage path, got: {message}"
    );
}
