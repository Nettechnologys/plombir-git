//! card_b66a1a2dcc18: a repeat merge-queue enqueue must not confirm a stale
//! entry after pull-request or repository deletion wins behind the handler's
//! initial lookups.
//!
//! The trigger fires inside the DB primitive's guarded existing-row write, so
//! these are deterministic orderings rather than timing tests. Both paths must
//! return typed `404`, never the old blanket `400` and never a backend-shaped
//! `500`; neither may publish `merge_queue_enqueued` for the deleted entry.

use rg_db::sea_orm::{
    ColumnTrait, ConnectionTrait, EntityTrait, NotSet, PaginatorTrait, QueryFilter, Set, Statement,
};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

async fn seed_pr(
    base: &str,
    db: &rg_db::DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> (String, i64, i64, rg_db::entities::pull_request::Model) {
    let (token, user_id) = register_full(base, owner, &format!("{owner}@example.com")).await;
    let repo_id = create_repo(base, &token, repo_name).await;
    let now = chrono::Utc::now();
    let pr = rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("deletion race".to_string()),
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
    .expect("seed the open pull request");
    (token, user_id, repo_id, pr)
}

fn queue_url(base: &str, owner: &str, repo_name: &str) -> String {
    format!("{base}/api/v1/repos/{owner}/{repo_name}/pulls/1/merge-queue")
}

async fn assert_no_queue_or_enqueue_event(db: &rg_db::DatabaseConnection, pr_id: i64) {
    assert!(
        rg_db::ops::merge_queue_ops::find_by_pr(db, pr_id)
            .await
            .expect("look for a queue entry after the cascade")
            .is_none(),
        "the losing request left or recreated a merge-queue entry"
    );
    let events = rg_db::entities::pr_event::Entity::find()
        .filter(rg_db::entities::pr_event::Column::PrId.eq(pr_id))
        .filter(rg_db::entities::pr_event::Column::EventType.eq("merge_queue_enqueued"))
        .count(db)
        .await
        .expect("count merge-queue enqueue events");
    assert_eq!(
        events, 0,
        "the losing request published merge_queue_enqueued after deletion"
    );
}

#[tokio::test]
async fn pull_request_cascade_during_live_reenqueue_is_typed_not_found() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "queue-pr-race";
    let repo_name = "queue-pr";
    let (token, user_id, repo_id, pr) = seed_pr(&base, &db, owner, repo_name).await;
    let entry = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr.id, user_id, "merge")
        .await
        .expect("enqueue the first attempt")
        .expect("the fixture parents remain live");
    assert!(
        rg_db::ops::merge_queue_ops::claim(&db, entry.id, entry.attempt_number)
            .await
            .expect("claim the first attempt"),
        "the live branch must be exercised in running state"
    );

    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_pr_before_queue_validation \
             BEFORE UPDATE ON merge_queue_entries WHEN OLD.id = {} \
             BEGIN DELETE FROM pull_requests WHERE id = OLD.pr_id; END",
            entry.id
        ),
    ))
    .await
    .expect("install the competing pull-request delete");

    let response = reqwest::Client::new()
        .put(queue_url(&base, owner, repo_name))
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "rebase"}))
        .send()
        .await
        .expect("race a live re-enqueue against PR deletion");
    let status = response.status();
    let body = response.text().await.expect("read the typed error body");
    assert_eq!(
        status, 404,
        "PR deletion after the queue read must be typed absence, got {status}: {body}"
    );
    assert!(body.contains("pull request not found"), "{body}");
    assert!(!body.contains("RecordNotUpdated"), "{body}");
    assert!(rg_db::ops::repo_ops::find_by_id(&db, repo_id)
        .await
        .expect("read the surviving repository")
        .is_some());
    assert!(rg_db::ops::pull_request_ops::find_by_id(&db, pr.id)
        .await
        .expect("look for the deleted pull request")
        .is_none());
    assert_no_queue_or_enqueue_event(&db, pr.id).await;
}

#[tokio::test]
async fn repository_cascade_during_terminal_reenqueue_is_typed_not_found() {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = "queue-repo-race";
    let repo_name = "queue-repo";
    let (token, user_id, repo_id, pr) = seed_pr(&base, &db, owner, repo_name).await;
    let entry = rg_db::ops::merge_queue_ops::enqueue(&db, repo_id, pr.id, user_id, "merge")
        .await
        .expect("enqueue the first attempt")
        .expect("the fixture parents remain live");
    assert!(
        rg_db::ops::merge_queue_ops::finish(
            &db,
            entry.id,
            entry.attempt_number,
            "failed",
            Some("fixture failure".to_string()),
        )
        .await
        .expect("finish the first attempt"),
        "the terminal recycle branch must be exercised"
    );

    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_repo_before_queue_recycle \
             BEFORE UPDATE ON merge_queue_entries WHEN OLD.id = {} \
             BEGIN DELETE FROM repositories WHERE id = OLD.repo_id; END",
            entry.id
        ),
    ))
    .await
    .expect("install the competing repository delete");

    let response = reqwest::Client::new()
        .put(queue_url(&base, owner, repo_name))
        .bearer_auth(&token)
        .json(&serde_json::json!({"strategy": "squash"}))
        .send()
        .await
        .expect("race a terminal re-enqueue against repository deletion");
    let status = response.status();
    let body = response.text().await.expect("read the typed error body");
    assert_eq!(
        status, 404,
        "repository deletion after the queue read must be typed absence, got {status}: {body}"
    );
    assert!(body.contains("repository not found"), "{body}");
    assert!(!body.contains("RecordNotUpdated"), "{body}");
    assert!(rg_db::ops::repo_ops::find_by_id(&db, repo_id)
        .await
        .expect("look for the deleted repository")
        .is_none());
    assert!(
        rg_db::ops::pull_request_ops::find_by_id(&db, pr.id)
            .await
            .expect("read the pull request after repository deletion")
            .is_some(),
        "pull_requests.repo_id has no repository FK; the queue row disappears through its own repo_id cascade"
    );
    assert_no_queue_or_enqueue_event(&db, pr.id).await;
}
