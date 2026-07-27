//! Regression coverage for card_7468d39d1867: the tail of the 404-collapse —
//! notifications, branch protection, and the AI routes' repo resolution.
//!
//! Same class as `pr_lookup_failure_status_tests` /
//! `issue_lookup_failure_status_tests` / `review_lookup_failure_status_tests`:
//! any service error became a `404` carrying the error's own text, so a failed
//! query was indistinguishable from a deleted row and the `db: ...` context
//! reached the client (H-05).
//!
//! Notifications carry an extra requirement the other endpoints do not:
//! *another user's* notification must keep answering `404`. Masking somebody
//! else's inbox is the point; masking a broken database is the bug — the two
//! are asserted separately below.
//!
//! Each test drops exactly the table the endpoint reads, so authentication and
//! the repo resolution still succeed. Closing the pool would fail
//! authentication first and the assertions would pass unfixed.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};
use sea_orm::ConnectionTrait;

fn assert_no_internal_detail(body: &serde_json::Value, what: &str) {
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        !message.contains("db:")
            && !message.contains("notifications")
            && !message.contains("protected_branches"),
        "the {what} response body must not carry internal error detail, got: {message}"
    );
}

/// `POST /notifications/{id}/read` and `DELETE /notifications/{id}`.
#[tokio::test]
async fn broken_notification_lookup_is_not_reported_as_a_missing_notification() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "notifail-owner", "notifail@example.com").await;
    let client = reqwest::Client::new();

    // Baseline: this user has no notification #1, and that stays a 404 with a
    // fixed message — including the case where the row belongs to somebody else,
    // which the endpoint must keep masking.
    let resp = client
        .post(format!("{base}/api/v1/notifications/1/read"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 404, "an absent notification is still a 404");
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "notification not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    db.execute_unprepared("DROP TABLE notifications")
        .await
        .expect("drop notifications");

    for (method, url) in [
        ("read", format!("{base}/api/v1/notifications/1/read")),
        ("delete", format!("{base}/api/v1/notifications/1")),
    ] {
        let request = if method == "read" {
            client.post(&url)
        } else {
            client.delete(&url)
        };
        let resp = request.bearer_auth(&token).send().await.expect("request");
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.expect("json body");
        assert!(
            status.is_server_error(),
            "a failed notification {method} must be a 5xx, not {status} — a 404 \
             tells the client the notification is gone and puts nothing in the \
             alerts (body: {body})"
        );
        assert_no_internal_detail(&body, method);
    }
}

/// Another user's notification must still read as absent — the fix must not turn
/// the deliberate masking into a distinguishable "exists but forbidden".
#[tokio::test]
async fn another_users_notification_is_still_reported_as_absent() {
    let (base, db) = spawn_test_app_with_db().await;
    let (owner_token, owner_id) = register_full(&base, "notiowner", "notiowner@example.com").await;
    let (other_token, _) = register_full(&base, "notiother", "notiother@example.com").await;

    let created = rg_db::ops::notification_ops::create_notification(
        &db,
        owner_id,
        "issue",
        "private title",
        Some("private body"),
        None,
    )
    .await
    .expect("create notification");

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/notifications/{}/read", created.id);

    let resp = client
        .post(&url)
        .bearer_auth(&other_token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "somebody else's notification must stay indistinguishable from an absent one"
    );
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(body["error"]["message"], "notification not found");

    // And the owner can still mark it read — the masking is per-user, not a
    // blanket failure.
    let resp = client
        .post(&url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 200, "the owner can still mark it read");
}

/// `GET .../branches/protection/{id}`.
#[tokio::test]
async fn broken_protection_lookup_is_not_reported_as_a_missing_rule() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, _) = register_full(&base, "protfail-owner", "protfail@example.com").await;
    create_repo(&base, &token, "protfail-repo").await;

    let url = format!("{base}/api/v1/repos/protfail-owner/protfail-repo/branches/protection/1");
    let client = reqwest::Client::new();

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        404,
        "an absent protection rule is still a 404"
    );
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert_eq!(
        body["error"]["message"], "protection rule not found",
        "the 404 body must be the fixed message, got: {body}"
    );

    db.execute_unprepared("DROP TABLE protected_branches")
        .await
        .expect("drop protected_branches");

    let resp = client
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a failed protection-rule lookup must be a 5xx, not {status} (body: {body})"
    );
    assert_no_internal_detail(&body, "protection rule");
}
