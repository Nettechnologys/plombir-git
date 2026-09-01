//! CI artifact deletion spans the blob store and the database. A `204` — or a
//! retention sweep counting an artifact as cleaned — is truthful only after the
//! bytes are gone for good; a storage or metadata failure must leave the
//! artifact whole and downloadable.

use sea_orm::{ActiveModelTrait, Set};
use serde_json::Value;

use crate::common::{
    create_repo,
    fault::{fail_db_writes, spawn_test_app_for_fault_sweep, DbWrite, FaultSweepApp},
    register_full, upload_artifact_metadata,
};

/// Publish one artifact through the real runner upload route and hand back its
/// id together with the live path its bytes were written to.
async fn upload_artifact(
    app: &FaultSweepApp,
    repo_id: i64,
    name: &str,
) -> (i64, std::path::PathBuf) {
    let (runner, runner_token) = rg_db::ops::runner_ops::register_runner(
        &app.db,
        repo_id,
        &format!("artifact-runner-{name}"),
        "",
        None,
        None,
        None,
    )
    .await
    .expect("register runner");
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        &app.db,
        repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .expect("create pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(&app.db, pipeline.id, "test", 0)
        .await
        .expect("create stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        &app.db, stage.id, "unit", "echo ok", None, None, None, None, None, None, false, None,
        None, None,
    )
    .await
    .expect("create job");
    rg_db::ops::pipeline_ops::assign_job(&app.db, job.id, runner.id)
        .await
        .expect("assign job");

    let client = reqwest::Client::new();
    let response = upload_artifact_metadata(
        &app.base,
        &client,
        &app.repo_root,
        runner.id,
        job.id,
        &runner_token,
        name,
        format!("bytes of {name}").as_bytes(),
    )
    .await;
    assert_eq!(response.status(), 201, "artifact upload failed");
    let artifact_id = response.json::<Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap();

    let stored = rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
        .await
        .unwrap()
        .expect("uploaded artifact row");
    let live = app.repo_root.join(&stored.file_path);
    assert!(live.exists(), "upload did not write the artifact blob");
    (artifact_id, live)
}

async fn download(app: &FaultSweepApp, token: &str, artifact_id: i64) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{}/api/v1/artifacts/{artifact_id}/download",
            app.base
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("download artifact")
}

async fn delete(app: &FaultSweepApp, token: &str, artifact_id: i64) -> reqwest::Response {
    reqwest::Client::new()
        .delete(format!("{}/api/v1/artifacts/{artifact_id}", app.base))
        .bearer_auth(token)
        .send()
        .await
        .expect("delete artifact")
}

/// Age an artifact out of its retention window without waiting for one.
async fn expire(app: &FaultSweepApp, artifact_id: i64) {
    let row = rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
        .await
        .unwrap()
        .expect("artifact row");
    let mut model: rg_db::entities::artifact::ActiveModel = row.into();
    model.expires_at = Set(Some(chrono::Utc::now() - chrono::Duration::days(1)));
    model.update(&app.db).await.expect("expire artifact");
}

async fn cleanup_expired(app: &FaultSweepApp, token: &str, owner: &str, repo: &str) -> Value {
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/actions/retention/expired",
            app.base
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("run retention cleanup");
    assert_eq!(response.status(), 200, "retention cleanup failed");
    response.json().await.unwrap()
}

/// card_9144e5a17727: the bytes leave the live namespace before the row does, so
/// a backend that refuses the move is a failed DELETE — not a warning on the way
/// to `204` with the artifact already destroyed.
#[tokio::test]
async fn artifact_delete_storage_failure_leaves_the_artifact_downloadable() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "artifact-prepare-fault";
    let repo = "ci";
    let (token, _) = register_full(&app.base, owner, "artifact-prepare-fault@example.com").await;
    let repo_id = create_repo(&app.base, &token, repo).await;
    let (artifact_id, live) = upload_artifact(&app, repo_id, "report.txt").await;

    app.blob_faults.fail_delete();
    let refused = delete(&app, &token, artifact_id).await;
    assert_eq!(
        refused.status(),
        500,
        "blob storage failure was reported as a completed artifact delete"
    );
    assert!(live.exists(), "failed DELETE moved the artifact blob");

    app.blob_faults.heal();
    let still_readable = download(&app, &token, artifact_id).await;
    assert_eq!(
        still_readable.status(),
        200,
        "artifact metadata was deleted"
    );
    assert_eq!(
        still_readable.bytes().await.unwrap().as_ref(),
        b"bytes of report.txt"
    );
}

/// The blob is only staged before the DB write. If that write fails, the exact
/// object moves back and the surviving row remains usable.
#[tokio::test]
async fn artifact_metadata_failure_restores_the_staged_blob() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "artifact-db-fault";
    let repo = "ci";
    let (token, _) = register_full(&app.base, owner, "artifact-db-fault@example.com").await;
    let repo_id = create_repo(&app.base, &token, repo).await;
    let (artifact_id, live) = upload_artifact(&app, repo_id, "restore.txt").await;

    let db_fault = fail_db_writes(&app.db, "artifacts", DbWrite::Delete).await;
    let response = delete(&app, &token, artifact_id).await;
    assert_eq!(response.status(), 500, "DB failure was reported as success");
    db_fault.clear().await;

    assert!(live.exists(), "the staged artifact blob was not put back");
    let restored = download(&app, &token, artifact_id).await;
    assert_eq!(restored.status(), 200, "the surviving row lost its blob");
    assert_eq!(
        restored.bytes().await.unwrap().as_ref(),
        b"bytes of restore.txt"
    );
}

/// The healthy path owes nothing afterwards: no row, no live object, and no
/// private tombstone left for an operator to find.
#[tokio::test]
async fn artifact_delete_retires_the_row_the_blob_and_the_tombstone() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "artifact-delete";
    let repo = "ci";
    let (token, _) = register_full(&app.base, owner, "artifact-delete@example.com").await;
    let repo_id = create_repo(&app.base, &token, repo).await;
    let (artifact_id, live) = upload_artifact(&app, repo_id, "gone.txt").await;

    let deleted = delete(&app, &token, artifact_id).await;
    assert_eq!(deleted.status(), 204, "healthy artifact deletion failed");
    assert!(!live.exists(), "successful DELETE left the artifact blob");
    assert!(
        rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
            .await
            .unwrap()
            .is_none(),
        "successful DELETE left the artifact row"
    );
    let tombstone_root = app
        .repo_root
        .join(format!("_deleted/artifact-deletions/{artifact_id}"));
    assert!(
        std::fs::read_dir(&tombstone_root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "DELETE returned success while an artifact tombstone remained"
    );
}

/// After the row commits there is no safe rollback: the live key is already
/// free, but retained bytes still make `204` a lie. The response and the staged
/// namespace have to make that debt explicit.
#[tokio::test]
async fn artifact_tombstone_cleanup_failure_is_not_reported_as_a_completed_delete() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "artifact-cleanup-fault";
    let repo = "ci";
    let (token, _) = register_full(&app.base, owner, "artifact-cleanup-fault@example.com").await;
    let repo_id = create_repo(&app.base, &token, repo).await;
    let (artifact_id, live) = upload_artifact(&app, repo_id, "retained.txt").await;

    app.blob_faults.fail_delete_prefix();
    let response = delete(&app, &token, artifact_id).await;
    assert_eq!(
        response.status(),
        500,
        "post-commit cleanup failure was reported as a completed delete"
    );
    assert!(!live.exists(), "failed retirement restored the live blob");
    assert!(
        rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
            .await
            .unwrap()
            .is_none(),
        "artifact metadata did not commit before retirement"
    );
    let tombstone_root = app
        .repo_root
        .join(format!("_deleted/artifact-deletions/{artifact_id}"));
    assert!(
        std::fs::read_dir(&tombstone_root)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false),
        "failed cleanup did not leave a discoverable private tombstone"
    );
}

/// The retention sweep deletes on the same terms. Nobody reads its response, so
/// a metadata failure after an irreversible unlink would silently destroy an
/// artifact its row still advertises — and the next pass would report it again.
#[tokio::test]
async fn expired_artifact_cleanup_restores_bytes_when_metadata_delete_fails() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "artifact-retention-fault";
    let repo = "ci";
    let (token, _) = register_full(&app.base, owner, "artifact-retention-fault@example.com").await;
    let repo_id = create_repo(&app.base, &token, repo).await;
    let (artifact_id, live) = upload_artifact(&app, repo_id, "expired.txt").await;
    expire(&app, artifact_id).await;

    let db_fault = fail_db_writes(&app.db, "artifacts", DbWrite::Delete).await;
    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(
        summary["artifacts_deleted"], 0,
        "a failed metadata delete was counted as a cleaned artifact"
    );
    assert_eq!(summary["failures"], 1, "the failure was not reported");
    db_fault.clear().await;

    assert!(live.exists(), "the staged artifact blob was not put back");
    assert!(
        rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
            .await
            .unwrap()
            .is_some(),
        "the artifact row did not survive its failed cleanup"
    );

    // The same sweep, once the database is healthy again, is what actually
    // frees the space — and only then is the artifact counted.
    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(summary["artifacts_deleted"], 1, "retry did not clean up");
    assert_eq!(summary["failures"], 0);
    assert!(!live.exists(), "cleanup left the artifact blob");
    assert!(
        rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
            .await
            .unwrap()
            .is_none(),
        "cleanup left the artifact row"
    );
    let tombstone_root = app
        .repo_root
        .join(format!("_deleted/artifact-deletions/{artifact_id}"));
    assert!(
        std::fs::read_dir(&tombstone_root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "retention counted an artifact whose staged bytes were still parked"
    );
}

/// A retention pass that cannot retire the staged bytes has not reclaimed the
/// space it reports, so the artifact stays on the failure side of the summary.
#[tokio::test]
async fn expired_artifact_cleanup_counts_a_retained_tombstone_as_a_failure() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "artifact-retention-debt";
    let repo = "ci";
    let (token, _) = register_full(&app.base, owner, "artifact-retention-debt@example.com").await;
    let repo_id = create_repo(&app.base, &token, repo).await;
    let (artifact_id, live) = upload_artifact(&app, repo_id, "debt.txt").await;
    expire(&app, artifact_id).await;

    app.blob_faults.fail_delete_prefix();
    let summary = cleanup_expired(&app, &token, owner, repo).await;
    assert_eq!(
        summary["artifacts_deleted"], 0,
        "an artifact whose bytes are still staged was counted as cleaned"
    );
    assert_eq!(summary["failures"], 1, "the cleanup debt was not reported");
    assert!(!live.exists(), "the live blob outlived a committed delete");
}
