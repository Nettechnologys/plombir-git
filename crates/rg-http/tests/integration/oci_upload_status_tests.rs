//! OCI resumable-upload status is part of the recovery protocol, not a debug view.
//!
//! After rejecting an out-of-order chunk with `416`, a distribution client may
//! `GET` the upload `Location`, read the acknowledged `Range`, and continue from
//! there. These tests drive that route and prove the read neither rewrites the
//! session row nor touches staged bytes.

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use sea_orm::{ActiveModelTrait, Set};
use sha2::Digest as _;

use crate::common::{
    build_test_app_state, create_repo, register_full, setup_test_db, spawn_test_app_with_state,
};

const ABSENT_UUID: &str = "be49a424-e076-4f87-988c-68d5cfaed3df";

fn sha256(payload: &[u8]) -> String {
    format!("sha256:{}", hex::encode(sha2::Sha256::digest(payload)))
}

async fn start_upload(base: &str, token: &str, owner: &str, repo: &str) -> (String, String) {
    let response = reqwest::Client::new()
        .post(format!("{base}/v2/{owner}/{repo}/blobs/uploads/"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 202, "start upload failed");
    let location = response
        .headers()
        .get(reqwest::header::LOCATION)
        .and_then(|value| value.to_str().ok())
        .expect("start upload must return a Location")
        .to_string();
    let uuid = location
        .rsplit('/')
        .next()
        .expect("upload uuid in Location")
        .to_string();
    (location, uuid)
}

async fn upload_row(
    db: &rg_db::DatabaseConnection,
    uuid: &str,
) -> rg_db::entities::oci_upload::Model {
    rg_db::ops::oci_ops::find_upload(db, uuid)
        .await
        .expect("read upload row")
        .expect("live upload row")
}

async fn error_answer(response: reqwest::Response) -> (StatusCode, Vec<u8>) {
    let status = response.status();
    let body = response.bytes().await.unwrap().to_vec();
    (status, body)
}

#[tokio::test]
async fn status_get_recovers_after_416_without_mutating_the_upload() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();
    let (token, _) =
        register_full(&base, "oci_status_resume", "oci_status_resume@example.com").await;
    create_repo(&base, &token, "resume-layer").await;

    let first_chunk = b"0123456789";
    let final_chunk = b"abcde";
    let (location, uuid) = start_upload(&base, &token, "oci_status_resume", "resume-layer").await;
    let staged = state
        .oci_storage
        .upload_file("oci_status_resume", "resume-layer", &uuid);

    let first = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .header("content-range", "0-9")
        .body(first_chunk.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 202);

    let before_row = upload_row(&db, &uuid).await;
    let before_bytes = std::fs::read(&staged).expect("read staged upload");
    assert_eq!(before_row.bytes_uploaded, first_chunk.len() as i64);
    assert_eq!(before_bytes, first_chunk);

    let refused = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .header("content-range", "15-19")
        .body(final_chunk.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), 416);

    let status = client
        .get(format!("{base}{location}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(status.status(), StatusCode::NO_CONTENT);
    assert_eq!(
        status
            .headers()
            .get(reqwest::header::LOCATION)
            .and_then(|value| value.to_str().ok()),
        Some(location.as_str())
    );
    assert_eq!(
        status
            .headers()
            .get(reqwest::header::RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("0-9")
    );
    assert_eq!(
        status
            .headers()
            .get("docker-upload-uuid")
            .and_then(|value| value.to_str().ok()),
        Some(uuid.as_str())
    );
    assert!(status.bytes().await.unwrap().is_empty());

    assert_eq!(
        upload_row(&db, &uuid).await,
        before_row,
        "status GET changed the session row"
    );
    assert_eq!(
        std::fs::read(&staged).expect("read staged upload after status GET"),
        before_bytes,
        "status GET changed staged bytes"
    );

    let resumed = client
        .patch(format!("{base}{location}"))
        .bearer_auth(&token)
        .header("content-range", "10-14")
        .body(final_chunk.to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(resumed.status(), 202);
    assert_eq!(
        resumed
            .headers()
            .get(reqwest::header::RANGE)
            .and_then(|value| value.to_str().ok()),
        Some("0-14")
    );

    let payload = [first_chunk.as_slice(), final_chunk.as_slice()].concat();
    let digest = sha256(&payload);
    let complete = client
        .put(format!("{base}{location}"))
        .query(&[("digest", digest.as_str())])
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    crate::common::assert_blob_push_created(complete, payload.len()).await;
}

#[tokio::test]
async fn foreign_upload_status_is_indistinguishable_from_an_absent_uuid() {
    let (base, db, _state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();
    let (victim, _) =
        register_full(&base, "oci_status_victim", "oci_status_victim@example.com").await;
    create_repo(&base, &victim, "victim-layer").await;
    let (attacker, _) = register_full(
        &base,
        "oci_status_attacker",
        "oci_status_attacker@example.com",
    )
    .await;
    create_repo(&base, &attacker, "attacker-layer").await;

    let (victim_location, victim_uuid) =
        start_upload(&base, &victim, "oci_status_victim", "victim-layer").await;
    let first = client
        .patch(format!("{base}{victim_location}"))
        .bearer_auth(&victim)
        .body(b"private offset".to_vec())
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), 202);
    let before = upload_row(&db, &victim_uuid).await;

    // Ensure the attacker's namespace has its own OCI repository row. The two
    // lookups then differ only in whether the named uuid belongs to that row.
    start_upload(&base, &attacker, "oci_status_attacker", "attacker-layer").await;

    let foreign = client
        .get(format!(
            "{base}/v2/oci_status_attacker/attacker-layer/blobs/uploads/{victim_uuid}"
        ))
        .bearer_auth(&attacker)
        .send()
        .await
        .unwrap();
    let absent = client
        .get(format!(
            "{base}/v2/oci_status_attacker/attacker-layer/blobs/uploads/{ABSENT_UUID}"
        ))
        .bearer_auth(&attacker)
        .send()
        .await
        .unwrap();
    let foreign = error_answer(foreign).await;
    let absent = error_answer(absent).await;

    assert_eq!(foreign, absent, "a foreign uuid leaked a distinct answer");
    assert_eq!(foreign.0, StatusCode::NOT_FOUND);
    let body: serde_json::Value = serde_json::from_slice(&foreign.1).unwrap();
    assert_eq!(body["errors"][0]["code"], "BLOB_UPLOAD_UNKNOWN");
    assert_eq!(
        upload_row(&db, &victim_uuid).await,
        before,
        "a foreign status read changed the victim's session"
    );
}

#[tokio::test]
async fn expired_upload_status_is_unknown_before_the_retention_sweep_runs() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let client = reqwest::Client::new();
    let (token, _) = register_full(
        &base,
        "oci_status_expired",
        "oci_status_expired@example.com",
    )
    .await;
    create_repo(&base, &token, "expired-layer").await;
    let (location, uuid) = start_upload(&base, &token, "oci_status_expired", "expired-layer").await;
    let staged = state
        .oci_storage
        .upload_file("oci_status_expired", "expired-layer", &uuid);

    let row = upload_row(&db, &uuid).await;
    let mut aged: rg_db::entities::oci_upload::ActiveModel = row.into();
    aged.expires_at = Set(chrono::Utc::now() - chrono::Duration::hours(1));
    aged.update(&db).await.expect("expire upload session");

    let expired = client
        .get(format!("{base}{location}"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let absent = client
        .get(format!(
            "{base}/v2/oci_status_expired/expired-layer/blobs/uploads/{ABSENT_UUID}"
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let expired = error_answer(expired).await;
    let absent = error_answer(absent).await;
    assert_eq!(
        expired, absent,
        "an expired session leaked a distinct answer from an absent uuid"
    );
    assert_eq!(expired.0, StatusCode::NOT_FOUND);
    let body: serde_json::Value = serde_json::from_slice(&expired.1).unwrap();
    assert_eq!(body["errors"][0]["code"], "BLOB_UPLOAD_UNKNOWN");
    assert!(
        staged.is_file(),
        "status lookup must not perform the retention sweep itself"
    );
    assert!(
        rg_db::ops::oci_ops::find_upload(&db, &uuid)
            .await
            .unwrap()
            .is_some(),
        "status lookup must not delete an expired row"
    );
}

#[tokio::test]
async fn upload_status_db_outage_keeps_the_oci_503_envelope() {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).expect("create repo root");
    let state = build_test_app_state(db.clone(), repo_root);
    db.close().await.expect("close pool");

    let response = rg_http::oci::get_upload_status(
        State(state),
        HeaderMap::new(),
        Path((
            "owner".to_string(),
            "repo".to_string(),
            ABSENT_UUID.to_string(),
        )),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let body = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read OCI outage body");
    let json: serde_json::Value = serde_json::from_slice(&body).expect("body is JSON");
    assert!(json["errors"].is_array(), "OCI envelope missing: {json}");
    assert!(
        json.get("error").is_none(),
        "AppError envelope leaked: {json}"
    );
}
