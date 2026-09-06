//! card_9aa1ded8824e: switching a PR between auto-merge and the merge queue is
//! one ownership handoff. If its second primary write fails, the first must be
//! rolled back so the PR is never left in neither mechanism.

use rg_db::sea_orm::{ConnectionTrait, NotSet, Set};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

struct Fixture {
    base: String,
    db: rg_db::DatabaseConnection,
    token: String,
    user_id: i64,
    owner: &'static str,
    repo_name: &'static str,
    pr: rg_db::entities::pull_request::Model,
}

async fn fixture(owner: &'static str, repo_name: &'static str) -> Fixture {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, owner, &format!("{owner}@example.com")).await;
    let repo_id = create_repo(&base, &token, repo_name).await;

    // If a handoff unexpectedly succeeds, both automatic mechanisms must stop
    // at `pending` instead of reaching Git and accidentally producing the same
    // 5xx the injected database failure is meant to prove.
    let protection = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo_name}/branches/protection"
        ))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_approval": true,
            "required_approvals": 1,
        }))
        .send()
        .await
        .expect("create branch protection");
    assert_eq!(
        protection.status(),
        201,
        "{}",
        protection.text().await.unwrap()
    );

    let now = chrono::Utc::now();
    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("atomic ownership handoff".to_string()),
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
    .expect("seed pull request");

    Fixture {
        base,
        db,
        token,
        user_id,
        owner,
        repo_name,
        pr,
    }
}

async fn reload_pr(fixture: &Fixture) -> rg_db::entities::pull_request::Model {
    rg_db::ops::pull_request_ops::find_by_id(&fixture.db, fixture.pr.id)
        .await
        .expect("reload pull request")
        .expect("pull request still exists")
}

#[tokio::test]
async fn failed_queue_insert_restores_auto_merge_ownership() {
    let fixture = fixture("queuehandoff", "queue-handoff-repo").await;
    let enabled_at = chrono::Utc::now();
    let mut active: rg_db::entities::pull_request::ActiveModel = fixture.pr.clone().into();
    active.auto_merge_enabled = Set(true);
    active.auto_merge_strategy = Set(Some("merge".to_string()));
    active.auto_merge_enabled_by_id = Set(Some(fixture.user_id));
    active.auto_merge_enabled_at = Set(Some(enabled_at));
    active.updated_at = Set(enabled_at);
    rg_db::ops::pull_request_ops::update(&fixture.db, active)
        .await
        .expect("enable auto-merge before the queue handoff");

    // The trigger fires only after the transaction has performed its first
    // mutation. Removing or reordering the auto-merge relinquish makes this
    // injection stay silent and the endpoint return a normal pending response.
    fixture
        .db
        .execute_unprepared(
            "CREATE TRIGGER fail_second_queue_handoff_write \
             BEFORE INSERT ON merge_queue_entries \
             WHEN EXISTS ( \
                 SELECT 1 FROM pull_requests \
                 WHERE id = new.pr_id AND auto_merge_enabled = 0 \
             ) \
             BEGIN SELECT RAISE(ABORT, 'injected queue handoff failure'); END;",
        )
        .await
        .expect("install queue-insert fault");

    let response = reqwest::Client::new()
        .put(format!(
            "{}/api/v1/repos/{}/{}/pulls/1/merge-queue",
            fixture.base, fixture.owner, fixture.repo_name
        ))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .expect("request queue handoff");
    assert_eq!(response.status(), 500, "{}", response.text().await.unwrap());

    let pr = reload_pr(&fixture).await;
    assert!(pr.auto_merge_enabled, "the first write escaped rollback");
    assert_eq!(pr.auto_merge_strategy.as_deref(), Some("merge"));
    assert_eq!(pr.auto_merge_enabled_by_id, Some(fixture.user_id));
    assert_eq!(pr.auto_merge_enabled_at, Some(enabled_at));
    assert!(
        rg_db::ops::merge_queue_ops::find_by_pr(&fixture.db, pr.id)
            .await
            .expect("check queue after rollback")
            .is_none(),
        "a failed queue handoff left both ownership mechanisms active"
    );
}

#[tokio::test]
async fn failed_auto_merge_update_restores_queue_ownership() {
    let fixture = fixture("autohandoff", "auto-handoff-repo").await;
    let entry = rg_db::ops::merge_queue_ops::enqueue(
        &fixture.db,
        fixture.pr.repo_id,
        fixture.pr.id,
        fixture.user_id,
        "merge",
    )
    .await
    .expect("seed queue entry")
    .expect("queue entry exists");

    // This fault exists only after the queued entry has become canceled inside
    // the same transaction. If cancellation is skipped or moved after the PR
    // update, the trigger cannot fire and the endpoint returns 200 pending.
    fixture
        .db
        .execute_unprepared(
            "CREATE TRIGGER fail_second_auto_merge_handoff_write \
             BEFORE UPDATE ON pull_requests \
             WHEN new.auto_merge_enabled = 1 AND EXISTS ( \
                 SELECT 1 FROM merge_queue_entries \
                 WHERE pr_id = old.id AND status = 'canceled' \
             ) \
             BEGIN SELECT RAISE(ABORT, 'injected auto-merge handoff failure'); END;",
        )
        .await
        .expect("install auto-merge update fault");

    let response = reqwest::Client::new()
        .put(format!(
            "{}/api/v1/repos/{}/{}/pulls/1/auto-merge",
            fixture.base, fixture.owner, fixture.repo_name
        ))
        .bearer_auth(&fixture.token)
        .json(&serde_json::json!({"strategy": "merge"}))
        .send()
        .await
        .expect("request auto-merge handoff");
    assert_eq!(response.status(), 500, "{}", response.text().await.unwrap());

    let pr = reload_pr(&fixture).await;
    assert!(!pr.auto_merge_enabled, "the failed second write committed");
    assert_eq!(pr.auto_merge_strategy, None);
    assert_eq!(pr.auto_merge_enabled_by_id, None);
    assert_eq!(pr.auto_merge_enabled_at, None);
    let current = rg_db::ops::merge_queue_ops::find_by_pr(&fixture.db, pr.id)
        .await
        .expect("reload queue after rollback")
        .expect("queue ownership survived the failed handoff");
    assert_eq!(current.id, entry.id);
    assert_eq!(current.attempt_number, entry.attempt_number);
    assert_eq!(current.status, "queued");
    assert_eq!(current.finished_at, None);
}
