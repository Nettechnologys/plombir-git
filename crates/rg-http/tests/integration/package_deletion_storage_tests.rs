//! Deleting a package version spans one blob prefix and two metadata tables. A
//! `204` is truthful only after all three are gone; any failure before that must
//! leave the published version whole — every file of it, not the ones a
//! per-object loop had not reached yet.

use serde_json::Value;

use crate::common::{
    create_repo,
    fault::{fail_db_writes, spawn_test_app_for_fault_sweep, DbWrite, FaultSweepApp},
    register_full,
};

const OWNER_FILE: &str = "sample.bin";
const SECOND_FILE: &str = "extra.bin";

async fn publish(app: &FaultSweepApp, token: &str, owner: &str, repo: &str, filename: &str) {
    let response = reqwest::Client::new()
        .post(format!(
            "{}/api/v1/repos/{owner}/{repo}/packages/generic/publish?name=sample&version=1.0.0",
            app.base
        ))
        .bearer_auth(token)
        .header(
            reqwest::header::CONTENT_DISPOSITION,
            format!("attachment; filename=\"{filename}\""),
        )
        .body(format!("bytes of {filename}"))
        .send()
        .await
        .expect("publish package");
    let status = response.status();
    let body = response.text().await.unwrap();
    // A second file published under the same name/version joins the existing
    // version rather than creating one, and says so with `200`.
    assert!(
        status == 201 || status == 200,
        "publish failed: {status} {body}"
    );
}

async fn download(
    app: &FaultSweepApp,
    token: &str,
    owner: &str,
    repo: &str,
    filename: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/{owner}/{repo}/packages/generic/sample/1.0.0/{filename}",
            app.base
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("download package file")
}

async fn delete_version(
    app: &FaultSweepApp,
    token: &str,
    owner: &str,
    repo: &str,
) -> reqwest::Response {
    reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/packages/generic/sample/1.0.0",
            app.base
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("delete package version")
}

async fn version_files(app: &FaultSweepApp, owner: &str, repo: &str, token: &str) -> Value {
    let response = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/{owner}/{repo}/packages/generic/sample/1.0.0",
            app.base
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("read package version");
    assert_eq!(response.status(), 200, "package version is not published");
    response.json().await.unwrap()
}

/// The live prefix a healthy publish writes under.
fn live_prefix(app: &FaultSweepApp, owner: &str, repo: &str) -> std::path::PathBuf {
    app.repo_root
        .join(format!("packages/{owner}/{repo}/generic/sample/1.0.0"))
}

/// Assert both published files still answer with their own bytes.
async fn assert_both_files_readable(app: &FaultSweepApp, token: &str, owner: &str, repo: &str) {
    for filename in [OWNER_FILE, SECOND_FILE] {
        let response = download(app, token, owner, repo, filename).await;
        assert_eq!(
            response.status(),
            200,
            "package file {filename} is no longer downloadable"
        );
        assert_eq!(
            response.bytes().await.unwrap().as_ref(),
            format!("bytes of {filename}").as_bytes()
        );
    }
}

/// card_337012d224a5: the version's objects leave the live namespace as one
/// atomic move, so a backend that refuses it changes nothing at all — where the
/// per-object loop it replaced could destroy the first file and stop.
#[tokio::test]
async fn package_version_delete_storage_failure_leaves_every_file_published() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "pkg-prepare-fault";
    let repo = "registry";
    let (token, _) = register_full(&app.base, owner, "pkg-prepare-fault@example.com").await;
    create_repo(&app.base, &token, repo).await;
    publish(&app, &token, owner, repo, OWNER_FILE).await;
    publish(&app, &token, owner, repo, SECOND_FILE).await;

    app.blob_faults.fail_delete();
    let refused = delete_version(&app, &token, owner, repo).await;
    assert_eq!(
        refused.status(),
        500,
        "blob storage failure was reported as a completed version delete"
    );
    assert!(
        live_prefix(&app, owner, repo).exists(),
        "failed DELETE moved the live version prefix"
    );

    app.blob_faults.heal();
    assert_both_files_readable(&app, &token, owner, repo).await;
}

/// The version row is deleted after its file rows, in one transaction. A failure
/// on the second statement used to leave a published version with no files; now
/// both rows survive and the staged prefix comes back.
#[tokio::test]
async fn package_version_row_failure_restores_the_prefix_and_the_file_rows() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "pkg-version-fault";
    let repo = "registry";
    let (token, _) = register_full(&app.base, owner, "pkg-version-fault@example.com").await;
    create_repo(&app.base, &token, repo).await;
    publish(&app, &token, owner, repo, OWNER_FILE).await;
    publish(&app, &token, owner, repo, SECOND_FILE).await;

    let db_fault = fail_db_writes(&app.db, "package_versions", DbWrite::Delete).await;
    let response = delete_version(&app, &token, owner, repo).await;
    assert_eq!(response.status(), 500, "DB failure was reported as success");
    db_fault.clear().await;

    let version = version_files(&app, owner, repo, &token).await;
    assert_eq!(
        version["files"].as_array().map(Vec::len),
        Some(2),
        "the file rows were destroyed by a version delete that failed"
    );
    assert_both_files_readable(&app, &token, owner, repo).await;
}

/// The same guarantee from the other statement: a file-row failure must not
/// leave the version's bytes parked under a private prefix.
#[tokio::test]
async fn package_file_row_failure_restores_the_prefix() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "pkg-file-fault";
    let repo = "registry";
    let (token, _) = register_full(&app.base, owner, "pkg-file-fault@example.com").await;
    create_repo(&app.base, &token, repo).await;
    publish(&app, &token, owner, repo, OWNER_FILE).await;
    publish(&app, &token, owner, repo, SECOND_FILE).await;

    let db_fault = fail_db_writes(&app.db, "package_files", DbWrite::Delete).await;
    let response = delete_version(&app, &token, owner, repo).await;
    assert_eq!(response.status(), 500, "DB failure was reported as success");
    db_fault.clear().await;

    assert!(
        live_prefix(&app, owner, repo).exists(),
        "the staged version prefix was not put back"
    );
    assert_both_files_readable(&app, &token, owner, repo).await;
}

/// The healthy path owes nothing afterwards: no metadata, no live prefix, no
/// private tombstone.
#[tokio::test]
async fn package_version_delete_retires_metadata_prefix_and_tombstone() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "pkg-delete";
    let repo = "registry";
    let (token, _) = register_full(&app.base, owner, "pkg-delete@example.com").await;
    create_repo(&app.base, &token, repo).await;
    publish(&app, &token, owner, repo, OWNER_FILE).await;
    publish(&app, &token, owner, repo, SECOND_FILE).await;

    let deleted = delete_version(&app, &token, owner, repo).await;
    assert_eq!(deleted.status(), 204, "healthy version deletion failed");
    assert!(
        !live_prefix(&app, owner, repo).exists(),
        "successful DELETE left the live version prefix"
    );
    assert_eq!(
        download(&app, &token, owner, repo, OWNER_FILE)
            .await
            .status(),
        404,
        "a deleted version is still downloadable"
    );

    let tombstone_root = app.repo_root.join(format!(
        "_deleted/package-deletions/{owner}/{repo}/generic/sample/1.0.0"
    ));
    assert!(
        std::fs::read_dir(&tombstone_root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "DELETE returned success while a version tombstone remained"
    );
}

/// After the metadata commits the live prefix is already free, so there is
/// nothing to roll back — but bytes still parked under a private prefix are not
/// a completed deletion, and the response has to say so.
#[tokio::test]
async fn package_version_tombstone_cleanup_failure_is_not_reported_as_a_completed_delete() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "pkg-cleanup-fault";
    let repo = "registry";
    let (token, _) = register_full(&app.base, owner, "pkg-cleanup-fault@example.com").await;
    create_repo(&app.base, &token, repo).await;
    publish(&app, &token, owner, repo, OWNER_FILE).await;

    app.blob_faults.fail_delete_prefix();
    let response = delete_version(&app, &token, owner, repo).await;
    assert_eq!(
        response.status(),
        500,
        "post-commit cleanup failure was reported as a completed delete"
    );
    assert!(
        !live_prefix(&app, owner, repo).exists(),
        "failed retirement restored the live prefix"
    );
    assert_eq!(
        download(&app, &token, owner, repo, OWNER_FILE)
            .await
            .status(),
        404,
        "package metadata did not commit before retirement"
    );
    let tombstone_root = app.repo_root.join(format!(
        "_deleted/package-deletions/{owner}/{repo}/generic/sample/1.0.0"
    ));
    assert!(
        std::fs::read_dir(&tombstone_root)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false),
        "failed cleanup did not leave a discoverable private tombstone"
    );
}
