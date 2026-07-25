//! Integration tests for opt-in release-asset provenance attestation.
//!
//! Guards:
//!   POST /repos/:o/:r/releases/assets/:id/attestation         — sign
//!   GET  /repos/:o/:r/releases/assets/:id/attestation         — fetch envelope
//!   POST /repos/:o/:r/releases/assets/:id/attestation/verify  — verify

mod common;

use common::{
    build_test_app_state, create_repo, register_user, setup_test_db, spawn_test_app,
};

const PW: &str = "Qz7$wRtm";

async fn create_release(base: &str, token: &str, owner: &str, repo: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/releases"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "tag_name": "v1.0.0", "title": "Rel" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let rel: serde_json::Value = resp.json().await.unwrap();
    rel["id"].as_i64().unwrap()
}

async fn upload_asset(base: &str, token: &str, owner: &str, repo: &str, release_id: i64) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets"
        ))
        .bearer_auth(token)
        .header("content-type", "text/plain")
        .header("content-disposition", "attachment; filename=notes.txt")
        .body("release asset")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201);
    let asset: serde_json::Value = resp.json().await.unwrap();
    asset["id"].as_i64().unwrap()
}

#[tokio::test]
async fn sign_get_verify_round_trip() {
    let base = spawn_test_app().await;
    let owner = "attuser".to_string();
    let token = register_user(&base, &owner, "attuser@example.com", PW).await;
    let repo = "attrepo".to_string();
    create_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;
    let client = reqwest::Client::new();

    // Sign → 201 DSSE envelope.
    let signed = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(signed.status(), 201);
    let envelope: serde_json::Value = signed.json().await.unwrap();
    assert_eq!(envelope["payloadType"], "application/vnd.in-toto+json");
    assert!(envelope["payload"].as_str().unwrap().len() > 0);
    assert_eq!(envelope["signatures"].as_array().unwrap().len(), 1);

    // Fetch stored envelope → identical.
    let fetched = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(fetched.status(), 200);
    let fetched_env: serde_json::Value = fetched.json().await.unwrap();
    assert_eq!(fetched_env, envelope);

    // Verify → verified against the asset digest and the instance key.
    let verified = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation/verify"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(verified.status(), 200);
    let report: serde_json::Value = verified.json().await.unwrap();
    assert_eq!(report["verified"], true, "report: {report}");
    assert_eq!(
        report["predicate_type"],
        "https://forgekeep.dev/provenance/v1"
    );
    // "release asset" → known SHA-256 (matches the digest step's vector).
    assert_eq!(
        report["asset_sha256"],
        "e6abe9df7db8513616674b02b5edb26c37bf3b2f81daeec1e3c6fc8c9a802850"
    );
}

#[tokio::test]
async fn verify_without_attestation_is_404() {
    let base = spawn_test_app().await;
    let owner = "attuser2".to_string();
    let token = register_user(&base, &owner, "attuser2@example.com", PW).await;
    let repo = "attrepo2".to_string();
    create_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 404);
}

/// With attestation disabled (production default), every endpoint 404s.
#[tokio::test]
async fn disabled_endpoints_return_404() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).ok();
    let mut state = build_test_app_state(db, repo_root);
    state.attestation_enabled = false; // opt-in: off
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    common::wait_for_listener(&addr).await;

    let owner = "attuser3".to_string();
    let token = register_user(&base, &owner, "attuser3@example.com", PW).await;
    let repo = "attrepo3".to_string();
    create_repo(&base, &token, &repo).await;
    let release_id = create_release(&base, &token, &owner, &repo).await;
    let asset_id = upload_asset(&base, &token, &owner, &repo, release_id).await;
    let client = reqwest::Client::new();

    let sign = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(sign.status(), 404, "sign must 404 when feature disabled");

    let verify = client
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/attestation/verify"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(verify.status(), 404, "verify must 404 when feature disabled");
}
