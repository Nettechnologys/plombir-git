//! Release deletion spans metadata, backend-neutral blobs and a legacy local
//! layout. A `204` is truthful only after every live representation is gone;
//! a prepare/DB failure must leave the release and its assets readable.

use serde_json::Value;

use crate::common::{
    create_repo,
    fault::{fail_db_writes, spawn_test_app_for_fault_sweep, DbWrite},
    register_full,
};

async fn create_release(base: &str, token: &str, owner: &str, repo: &str) -> i64 {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{repo}/releases"))
        .bearer_auth(token)
        .json(&serde_json::json!({
            "tag_name": "v1.0.0",
            "title": "release deletion"
        }))
        .send()
        .await
        .expect("create release");
    assert_eq!(response.status(), 201, "create release failed");
    response.json::<Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn upload_asset(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    release_id: i64,
    filename: &str,
    payload: &[u8],
) -> i64 {
    let response = reqwest::Client::new()
        .post(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/{release_id}/assets"
        ))
        .bearer_auth(token)
        .header("x-asset-filename", filename)
        .body(payload.to_vec())
        .send()
        .await
        .expect("upload release asset");
    assert_eq!(response.status(), 201, "upload release asset failed");
    response.json::<Value>().await.unwrap()["id"]
        .as_i64()
        .unwrap()
}

async fn download_asset(
    base: &str,
    token: &str,
    owner: &str,
    repo: &str,
    asset_id: i64,
) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!(
            "{base}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}/download"
        ))
        .bearer_auth(token)
        .send()
        .await
        .expect("download release asset")
}

/// card_ce2328eac8a0: a backend refusal is a failed DELETE, not a warning on the
/// way to `204`. Once the backend heals, the same request removes the portable
/// blob, the historical duplicate and the metadata row.
#[tokio::test]
async fn asset_delete_failure_preserves_metadata_and_a_retry_removes_every_representation() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "asset-delete";
    let repo = "releases";
    let (token, _) = register_full(&app.base, owner, "asset-delete@example.com").await;
    create_repo(&app.base, &token, repo).await;
    let release_id = create_release(&app.base, &token, owner, repo).await;
    let asset_id = upload_asset(
        &app.base,
        &token,
        owner,
        repo,
        release_id,
        "payload.bin",
        b"portable release asset",
    )
    .await;
    let portable = app.repo_root.join(format!(
        "releases/{owner}/{repo}/{release_id}/{asset_id}/payload.bin"
    ));
    assert!(portable.exists(), "upload did not write the portable blob");

    let legacy = app.repo_root.join(format!(
        "{owner}/{repo}.releases/assets/{asset_id}/payload.bin"
    ));
    std::fs::create_dir_all(legacy.parent().unwrap()).unwrap();
    std::fs::write(&legacy, b"legacy duplicate").unwrap();

    app.blob_faults.fail_delete();
    let refused = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete asset with failed storage");
    assert_eq!(
        refused.status(),
        500,
        "blob storage failure was reported as a completed asset delete"
    );
    assert!(portable.exists(), "failed DELETE moved the portable blob");
    assert!(legacy.exists(), "failed DELETE moved the legacy asset");

    app.blob_faults.heal();
    let still_readable = download_asset(&app.base, &token, owner, repo, asset_id).await;
    assert_eq!(still_readable.status(), 200, "asset metadata was deleted");
    assert_eq!(
        still_readable.bytes().await.unwrap().as_ref(),
        b"portable release asset"
    );

    let deleted = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("retry asset deletion");
    assert_eq!(deleted.status(), 204, "healthy asset deletion failed");
    assert!(
        !portable.exists(),
        "successful DELETE left the portable blob"
    );
    assert!(!legacy.exists(), "successful DELETE left the legacy blob");
    assert!(
        rg_db::ops::release_ops::find_asset_by_id(&app.db, asset_id)
            .await
            .unwrap()
            .is_none(),
        "successful DELETE left the asset row"
    );
}

/// The blob prefix is only staged before the DB write. If that write fails, the
/// compensation moves the exact prefix back and the surviving row remains usable.
#[tokio::test]
async fn asset_metadata_failure_restores_the_staged_blob() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "asset-db-fault";
    let repo = "releases";
    let (token, _) = register_full(&app.base, owner, "asset-db-fault@example.com").await;
    create_repo(&app.base, &token, repo).await;
    let release_id = create_release(&app.base, &token, owner, repo).await;
    let asset_id = upload_asset(
        &app.base,
        &token,
        owner,
        repo,
        release_id,
        "recover.bin",
        b"restore me",
    )
    .await;

    let db_fault = fail_db_writes(&app.db, "release_assets", DbWrite::Delete).await;
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete asset with failed DB write");
    assert_eq!(response.status(), 500, "DB failure was reported as success");
    db_fault.clear().await;

    let restored = download_asset(&app.base, &token, owner, repo, asset_id).await;
    assert_eq!(restored.status(), 200, "the surviving row lost its blob");
    assert_eq!(restored.bytes().await.unwrap().as_ref(), b"restore me");
}

/// After the row commits there is no safe rollback: the live name is already
/// free, but a retained private tombstone still makes `204` a lie. The response
/// and the on-disk namespace must make that cleanup debt explicit.
#[tokio::test]
async fn asset_tombstone_cleanup_failure_is_not_reported_as_a_completed_delete() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "asset-cleanup-fault";
    let repo = "releases";
    let (token, _) = register_full(&app.base, owner, "asset-cleanup-fault@example.com").await;
    create_repo(&app.base, &token, repo).await;
    let release_id = create_release(&app.base, &token, owner, repo).await;
    let asset_id = upload_asset(
        &app.base,
        &token,
        owner,
        repo,
        release_id,
        "retained.bin",
        b"private tombstone",
    )
    .await;
    let live = app.repo_root.join(format!(
        "releases/{owner}/{repo}/{release_id}/{asset_id}/retained.bin"
    ));

    app.blob_faults.fail_delete_prefix();
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/releases/assets/{asset_id}",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete asset with failed tombstone cleanup");
    assert_eq!(
        response.status(),
        500,
        "post-commit cleanup failure was reported as a completed delete"
    );
    assert!(!live.exists(), "failed retirement restored the live blob");
    assert!(
        rg_db::ops::release_ops::find_asset_by_id(&app.db, asset_id)
            .await
            .unwrap()
            .is_none(),
        "asset metadata did not commit before retirement"
    );
    let tombstone_root = app
        .repo_root
        .join(format!("_deleted/release-deletions/asset/{asset_id}"));
    assert!(
        std::fs::read_dir(&tombstone_root)
            .map(|mut entries| entries.next().is_some())
            .unwrap_or(false),
        "failed cleanup did not leave a discoverable private tombstone"
    );
}

/// Parent deletion owns the whole release prefix. One atomic prepare covers all
/// assets, and the FK cascade removes their rows only after that prepare succeeds.
#[tokio::test]
async fn release_delete_retires_every_asset_row_and_blob() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "release-delete";
    let repo = "releases";
    let (token, _) = register_full(&app.base, owner, "release-delete@example.com").await;
    create_repo(&app.base, &token, repo).await;
    let release_id = create_release(&app.base, &token, owner, repo).await;
    let first = upload_asset(
        &app.base, &token, owner, repo, release_id, "one.bin", b"one",
    )
    .await;
    let second = upload_asset(
        &app.base, &token, owner, repo, release_id, "two.bin", b"two",
    )
    .await;

    let deleted = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/releases/{release_id}",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete release");
    assert_eq!(deleted.status(), 204, "release deletion failed");
    assert!(
        rg_db::ops::release_ops::find_by_id(&app.db, release_id)
            .await
            .unwrap()
            .is_none(),
        "release row survived DELETE"
    );
    for asset_id in [first, second] {
        assert!(
            rg_db::ops::release_ops::find_asset_by_id(&app.db, asset_id)
                .await
                .unwrap()
                .is_none(),
            "asset row {asset_id} survived the release cascade"
        );
    }
    assert!(
        !app.repo_root
            .join(format!("releases/{owner}/{repo}/{release_id}"))
            .exists(),
        "release blob prefix survived DELETE"
    );
    let tombstone_root = app
        .repo_root
        .join(format!("_deleted/release-deletions/release/{release_id}"));
    assert!(
        std::fs::read_dir(&tombstone_root)
            .map(|mut entries| entries.next().is_none())
            .unwrap_or(true),
        "DELETE returned success while a release tombstone remained"
    );
}

/// A failed parent-row delete restores the whole prefix, so every child row and
/// every child blob remains readable instead of only the first one.
#[tokio::test]
async fn release_metadata_failure_restores_all_asset_blobs() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "release-db-fault";
    let repo = "releases";
    let (token, _) = register_full(&app.base, owner, "release-db-fault@example.com").await;
    create_repo(&app.base, &token, repo).await;
    let release_id = create_release(&app.base, &token, owner, repo).await;
    let first = upload_asset(
        &app.base, &token, owner, repo, release_id, "one.bin", b"one",
    )
    .await;
    let second = upload_asset(
        &app.base, &token, owner, repo, release_id, "two.bin", b"two",
    )
    .await;

    let db_fault = fail_db_writes(&app.db, "releases", DbWrite::Delete).await;
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/{owner}/{repo}/releases/{release_id}",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete release with failed DB write");
    assert_eq!(response.status(), 500, "DB failure was reported as success");
    db_fault.clear().await;

    for (asset_id, expected) in [(first, b"one".as_slice()), (second, b"two".as_slice())] {
        let restored = download_asset(&app.base, &token, owner, repo, asset_id).await;
        assert_eq!(restored.status(), 200, "asset {asset_id} was not restored");
        assert_eq!(restored.bytes().await.unwrap().as_ref(), expected);
    }
}
