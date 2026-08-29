//! Acceptance tests for inbound external-webhook HMAC verification.
//!
//! Card `card_11de57300ce7`: when an inbound-webhook secret is configured, a
//! request with a missing / wrong signature is rejected (401) and one with a
//! valid `X-Hub-Signature-256` over the raw body passes. With no secret the
//! signature check is off entirely.
//!
//! The signature is defense-in-depth *on top of* the endpoint's `RepoWrite`
//! gate, never a replacement for it — `webhook_external_authz_tests` drives
//! that side, including a correctly signed request from an outsider.

use crate::common::{
    create_repo, register_user, spawn_test_app_with_db, spawn_test_app_with_webhook_secret,
};
use sea_orm::{ConnectionTrait, Statement};

/// Produce the `sha256=<hex>` header value an external sender would send.
fn sign(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

#[tokio::test]
async fn external_ci_webhook_enforces_hmac_when_secret_configured() {
    let secret = "shared-webhook-secret-1234567890";
    let (base, _db) = spawn_test_app_with_webhook_secret(secret).await;
    let token = register_user(&base, "hookuser", "hook@example.com", "Qz7$wRtm").await;
    create_repo(&base, &token, "hookrepo").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/hookuser/hookrepo/webhooks/external/ci");
    let body = r#"{"context":"jenkins/pipe","state":"success"}"#;

    // (a) authenticated, but NO signature header → 401.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "missing signature must be rejected");

    // (b) authenticated, WRONG signature (computed with another secret) → 401.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .header(
            "X-Hub-Signature-256",
            sign("attacker-secret", body.as_bytes()),
        )
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "wrong signature must be rejected");

    // (c) authenticated, CORRECT signature over the exact bytes → 200.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .header("X-Hub-Signature-256", sign(secret, body.as_bytes()))
        .body(body.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "correct signature must pass");
}

#[tokio::test]
async fn external_ci_webhook_skips_hmac_when_no_secret() {
    // Default test state has no inbound-webhook secret → signature check is off.
    let (base, _db) = spawn_test_app_with_db().await;
    let token = register_user(&base, "nohook", "nohook@example.com", "Qz7$wRtm").await;
    create_repo(&base, &token, "nohookrepo").await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/nohook/nohookrepo/webhooks/external/ci");
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .header("content-type", "application/json")
        .body(r#"{"context":"ci","state":"pending"}"#.to_string())
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        200,
        "no configured secret ⇒ the access gate is the only thing standing here"
    );
}

#[tokio::test]
async fn external_ci_repository_cascade_after_status_read_is_404() {
    let (base, db) = spawn_test_app_with_db().await;
    let token = register_user(
        &base,
        "status-race-owner",
        "status-race-owner@example.com",
        "Qz7$wRtm",
    )
    .await;
    create_repo(&base, &token, "status-race").await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/status-race-owner/status-race/webhooks/external/ci");
    let initial = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"context":"ci/build","state":"pending"}))
        .send()
        .await
        .expect("publish the status the raced request observes");
    assert_eq!(initial.status(), 200);

    let repo =
        rg_core::repo::service::find_repo_by_owner_name(&db, "status-race-owner", "status-race")
            .await
            .expect("read the repository")
            .expect("the repository exists before the race");
    let status = rg_db::ops::commit_status_ops::list_by_sha(&db, repo.id, "")
        .await
        .expect("read the initial status")
        .into_iter()
        .next()
        .expect("the initial status exists");
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER delete_status_repo_before_external_update \
             BEFORE UPDATE ON commit_statuses WHEN OLD.id = {} \
             BEGIN DELETE FROM repositories WHERE id = OLD.repo_id; END",
            status.id
        ),
    ))
    .await
    .expect("install the competing repository delete");

    let raced = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"context":"ci/build","state":"success"}))
        .send()
        .await
        .expect("race the repeated status report against repository deletion");
    assert_eq!(
        raced.status(),
        404,
        "repository deletion after the status read must stay typed absence"
    );
    let body = raced.text().await.expect("read the typed error body");
    assert!(body.contains("repository not found"), "{body}");
    assert!(!body.contains("RecordNotUpdated"), "{body}");
    assert!(rg_db::ops::repo_ops::find_by_id(&db, repo.id)
        .await
        .expect("look for the deleted repository")
        .is_none());
    assert!(
        rg_db::ops::commit_status_ops::list_by_sha(&db, repo.id, "")
            .await
            .expect("look for a resurrected status")
            .is_empty(),
        "the losing webhook recreated a status after repository deletion"
    );
}
