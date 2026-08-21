//! card_dc0f5d58e5f4: dismissing a review must withdraw the approval it names.
//!
//! `dismiss_review` used to insert a *second* `pr_reviews` row with
//! `action = "dismiss"` under the *dismissor's* `reviewer_id`, and nothing read
//! it. `count_current_approvals` folds only `approve` / `request_changes` into
//! its per-reviewer verdict map, so the dismissal displaced nobody — and had it
//! been folded in, it carried the wrong reviewer's id and would have displaced
//! the wrong verdict. The maintainer got a `200` and a timeline entry while the
//! pull request stayed mergeable on exactly the approval they had withdrawn:
//! the operation reported success and did nothing.
//!
//! Each test asserts the gate **before** the dismissal as well, so a suite in
//! which nothing approves anything cannot satisfy it.

use rg_db::sea_orm::{NotSet, Set};

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

const HEAD_SHA: &str = "3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c3c";

async fn seed_pr(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    author_id: i64,
    reviewer_id: i64,
) -> rg_db::entities::pull_request::Model {
    let now = chrono::Utc::now();
    rg_db::ops::pull_request_ops::create(
        db,
        rg_db::entities::pull_request::ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            number: Set(1),
            title: Set("A withdrawn approval must stop authorizing".to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            is_draft: Set(false),
            auto_merge_enabled: Set(false),
            auto_merge_strategy: Set(None),
            auto_merge_enabled_by_id: Set(None),
            auto_merge_enabled_at: Set(None),
            author_id: Set(author_id),
            reviewer_id: Set(Some(reviewer_id)),
            head_branch: Set("feature".to_string()),
            base_branch: Set("main".to_string()),
            head_sha: Set(Some(HEAD_SHA.to_string())),
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
    .expect("seed pull request")
}

async fn seed_approval(
    db: &rg_db::DatabaseConnection,
    pr: &rg_db::entities::pull_request::Model,
    reviewer_id: i64,
    at: chrono::DateTime<chrono::Utc>,
) -> rg_db::entities::pr_review::Model {
    rg_db::ops::pr_review_ops::create(
        db,
        rg_db::entities::pr_review::ActiveModel {
            id: NotSet,
            pr_id: Set(pr.id),
            repo_id: Set(pr.repo_id),
            reviewer_id: Set(reviewer_id),
            action: Set("approve".to_string()),
            body: Set(Some("Looks good".to_string())),
            commit_id: Set(Some(HEAD_SHA.to_string())),
            created_at: Set(at),
            dismissed_at: Set(None),
            dismissed_by: Set(None),
        },
    )
    .await
    .expect("seed approval")
}

async fn require_one_approval(base: &str, token: &str, owner: &str, repo: &str) {
    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/branches/protection"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "branch_name": "main",
            "require_approval": true,
            "required_approvals": 1
        }))
        .send()
        .await
        .expect("create branch protection");
    assert_eq!(
        resp.status(),
        201,
        "branch protection setup failed: {}",
        resp.text().await.unwrap_or_default()
    );
}

async fn dismiss(base: &str, token: &str, owner: &str, repo: &str, review_id: i64) -> String {
    let resp = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/pulls/1/reviews/{review_id}/dismiss"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({"message": "stale — the branch moved on"}))
        .send()
        .await
        .expect("dismiss review");
    let status = resp.status();
    let body = resp.text().await.unwrap_or_default();
    assert!(
        status.is_success(),
        "dismissing a review answered {status}: {body}"
    );
    body
}

/// The whole card in one path: approve, gate opens, dismiss, gate closes.
#[tokio::test]
async fn a_dismissed_approval_stops_satisfying_required_approvals() {
    let (base, db) = spawn_test_app_with_db().await;
    let (host_token, host_id) =
        register_full(&base, "dismiss-host", "dismiss-host@example.com").await;
    let (_reviewer_token, reviewer_id) =
        register_full(&base, "dismiss-reviewer", "dismiss-reviewer@example.com").await;
    let repo_id = create_repo(&base, &host_token, "withdrawn-approval").await;

    let pr = seed_pr(&db, repo_id, host_id, reviewer_id).await;
    let review = seed_approval(&db, &pr, reviewer_id, chrono::Utc::now()).await;
    require_one_approval(&base, &host_token, "dismiss-host", "withdrawn-approval").await;

    // Baseline: the approval really does open the gate, so the assertions
    // after the dismissal cannot be satisfied by a gate that refuses always.
    assert_eq!(
        rg_db::ops::pr_review_ops::count_current_approvals(&db, pr.id, Some(HEAD_SHA))
            .await
            .expect("count approvals"),
        1,
        "a live reviewer's standing approval must count"
    );
    rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr.id)
        .await
        .expect("a standing approval must satisfy the gate");

    dismiss(
        &base,
        &host_token,
        "dismiss-host",
        "withdrawn-approval",
        review.id,
    )
    .await;

    assert_eq!(
        rg_db::ops::pr_review_ops::count_current_approvals(&db, pr.id, Some(HEAD_SHA))
            .await
            .expect("count approvals after dismissal"),
        0,
        "the dismissed approval is still being counted — the 200 meant nothing"
    );
    let refused =
        rg_core::branch_protection::service::check_merge_allowed(&db, repo_id, "main", pr.id)
            .await
            .expect_err("a dismissed approval still authorizes the merge");
    assert!(
        format!("{refused:#}").contains("requires at least 1 approval(s), got 0"),
        "unexpected merge-gate error: {refused:#}"
    );

    // The withdrawal is a property of the review it names, recorded on that
    // row — not a separate opinion filed under whoever pressed the button.
    let stored = rg_db::ops::pr_review_ops::find_by_id(&db, review.id)
        .await
        .expect("read the dismissed review")
        .expect("the review disappeared instead of being withdrawn");
    assert!(
        stored.dismissed_at.is_some(),
        "the dismissal was not stamped on the review it dismissed"
    );
    assert_eq!(
        stored.dismissed_by,
        Some(host_id),
        "the review does not record who withdrew it"
    );
    assert_eq!(
        stored.reviewer_id, reviewer_id,
        "the dismissal overwrote the reviewer of the original review"
    );

    // "Who did what, when" still reaches the timeline.
    let timeline: Vec<serde_json::Value> = reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/dismiss-host/withdrawn-approval/pulls/1/timeline"
        ))
        .bearer_auth(&host_token)
        .send()
        .await
        .expect("read timeline")
        .json()
        .await
        .expect("timeline body");
    assert!(
        timeline
            .iter()
            .any(|event| event["kind"] == "review_dismiss"),
        "the dismissal left no timeline entry: {timeline:?}"
    );
}

/// Dismissing a reviewer's latest approval must not promote an older one of
/// theirs back into the count. A withdrawn verdict is still that reviewer's
/// last word; it simply authorizes nothing.
#[tokio::test]
async fn dismissing_the_latest_approval_does_not_resurrect_an_earlier_one() {
    let (base, db) = spawn_test_app_with_db().await;
    let (host_token, host_id) =
        register_full(&base, "revive-host", "revive-host@example.com").await;
    let (_reviewer_token, reviewer_id) =
        register_full(&base, "revive-reviewer", "revive-reviewer@example.com").await;
    let repo_id = create_repo(&base, &host_token, "no-resurrection").await;

    let pr = seed_pr(&db, repo_id, host_id, reviewer_id).await;
    let now = chrono::Utc::now();
    let _earlier = seed_approval(&db, &pr, reviewer_id, now - chrono::Duration::minutes(10)).await;
    let latest = seed_approval(&db, &pr, reviewer_id, now).await;
    require_one_approval(&base, &host_token, "revive-host", "no-resurrection").await;

    assert_eq!(
        rg_db::ops::pr_review_ops::count_current_approvals(&db, pr.id, Some(HEAD_SHA))
            .await
            .expect("count approvals"),
        1,
        "two approvals from one reviewer must still count once"
    );

    dismiss(
        &base,
        &host_token,
        "revive-host",
        "no-resurrection",
        latest.id,
    )
    .await;

    assert_eq!(
        rg_db::ops::pr_review_ops::count_current_approvals(&db, pr.id, Some(HEAD_SHA))
            .await
            .expect("count approvals after dismissal"),
        0,
        "the reviewer's earlier approval came back to life when their latest was withdrawn"
    );
}

/// A dismissal names the review it withdraws. Submitting one as a free-standing
/// review used to mint a row that withdrew nothing and answered `201`.
#[tokio::test]
async fn a_review_cannot_be_submitted_as_a_dismissal() {
    let (base, db) = spawn_test_app_with_db().await;
    let (host_token, host_id) = register_full(&base, "inert-host", "inert-host@example.com").await;
    let (reviewer_token, reviewer_id) =
        register_full(&base, "inert-reviewer", "inert-reviewer@example.com").await;
    let repo_id = create_repo(&base, &host_token, "inert-dismissal").await;
    let pr = seed_pr(&db, repo_id, host_id, reviewer_id).await;

    let client = reqwest::Client::new();
    let reviews_url = format!("{base}/api/v1/repos/inert-host/inert-dismissal/pulls/1/reviews");

    // Baseline: this reviewer can submit a review through this endpoint at all.
    let accepted = client
        .post(&reviews_url)
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({"action": "comment", "body": "reading it now"}))
        .send()
        .await
        .expect("submit comment review");
    assert!(
        accepted.status().is_success(),
        "the baseline review was refused: {}",
        accepted.text().await.unwrap_or_default()
    );

    let resp = client
        .post(&reviews_url)
        .bearer_auth(&reviewer_token)
        .json(&serde_json::json!({"action": "dismiss", "body": "stale"}))
        .send()
        .await
        .expect("submit dismissal as a review");
    assert_eq!(
        resp.status(),
        400,
        "a free-standing dismissal must be refused, not filed as an inert row"
    );

    let reviews = rg_db::ops::pr_review_ops::list_by_pr(&db, pr.id)
        .await
        .expect("list reviews");
    assert!(
        reviews.iter().all(|review| review.action != "dismiss"),
        "the refused dismissal was written anyway: {reviews:?}"
    );
}
