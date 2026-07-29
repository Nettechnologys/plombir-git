use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::{ActiveModelTrait, Set};
use sha2::{Digest, Sha256};

async fn create_private_repo(base: &str, token: &str, name: &str) -> i64 {
    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "name": name,
            "is_private": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create private repo failed");
    resp.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn create_assigned_job(
    db: &rg_db::DatabaseConnection,
    repo_id: i64,
    runner_id: i64,
) -> (i64, i64) {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .unwrap();
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .unwrap();
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "unit", "echo ok", None, None, None, None, None, false, None, None, None,
    )
    .await
    .unwrap();
    rg_db::ops::pipeline_ops::assign_job(db, job.id, runner_id)
        .await
        .unwrap();
    (pipeline.id, job.id)
}

#[tokio::test]
async fn artifact_raw_upload_persists_file_and_download_respects_repo_read() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) =
        register_full(&base, "artifact_owner", "artifact_owner@example.com").await;
    let repo_id = create_private_repo(&base, &owner_token, "private-artifacts").await;
    let runner =
        rg_db::ops::runner_ops::register_runner(&db, "artifact-runner", "", None, None, None)
            .await
            .unwrap();
    let (pipeline_id, job_id) = create_assigned_job(&db, repo_id, runner.id).await;

    let policy_url =
        format!("{base}/api/v1/repos/artifact_owner/private-artifacts/actions/retention");
    let default_policy = client
        .get(&policy_url)
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(default_policy.status(), 200);
    assert_eq!(
        default_policy.json::<serde_json::Value>().await.unwrap()["artifact_retention_days"],
        30
    );
    assert_eq!(
        client
            .put(&policy_url)
            .bearer_auth(&owner_token)
            .json(&serde_json::json!({"artifact_retention_days": 1, "cache_retention_days": 2}))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    let upload_resp = client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/artifacts",
            base, runner.id, job_id
        ))
        .bearer_auth(&runner.token)
        .header("x-artifact-name", "report.txt")
        .body("artifact bytes")
        .send()
        .await
        .unwrap();
    let upload_status = upload_resp.status();
    let upload_body = upload_resp.text().await.unwrap();
    assert_eq!(upload_status, 201, "upload failed: {upload_body}");
    let uploaded: serde_json::Value = serde_json::from_str(&upload_body).unwrap();
    let artifact_id = uploaded["id"].as_i64().unwrap();
    let stored = rg_db::ops::artifact_ops::get_by_id(&db, artifact_id)
        .await
        .unwrap()
        .unwrap();
    assert!(stored.expires_at.is_some());
    assert!(stored.file_path.starts_with("artifacts/jobs/"));
    assert!(!std::path::Path::new(&stored.file_path).is_absolute());
    // Upload records a SHA-256 digest of the artifact bytes.
    let expected_sha = hex::encode(Sha256::digest(b"artifact bytes"));
    assert_eq!(stored.sha256.as_deref(), Some(expected_sha.as_str()));
    assert_eq!(expected_sha.len(), 64);

    let anon_download = client
        .get(format!(
            "{}/api/v1/artifacts/{}/download",
            base, artifact_id
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(anon_download.status(), 401);

    let owner_download = client
        .get(format!(
            "{}/api/v1/artifacts/{}/download",
            base, artifact_id
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(owner_download.status(), 200);
    // Download echoes the digest so clients can verify the payload end-to-end.
    assert_eq!(
        owner_download
            .headers()
            .get("x-checksum-sha256")
            .and_then(|v| v.to_str().ok()),
        Some(expected_sha.as_str()),
    );
    assert_eq!(
        owner_download.bytes().await.unwrap().as_ref(),
        b"artifact bytes"
    );

    let list_resp = client
        .get(format!(
            "{}/api/v1/repos/artifact_owner/private-artifacts/pipelines/{}/artifacts",
            base, pipeline_id
        ))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(list_resp.status(), 200);
    let listed: serde_json::Value = list_resp.json().await.unwrap();
    assert_eq!(listed.as_array().unwrap().len(), 1);

    let mut expired: rg_db::entities::artifact::ActiveModel = stored.into();
    expired.expires_at = Set(Some(chrono::Utc::now() - chrono::Duration::minutes(1)));
    expired.update(&db).await.unwrap();
    let cleanup = client
        .delete(format!("{policy_url}/expired"))
        .bearer_auth(&owner_token)
        .send()
        .await
        .unwrap();
    assert_eq!(cleanup.status(), 200);
    assert_eq!(
        cleanup.json::<serde_json::Value>().await.unwrap()["artifacts_deleted"],
        1
    );
    assert!(rg_db::ops::artifact_ops::get_by_id(&db, artifact_id)
        .await
        .unwrap()
        .is_none());
}

/// The four artifact routes take their access level as a *type* now —
/// `ArtifactRead` / `ArtifactWrite` resolve the repository from the artifact
/// and gate it before the handler is entered (card_1ec383429aea). This drives
/// the live routes, because the gate being in the signature is exactly what no
/// call to a helper can prove any more.
///
/// The baseline is in the same test and comes first: the owner reads, lists and
/// finally deletes the very artifact the outsider is refused, so a wall of
/// denials cannot be a dead fixture reported as a passing security test.
#[tokio::test]
async fn a_private_artifact_is_refused_to_an_outsider_and_kept_for_its_owner() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (owner_token, _owner_id) = register_full(
        &base,
        "artifact_gate_owner",
        "artifact_gate_owner@example.com",
    )
    .await;
    let (outsider_token, _outsider_id) = register_full(
        &base,
        "artifact_gate_outsider",
        "artifact_gate_outsider@example.com",
    )
    .await;

    let repo_id = create_private_repo(&base, &owner_token, "gated-artifacts").await;
    // A second repository of the *same* owner: reading it is the owner's right,
    // so a pipeline of this one listed through the other one's URL is refused
    // by the anchoring, not by the gate.
    let other_repo_id = create_private_repo(&base, &owner_token, "gated-artifacts-two").await;
    let runner =
        rg_db::ops::runner_ops::register_runner(&db, "artifact-gate-runner", "", None, None, None)
            .await
            .unwrap();
    let (pipeline_id, job_id) = create_assigned_job(&db, repo_id, runner.id).await;
    let (other_pipeline_id, _other_job_id) =
        create_assigned_job(&db, other_repo_id, runner.id).await;

    let upload = client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/artifacts",
            base, runner.id, job_id
        ))
        .bearer_auth(&runner.token)
        .header("x-artifact-name", "report.txt")
        .body("artifact bytes")
        .send()
        .await
        .unwrap();
    assert_eq!(upload.status(), 201);
    let artifact_id = upload.json::<serde_json::Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let metadata = format!("{base}/api/v1/artifacts/{artifact_id}");
    let download = format!("{base}/api/v1/artifacts/{artifact_id}/download");
    let listing = format!(
        "{base}/api/v1/repos/artifact_gate_owner/gated-artifacts/pipelines/{pipeline_id}/artifacts"
    );

    // ── Baseline: the artifact is really there and really readable ──────────
    assert_eq!(
        client
            .get(&metadata)
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .status(),
        200,
        "the owner cannot read his own artifact — every refusal below would be meaningless"
    );
    assert_eq!(
        client
            .get(&download)
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        client
            .get(&listing)
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .status(),
        200
    );

    // ── The outsider holds a perfectly valid session, and no rights here ────
    for url in [&metadata, &download, &listing] {
        assert_eq!(
            client
                .get(url)
                .bearer_auth(&outsider_token)
                .send()
                .await
                .unwrap()
                .status(),
            403,
            "{url} was served to an outsider"
        );
    }
    assert_eq!(
        client
            .delete(&metadata)
            .bearer_auth(&outsider_token)
            .send()
            .await
            .unwrap()
            .status(),
        403,
        "an outsider deleted an artifact out of a private repository"
    );

    // ── Anonymous: 401, and on the delete route before anything is resolved ─
    assert_eq!(client.get(&metadata).send().await.unwrap().status(), 401);
    assert_eq!(client.get(&download).send().await.unwrap().status(), 401);
    assert_eq!(client.delete(&metadata).send().await.unwrap().status(), 401);

    // ── The pipeline id is instance-wide; the URL's repository is not ───────
    assert_eq!(
        client
            .get(format!(
                "{base}/api/v1/repos/artifact_gate_owner/gated-artifacts/pipelines/{other_pipeline_id}/artifacts"
            ))
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .status(),
        404,
        "a pipeline of another repository was listed through this repository's URL"
    );

    // ── Baseline for the write gate: the owner still owns the delete ────────
    assert_eq!(
        client
            .delete(&metadata)
            .bearer_auth(&owner_token)
            .send()
            .await
            .unwrap()
            .status(),
        204
    );
    assert!(rg_db::ops::artifact_ops::get_by_id(&db, artifact_id)
        .await
        .unwrap()
        .is_none());
}
