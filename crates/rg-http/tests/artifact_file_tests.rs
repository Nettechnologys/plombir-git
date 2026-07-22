mod common;

use common::{register_full, spawn_test_app_with_db};
use sea_orm::{ActiveModelTrait, Set};

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
