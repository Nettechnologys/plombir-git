//! card_7c413bfeea0f: `DELETE .../merge-queue` answered `404 pull request is
//! not queued` to two different refusals.
//!
//! `merge_queue_ops::cancel` only moves an entry that is still `queued`, which
//! is right — a worker that already claimed the entry is mid-merge and nothing
//! can call that back. But the handler read its `bool` as "there is nothing
//! here", so a PR whose entry was `running` was told it is *not in the queue*.
//! That is the opposite of the truth, and it is the one answer that stops the
//! caller looking: a 404 reads as "wrong PR / already gone", while the real
//! answer is "you are too late, wait for the merge to finish".
//!
//! The three outcomes and their codes:
//!
//! * `queued` → `204`, the entry is canceled.
//! * no entry, or a finished one → `404`, there is nothing to cancel.
//! * `running` → `409`, with a message that names the state.

use rg_db::sea_orm::{NotSet, Set};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

/// A repository, an open PR on it, and the ids the test needs. The PR row is
/// seeded directly: this test is about the queue's state machine, and a PR
/// built through git would only add branches nothing here reads.
async fn seed_pr(
    base: &str,
    db: &rg_db::DatabaseConnection,
    owner: &str,
) -> (String, i64, i64, i64) {
    let (token, user_id) = register_full(base, owner, &format!("{owner}@example.com")).await;
    let repo_id = create_repo(base, &token, "queued").await;
    let now = chrono::Utc::now();
    let pr = rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("cancel me".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
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
    .expect("seed open PR");
    (token, user_id, repo_id, pr.id)
}

fn queue_url(base: &str, owner: &str) -> String {
    format!("{base}/api/v1/repos/{owner}/queued/pulls/1/merge-queue")
}

/// The refusal that used to be indistinguishable from "no such entry".
#[tokio::test]
async fn cancelling_an_entry_a_worker_already_took_is_a_conflict_not_a_missing_entry() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "cancel-running";
    let (token, user_id, repo_id, pr_id) = seed_pr(&base, &db, owner).await;

    let entry = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, user_id, "merge")
        .await
        .expect("enqueue the PR")
        .expect("the fixture repository and pull request remain live");
    // Exactly what a queue worker does when it picks the entry up.
    assert!(
        rg_db::ops::merge_queue_ops::claim(&db, entry.id, entry.attempt_number)
            .await
            .expect("claim the entry"),
        "non-vacuity: the entry has to actually be running for this test to mean anything",
    );

    let response = reqwest::Client::new()
        .delete(queue_url(&base, owner))
        .bearer_auth(&token)
        .send()
        .await
        .expect("cancel a running merge-queue entry");
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("json body");
    assert_eq!(
        status, 409,
        "an entry being merged is a state conflict, not a missing one (body: {body})"
    );
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("already merging"),
        "the 409 must say what the state problem is, got: {message}"
    );

    // The refusal changed nothing: the worker still owns its entry.
    let entry = rg_db::ops::merge_queue_ops::find_by_pr(&db, pr_id)
        .await
        .expect("reload the entry")
        .expect("the entry is still there");
    assert_eq!(entry.status, "running");
}

/// A terminal merge-queue row is recycled in place. Every delayed transition
/// must therefore carry the attempt number it read, or an old worker can claim
/// or finish the newly queued run under the same primary key.
#[tokio::test]
async fn stale_attempt_transitions_cannot_mutate_a_reenqueued_entry() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_token, user_id, repo_id, pr_id) = seed_pr(&base, &db, "stale-attempt").await;

    let first = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, user_id, "merge")
        .await
        .expect("enqueue first attempt")
        .expect("the fixture repository and pull request remain live");
    let canceled = rg_db::ops::merge_queue_ops::cancel(&db, pr_id)
        .await
        .expect("cancel first attempt")
        .expect("the queued attempt was canceled");
    assert_eq!(canceled.attempt_number, first.attempt_number);
    assert!(!rg_db::ops::merge_queue_ops::set_merge_group(
        &db,
        first.id,
        first.attempt_number,
        &"d".repeat(40),
        &"e".repeat(40),
        &"f".repeat(40),
        98,
    )
    .await
    .expect("terminal ownership write is a normal refusal"));

    let current = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, user_id, "merge")
        .await
        .expect("re-enqueue the row")
        .expect("the fixture repository and pull request remain live");
    assert_eq!(current.attempt_number, first.attempt_number + 1);

    assert!(!rg_db::ops::merge_queue_ops::set_merge_group(
        &db,
        first.id,
        first.attempt_number,
        &"a".repeat(40),
        &"b".repeat(40),
        &"c".repeat(40),
        99,
    )
    .await
    .expect("stale ownership write is a normal refusal"));
    assert!(
        !rg_db::ops::merge_queue_ops::claim(&db, first.id, first.attempt_number)
            .await
            .expect("stale claim is a normal refusal")
    );
    assert!(!rg_db::ops::merge_queue_ops::finish(
        &db,
        first.id,
        first.attempt_number,
        "failed",
        Some("stale worker".into()),
    )
    .await
    .expect("stale finish is a normal refusal"));

    let preserved = rg_db::ops::merge_queue_ops::find_by_pr(&db, pr_id)
        .await
        .expect("read current attempt")
        .expect("current attempt exists");
    assert_eq!(preserved.attempt_number, current.attempt_number);
    assert_eq!(preserved.status, "queued");
    assert!(preserved.failure_reason.is_none());
    assert!(preserved.merge_group_pipeline_id.is_none());
}

/// The other two outcomes, so the 409 above is a *distinction* and not a new
/// blanket answer: a queued entry still cancels, and a PR with nothing in the
/// queue is still a 404.
#[tokio::test]
async fn a_queued_entry_cancels_and_an_absent_one_is_still_a_404() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "cancel-queued";
    let (token, user_id, repo_id, pr_id) = seed_pr(&base, &db, owner).await;
    let client = reqwest::Client::new();

    // Nothing enqueued yet: there genuinely is no entry.
    let absent = client
        .delete(queue_url(&base, owner))
        .bearer_auth(&token)
        .send()
        .await
        .expect("cancel with an empty queue");
    assert_eq!(
        absent.status(),
        404,
        "a PR that was never queued has nothing to cancel"
    );

    rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr_id, user_id, "merge")
        .await
        .expect("enqueue the PR")
        .expect("the fixture repository and pull request remain live");
    let canceled = client
        .delete(queue_url(&base, owner))
        .bearer_auth(&token)
        .send()
        .await
        .expect("cancel a queued entry");
    assert_eq!(canceled.status(), 204);
    assert_eq!(
        rg_db::ops::merge_queue_ops::find_by_pr(&db, pr_id)
            .await
            .expect("reload the entry")
            .expect("the entry row survives its cancellation")
            .status,
        "canceled",
    );

    // A finished entry is not in the queue either — the 404 covers it, and the
    // 409 must not spread to every non-`queued` status.
    let again = client
        .delete(queue_url(&base, owner))
        .bearer_auth(&token)
        .send()
        .await
        .expect("cancel an already-canceled entry");
    assert_eq!(
        again.status(),
        404,
        "an entry that already finished is not a merge in progress"
    );
}
