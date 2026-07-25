//! Acceptance tests for inbound external-webhook HMAC verification.
//!
//! Card `card_11de57300ce7`: when an inbound-webhook secret is configured, a
//! request with a missing / wrong signature is rejected (401) and one with a
//! valid `X-Hub-Signature-256` over the raw body passes. With no secret the
//! endpoint keeps its previous auth-only behaviour.

mod common;

use std::sync::Arc;

use common::{
    build_test_app_state, create_repo, register_user, setup_test_db, spawn_test_app_with_db,
};

/// Produce the `sha256=<hex>` header value an external sender would send.
fn sign(secret: &str, body: &[u8]) -> String {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
}

/// Spawn a test server whose `AppState` carries the given inbound-webhook secret.
async fn spawn_with_secret(secret: &str) -> String {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let mut state = build_test_app_state(db, repo_root);
    state.external_webhook_secret = Some(Arc::new(secret.to_string()));
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base_url = format!("http://{addr}");
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    common::wait_for_listener(&addr).await;
    base_url
}

#[tokio::test]
async fn external_ci_webhook_enforces_hmac_when_secret_configured() {
    let secret = "shared-webhook-secret-1234567890";
    let base = spawn_with_secret(secret).await;
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
        .header("X-Hub-Signature-256", sign("attacker-secret", body.as_bytes()))
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
        "no configured secret ⇒ endpoint stays auth-only"
    );
}
