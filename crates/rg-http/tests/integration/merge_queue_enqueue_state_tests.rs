//! card_1dc8db5f5eb3: `PUT /repos/{owner}/{name}/pulls/{n}/merge-queue` refusing
//! a closed or draft pull request must answer `409`, not `500`.
//!
//! Both refusals sat at the top of `rg_core::pull_request::merge_queue::enqueue`
//! as bare `bail!`s, so they reached the client as `internal server error` —
//! while `POST .../merge`, the other entrance to the same merge, already
//! answered `409 draft pull requests cannot be merged` to the very same PR. One
//! pull request, two neighbouring endpoints, two classes of answer.
//!
//! These go through the HTTP funnel on purpose: the core tests next to the fix
//! pin the error *type*, and these pin what that type is worth to a client.

use rg_db::sea_orm::{NotSet, Set};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn seed_pr(
    base: &str,
    db: &rg_db::DatabaseConnection,
    owner: &str,
    repo_name: &str,
    state: &str,
    is_draft: bool,
) -> (String, rg_db::entities::pull_request::Model) {
    let (token, user_id) = register_full(base, owner, &format!("{owner}@example.com")).await;
    let repo_id = create_repo(base, &token, repo_name).await;
    let now = chrono::Utc::now();
    let pr = rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("state refusal".to_string()),
            body: Set(None),
            state: Set(state.to_string()),
            is_draft: Set(is_draft),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(user_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some("1".repeat(40))),
            merge_strategy: Set(None),
            merge_commit_sha: Set(None),
            head_repo_id: Set(None),
            ci_approved_sha: Set(None),
            ci_approved_by: Set(None),
            ci_approved_at: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
            closed_at: Set(None),
            merged_at: Set(None),
        },
    )
    .await
    .expect("seed the pull request");
    (token, pr)
}

async fn enqueue(base: &str, token: &str, owner: &str, repo_name: &str) -> (u16, String) {
    let response = reqwest::Client::new()
        .put(format!(
            "{base}/api/v1/repos/{owner}/{repo_name}/pulls/1/merge-queue"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .expect("call the merge-queue endpoint");
    let status = response.status().as_u16();
    let body = response.text().await.expect("read the error body");
    (status, body)
}

async fn assert_nothing_was_queued(db: &rg_db::DatabaseConnection, pr_id: i64) {
    assert!(
        rg_db::ops::merge_queue_ops::find_by_pr(db, pr_id)
            .await
            .expect("look for a queue entry after the refusal")
            .is_none(),
        "a refused enqueue still created a merge-queue entry"
    );
}

#[tokio::test]
async fn enqueueing_a_closed_pull_request_is_a_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "queue-closed";
    let repo_name = "queue-closed-repo";
    let (token, pr) = seed_pr(&base, &db, owner, repo_name, "closed", false).await;

    let (status, body) = enqueue(&base, &token, owner, repo_name).await;
    assert_eq!(
        status, 409,
        "a closed PR is state, not a server failure, got {status}: {body}"
    );
    assert!(
        body.contains("open pull request"),
        "the body must name the state that refused, got: {body}"
    );
    assert!(
        !body.to_lowercase().contains("internal server error"),
        "{body}"
    );
    assert_nothing_was_queued(&db, pr.id).await;
}

#[tokio::test]
async fn enqueueing_a_draft_pull_request_is_a_conflict() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "queue-draft";
    let repo_name = "queue-draft-repo";
    let (token, pr) = seed_pr(&base, &db, owner, repo_name, "open", true).await;

    let (status, body) = enqueue(&base, &token, owner, repo_name).await;
    assert_eq!(
        status, 409,
        "a draft PR is state, not a server failure, got {status}: {body}"
    );
    assert!(
        body.contains("draft"),
        "the body must name the state that refused, got: {body}"
    );
    assert!(
        !body.to_lowercase().contains("internal server error"),
        "{body}"
    );
    assert_nothing_was_queued(&db, pr.id).await;
}
