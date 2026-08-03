//! card_500345bec1ab: a stored JSON blob that does not decode must not be
//! replaced by a default that the code then treats as a fact.
//!
//! Two sites, one shape, two different ways for the default to do damage:
//!
//!   * `pr_events.metadata` → `{}`. The timeline entry renders empty but real,
//!     and `has_resource_event` deduplicates a stored event against the
//!     synthesized one *by a field inside metadata* — an empty object matches
//!     nothing, so the same event is listed twice, once hollow and once
//!     synthesized. One unreadable blob becomes a visibly self-contradicting
//!     history, under `200`.
//!   * `webhook_deliveries.request_payload` → `Value::Null`. Redelivery posted
//!     a body of `null` to the receiver and answered `200 redelivery
//!     triggered`. Not "did not arrive" — *arrived wrong*, which is the one
//!     failure mode a webhook must not have.

use axum::http::StatusCode;
use chrono::Utc;
use sea_orm::{ConnectionTrait, Set};

use crate::common::{register_full, spawn_test_app_with_db};

async fn create_repo(base: &str, token: &str, name: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(token)
        .json(&serde_json::json!({"name": name, "is_private": true}))
        .send()
        .await
        .expect("create repo");
    assert_eq!(response.status(), StatusCode::CREATED);
    response.json::<serde_json::Value>().await.expect("body")["id"]
        .as_i64()
        .expect("repo carries an id")
}

async fn insert_pr(
    db: &sea_orm::DatabaseConnection,
    repo_id: i64,
    author_id: i64,
) -> rg_db::entities::pull_request::Model {
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("PR 1".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            author_id: Set(author_id),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            ..Default::default()
        },
    )
    .await
    .expect("create pull request")
}

/// A stored `review_approved` event that points at a real review through
/// `metadata.review_id` — the pairing the timeline's deduplication relies on.
#[tokio::test]
async fn an_undecodable_pr_event_metadata_does_not_double_the_timeline_entry() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "tl-owner", "tl-owner@example.test").await;
    let repo_id = create_repo(&base, &token, "timeline").await;
    let pr = insert_pr(&db, repo_id, user_id).await;

    let review = rg_db::ops::pr_review_ops::create(
        &db,
        rg_db::entities::pr_review::ActiveModel {
            pr_id: Set(pr.id),
            repo_id: Set(repo_id),
            reviewer_id: Set(user_id),
            action: Set("approved".to_string()),
            body: Set(None),
            commit_id: Set(None),
            created_at: Set(Utc::now()),
            ..Default::default()
        },
    )
    .await
    .expect("create review");
    let event = rg_db::ops::pr_event_ops::record(
        &db,
        repo_id,
        pr.id,
        Some(user_id),
        "review_approved",
        None,
        serde_json::json!({"review_id": review.id}),
    )
    .await
    .expect("record the stored twin of that review");

    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/repos/tl-owner/timeline/pulls/1/timeline");

    // Baseline: the stored event and the review it points at collapse into one
    // entry. Without this the test could not tell "the fix works" from "the
    // fixture never produced a duplicate in the first place".
    let timeline = client
        .get(&endpoint)
        .bearer_auth(&token)
        .send()
        .await
        .expect("timeline");
    assert_eq!(timeline.status(), StatusCode::OK);
    let approvals = timeline
        .json::<Vec<serde_json::Value>>()
        .await
        .expect("body")
        .into_iter()
        .filter(|entry| entry["kind"] == "review_approved")
        .count();
    assert_eq!(
        approvals, 1,
        "the stored event and the review it references are one thing, not two"
    );

    db.execute_unprepared(&format!(
        "UPDATE pr_events SET metadata = 'not json' WHERE id = {};",
        event.id
    ))
    .await
    .expect("corrupt the stored metadata");

    let timeline = client
        .get(&endpoint)
        .bearer_auth(&token)
        .send()
        .await
        .expect("timeline with a broken event");
    assert_eq!(
        timeline.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "an unreadable event must stop the timeline, not be served as a hollow duplicate of itself"
    );
    let body = timeline.text().await.unwrap_or_default();
    assert!(
        !body.contains("review_approved"),
        "the contradictory history must not go out at all, got: {body}"
    );
}

/// Redelivery of a delivery row whose recorded payload cannot be reproduced.
#[tokio::test]
async fn a_webhook_redelivery_without_a_readable_payload_is_refused_not_sent_as_null() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "hook-owner", "hook-owner@example.test").await;
    create_repo(&base, &token, "hooks").await;
    let client = reqwest::Client::new();

    let created = client
        .post(format!("{base}/api/v1/repos/hook-owner/hooks/hooks"))
        .bearer_auth(&token)
        .json(&serde_json::json!({
            // Never contacted: the request is refused before any delivery is
            // spawned, which is precisely what the assertions below check.
            "url": "https://hooks.example.com/never-contacted",
            "events": ["push"],
        }))
        .send()
        .await
        .expect("create webhook");
    assert_eq!(created.status(), StatusCode::CREATED);
    let hook_id = created.json::<serde_json::Value>().await.expect("body")["id"]
        .as_i64()
        .expect("hook carries an id");

    let unreadable = rg_db::ops::webhook_ops::create_delivery(
        &db,
        rg_db::entities::webhook_delivery::ActiveModel {
            webhook_id: Set(hook_id),
            event: Set("push".to_string()),
            delivery_id: Set("11111111-1111-1111-1111-111111111111".to_string()),
            request_payload: Set(Some("{\"truncated\"".to_string())),
            response_status: Set(Some(200)),
            response_body: Set(None),
            duration_ms: Set(Some(1)),
            created_at: Set(Utc::now()),
            ..Default::default()
        },
    )
    .await
    .expect("record a delivery whose payload did not survive");

    let response = client
        .post(format!(
            "{base}/api/v1/repos/hook-owner/hooks/hooks/{hook_id}/deliveries/{}/redeliver",
            unreadable.id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("redeliver");
    assert_eq!(
        response.status(),
        StatusCode::INTERNAL_SERVER_ERROR,
        "a payload we cannot reproduce must stop the resend, not become a `null` body"
    );
    let body = response.text().await.unwrap_or_default();
    assert!(
        !body.contains("redelivery triggered"),
        "nothing was resent, so nothing may be reported as triggered, got: {body}"
    );

    // The other half of the same rule: a row that never recorded a payload.
    let absent = rg_db::ops::webhook_ops::create_delivery(
        &db,
        rg_db::entities::webhook_delivery::ActiveModel {
            webhook_id: Set(hook_id),
            event: Set("push".to_string()),
            delivery_id: Set("22222222-2222-2222-2222-222222222222".to_string()),
            request_payload: Set(None),
            response_status: Set(None),
            response_body: Set(None),
            duration_ms: Set(None),
            created_at: Set(Utc::now()),
            ..Default::default()
        },
    )
    .await
    .expect("record a delivery with no payload at all");

    let response = client
        .post(format!(
            "{base}/api/v1/repos/hook-owner/hooks/hooks/{hook_id}/deliveries/{}/redeliver",
            absent.id
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("redeliver");
    assert_eq!(
        response.status(),
        StatusCode::CONFLICT,
        "there is nothing to resend, and that is a fact about the resource, not a bad request"
    );
}
