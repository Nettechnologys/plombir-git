//! Fault-injection coverage for card_400cd1298392.
//!
//! Reviews, comments, thread state and manual reviewer requests are database
//! mutations whose matching PR event is part of the same user-visible change.
//! If only the event insert fails, the endpoint may return 5xx only when the
//! primary row was rolled back too; otherwise the response invites a retry of
//! an operation that already happened.

use crate::common::{register_full, spawn_test_app_with_db};
use chrono::Utc;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, Set};

struct Fixture {
    base: String,
    db: sea_orm::DatabaseConnection,
    owner: String,
    owner_token: String,
    reviewer: String,
    reviewer_token: String,
    reviewer_id: i64,
    repo: String,
    repo_id: i64,
    pr: rg_db::entities::pull_request::Model,
}

async fn fixture(prefix: &str) -> Fixture {
    let (base, db) = spawn_test_app_with_db().await;
    let owner = format!("{prefix}-owner");
    let reviewer = format!("{prefix}-reviewer");
    let repo = format!("{prefix}-repo");
    let (owner_token, owner_id) =
        register_full(&base, &owner, &format!("{owner}@example.com")).await;
    let (reviewer_token, reviewer_id) =
        register_full(&base, &reviewer, &format!("{reviewer}@example.com")).await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&owner_token)
        .json(&serde_json::json!({"name": repo, "is_private": false}))
        .send()
        .await
        .expect("create repository");
    assert_eq!(created.status(), 201, "repository fixture must be created");
    let repo_id = created
        .json::<serde_json::Value>()
        .await
        .expect("repository json")["id"]
        .as_i64()
        .expect("repository id");
    let now = Utc::now();
    let pr = rg_db::ops::pull_request_ops::create(
        &db,
        rg_db::entities::pull_request::ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("Atomic review writes".into()),
            body: Set(None),
            state: Set("open".into()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(owner_id),
            reviewer_id: Set(None),
            head_branch: Set("feature".into()),
            base_branch: Set("main".into()),
            head_sha: Set(Some("0".repeat(40))),
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
    .expect("insert pull request");

    Fixture {
        base,
        db,
        owner,
        owner_token,
        reviewer,
        reviewer_token,
        reviewer_id,
        repo,
        repo_id,
        pr,
    }
}

async fn reject_event(db: &sea_orm::DatabaseConnection, event_type: &str) {
    db.execute_unprepared(&format!(
        "CREATE TRIGGER review_timeline_outage BEFORE INSERT ON pr_events \
         WHEN new.event_type = '{event_type}' \
         BEGIN SELECT RAISE(ABORT, 'timeline storage is unavailable'); END;"
    ))
    .await
    .expect("install timeline write fault");
}

async fn seed_review(fixture: &Fixture) -> rg_db::entities::pr_review::Model {
    rg_db::ops::pr_review_ops::create(
        &fixture.db,
        rg_db::entities::pr_review::ActiveModel {
            id: sea_orm::NotSet,
            pr_id: Set(fixture.pr.id),
            repo_id: Set(fixture.repo_id),
            reviewer_id: Set(fixture.reviewer_id),
            action: Set("comment".into()),
            body: Set(None),
            commit_id: Set(None),
            created_at: Set(Utc::now()),
            dismissed_at: Set(None),
            dismissed_by: Set(None),
        },
    )
    .await
    .expect("seed review")
}

async fn seed_comment(fixture: &Fixture, review_id: i64) -> rg_db::entities::review_comment::Model {
    rg_db::ops::review_comment_ops::create(
        &fixture.db,
        rg_db::entities::review_comment::ActiveModel {
            id: sea_orm::NotSet,
            review_id: Set(review_id),
            pr_id: Set(fixture.pr.id),
            author_id: Set(fixture.reviewer_id),
            path: Set("src/lib.rs".into()),
            position: Set(None),
            line: Set(Some(7)),
            start_line: Set(None),
            side: Set(Some("RIGHT".into())),
            start_side: Set(None),
            body: Set("Please adjust this line".into()),
            suggestion: Set(None),
            suggestion_applied_at: Set(None),
            suggestion_applied_by_id: Set(None),
            suggestion_commit_sha: Set(None),
            commit_id: Set(None),
            reply_to_id: Set(None),
            resolved_at: Set(None),
            resolved_by_id: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
        },
    )
    .await
    .expect("seed review comment")
}

fn pr_url(fixture: &Fixture) -> String {
    format!(
        "{}/api/v1/repos/{}/{}/pulls/1",
        fixture.base, fixture.owner, fixture.repo
    )
}

async fn assert_server_failure(response: reqwest::Response, operation: &str) {
    let status = response.status();
    let body = response.text().await.expect("read failure body");
    assert!(
        status.is_server_error(),
        "a rejected timeline insert must fail {operation}, got {status}: {body}"
    );
}

#[tokio::test]
async fn a_review_and_its_timeline_event_commit_or_roll_back_together() {
    let fixture = fixture("review-atomic").await;
    reject_event(&fixture.db, "review_approve").await;

    let response = reqwest::Client::new()
        .post(format!("{}/reviews", pr_url(&fixture)))
        .bearer_auth(&fixture.reviewer_token)
        .json(&serde_json::json!({"action": "approve", "body": "looks good"}))
        .send()
        .await
        .expect("submit review");
    assert_server_failure(response, "review submission").await;

    let count = rg_db::entities::pr_review::Entity::find()
        .filter(rg_db::entities::pr_review::Column::PrId.eq(fixture.pr.id))
        .count(&fixture.db)
        .await
        .expect("count reviews after rollback");
    assert_eq!(count, 0, "a 5xx must not leave the review committed");
}

#[tokio::test]
async fn a_comment_and_its_timeline_event_commit_or_roll_back_together() {
    let fixture = fixture("comment-atomic").await;
    let review = seed_review(&fixture).await;
    reject_event(&fixture.db, "review_comment").await;

    let response = reqwest::Client::new()
        .post(format!("{}/comments", pr_url(&fixture)))
        .bearer_auth(&fixture.reviewer_token)
        .json(&serde_json::json!({
            "review_id": review.id,
            "path": "src/lib.rs",
            "line": 7,
            "side": "RIGHT",
            "body": "Please adjust this line"
        }))
        .send()
        .await
        .expect("create review comment");
    assert_server_failure(response, "review comment creation").await;

    let count = rg_db::entities::review_comment::Entity::find()
        .filter(rg_db::entities::review_comment::Column::PrId.eq(fixture.pr.id))
        .count(&fixture.db)
        .await
        .expect("count comments after rollback");
    assert_eq!(count, 0, "a 5xx must not leave the comment committed");
}

#[tokio::test]
async fn an_implicit_review_rolls_back_with_its_failed_comment() {
    let fixture = fixture("implicit-atomic").await;
    reject_event(&fixture.db, "review_comment").await;

    let response = reqwest::Client::new()
        .post(format!("{}/comments", pr_url(&fixture)))
        .bearer_auth(&fixture.reviewer_token)
        .json(&serde_json::json!({
            "path": "src/lib.rs",
            "line": 7,
            "side": "RIGHT",
            "body": "Implicit review comment"
        }))
        .send()
        .await
        .expect("create review comment with an implicit review");
    assert_server_failure(response, "implicit review comment creation").await;

    let reviews = rg_db::entities::pr_review::Entity::find()
        .filter(rg_db::entities::pr_review::Column::PrId.eq(fixture.pr.id))
        .count(&fixture.db)
        .await
        .expect("count implicit reviews after rollback");
    let comments = rg_db::entities::review_comment::Entity::find()
        .filter(rg_db::entities::review_comment::Column::PrId.eq(fixture.pr.id))
        .count(&fixture.db)
        .await
        .expect("count implicit comments after rollback");
    let events = rg_db::ops::pr_event_ops::list_by_pr(&fixture.db, fixture.pr.id)
        .await
        .expect("list implicit-review events after rollback");
    assert_eq!((reviews, comments, events.len()), (0, 0, 0));
}

#[tokio::test]
async fn thread_state_and_its_timeline_event_commit_or_roll_back_together() {
    let fixture = fixture("thread-atomic").await;
    let review = seed_review(&fixture).await;
    let comment = seed_comment(&fixture, review.id).await;
    reject_event(&fixture.db, "thread_resolved").await;

    let response = reqwest::Client::new()
        .patch(format!(
            "{}/comments/{}/resolution",
            pr_url(&fixture),
            comment.id
        ))
        .bearer_auth(&fixture.reviewer_token)
        .json(&serde_json::json!({"resolved": true}))
        .send()
        .await
        .expect("resolve review thread");
    assert_server_failure(response, "thread resolution").await;

    let stored = rg_db::ops::review_comment_ops::find_by_id(&fixture.db, comment.id)
        .await
        .expect("reload comment")
        .expect("comment still exists");
    assert!(
        stored.resolved_at.is_none() && stored.resolved_by_id.is_none(),
        "a 5xx must not leave the thread resolved"
    );
}

#[tokio::test]
async fn reviewer_request_and_event_commit_or_roll_back_together() {
    let fixture = fixture("request-atomic").await;
    reject_event(&fixture.db, "reviewer_requested").await;

    let response = reqwest::Client::new()
        .post(format!("{}/reviewers", pr_url(&fixture)))
        .bearer_auth(&fixture.owner_token)
        .json(&serde_json::json!({"username": fixture.reviewer}))
        .send()
        .await
        .expect("request reviewer");
    assert_server_failure(response, "reviewer request").await;

    assert!(
        rg_db::ops::pr_reviewer_request_ops::find(&fixture.db, fixture.pr.id, fixture.reviewer_id)
            .await
            .expect("look up rolled-back reviewer request")
            .is_none(),
        "a 5xx must not leave the reviewer requested"
    );
}

#[tokio::test]
async fn reviewer_removal_and_event_commit_or_roll_back_together() {
    let fixture = fixture("remove-atomic").await;
    rg_db::ops::pr_reviewer_request_ops::create(
        &fixture.db,
        rg_db::entities::pr_reviewer_request::ActiveModel {
            id: sea_orm::NotSet,
            pr_id: Set(fixture.pr.id),
            reviewer_id: Set(fixture.reviewer_id),
            requested_by_id: Set(fixture.pr.author_id),
            created_at: Set(Utc::now()),
        },
    )
    .await
    .expect("seed reviewer request");
    reject_event(&fixture.db, "reviewer_removed").await;

    let response = reqwest::Client::new()
        .delete(format!(
            "{}/reviewers/{}",
            pr_url(&fixture),
            fixture.reviewer
        ))
        .bearer_auth(&fixture.owner_token)
        .send()
        .await
        .expect("remove reviewer request");
    assert_server_failure(response, "reviewer removal").await;

    assert!(
        rg_db::ops::pr_reviewer_request_ops::find(&fixture.db, fixture.pr.id, fixture.reviewer_id)
            .await
            .expect("reload reviewer request after rollback")
            .is_some(),
        "a 5xx must leave the reviewer request available for a safe retry"
    );
}
