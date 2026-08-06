//! Repository deletion owns the blob namespaces whose keys are derived from
//! that repository. A successful DELETE must leave neither the live prefixes
//! nor private tombstones behind, and a storage failure must leave the active
//! repository intact rather than deleting a random subset of its objects.

use rg_core::blob_storage::BlobKey;

use crate::common::{
    create_repo,
    fault::{fail_db_writes, spawn_test_app_for_fault_sweep, DbWrite},
    register_full, spawn_test_app_with_state, TEST_ENCRYPTION_KEY,
};

/// The upstream a mirror points at. Never reached: every mirror assertion here
/// is about a pass that must not start.
const MIRROR_REMOTE: &str = "https://example.com/upstream.git";

fn representative_keys(owner: &str, repo: &str, repo_id: i64) -> Vec<BlobKey> {
    let repo_id = repo_id.to_string();
    vec![
        BlobKey::from_segments([
            "packages",
            owner,
            repo,
            "generic",
            "demo",
            "1.0.0",
            "objects",
            "one",
            "package.bin",
        ])
        .unwrap(),
        BlobKey::from_segments([
            "lfs",
            owner,
            repo,
            "aa",
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa.zst",
        ])
        .unwrap(),
        BlobKey::from_segments(["releases", owner, repo, "1", "1", "release.bin"]).unwrap(),
        BlobKey::from_segments(["attachments", repo_id.as_str(), "uuid", "attachment.bin"])
            .unwrap(),
    ]
}

fn live_prefixes(owner: &str, repo: &str, repo_id: i64) -> Vec<BlobKey> {
    let repo_id = repo_id.to_string();
    vec![
        BlobKey::from_segments(["packages", owner, repo]).unwrap(),
        BlobKey::from_segments(["lfs", owner, repo]).unwrap(),
        BlobKey::from_segments(["releases", owner, repo]).unwrap(),
        BlobKey::from_segments(["attachments", repo_id.as_str()]).unwrap(),
    ]
}

async fn seed_terminal_job(db: &rg_db::DatabaseConnection, repo_id: i64) -> i64 {
    let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
        db,
        repo_id,
        "1234567890123456789012345678901234567890",
        "refs/heads/main",
        "manual",
        None,
    )
    .await
    .expect("create historical pipeline");
    let stage = rg_db::ops::pipeline_ops::create_stage(db, pipeline.id, "test", 0)
        .await
        .expect("create historical stage");
    let job = rg_db::ops::pipeline_ops::create_job(
        db, stage.id, "unit", "echo ok", None, None, None, None, None, false, None, None, None,
    )
    .await
    .expect("create historical job");
    let finished_at = chrono::Utc::now().naive_utc();
    rg_db::ops::pipeline_ops::update_job_result(
        db,
        job.id,
        "success",
        Some(0),
        None,
        None,
        Some(finished_at),
    )
    .await
    .expect("settle historical job");
    rg_db::ops::pipeline_ops::update_stage_status(db, stage.id, "success", None, Some(finished_at))
        .await
        .expect("settle historical stage");
    rg_db::ops::pipeline_ops::update_pipeline_status(
        db,
        pipeline.id,
        "success",
        None,
        Some(finished_at),
    )
    .await
    .expect("settle historical pipeline");
    job.id
}

/// card_c01839965406: the routed DELETE covers the production Git path and all
/// four secondary managed namespaces named by the card. Recreating the name
/// starts with a new repository id and no inherited bytes.
#[tokio::test]
async fn delete_repository_retires_git_and_secondary_blob_namespaces() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "delete-blobs", "delete-blobs@example.com").await;
    let repo_id = create_repo(&base, &token, "recycled").await;
    let job_id = seed_terminal_job(&db, repo_id).await;
    let keys = representative_keys("delete-blobs", "recycled", repo_id);
    for (index, key) in keys.iter().enumerate() {
        state
            .blob_storage
            .put(key, format!("payload-{index}").as_bytes())
            .await
            .expect("seed representative repository blob");
    }

    let bare = state.repo_root.join("delete-blobs/recycled.git");
    let filesystem_directories = [
        state.repo_root.join("delete-blobs.lfs/recycled"),
        // Pre-migration release assets: still a live read fallback, and a
        // transfer already moves it, so a deletion has to take it too.
        state.repo_root.join("delete-blobs/recycled.releases"),
        state.repo_root.join("_ci_cache").join(repo_id.to_string()),
        state
            .repo_root
            .join("_artifacts/jobs")
            .join(job_id.to_string()),
    ];
    for (index, directory) in filesystem_directories.iter().enumerate() {
        std::fs::create_dir_all(directory).expect("create repository filesystem namespace");
        std::fs::write(
            directory.join(format!("marker-{index}")),
            format!("filesystem payload {index}"),
        )
        .expect("seed repository filesystem namespace");
    }
    assert!(
        bare.exists(),
        "repository creation did not seed Git storage"
    );
    let response = reqwest::Client::new()
        .delete(format!("{base}/api/v1/repos/delete-blobs/recycled"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete repository");
    assert_eq!(
        response.status(),
        200,
        "repository deletion failed: {}",
        response.text().await.unwrap_or_default()
    );

    assert!(!bare.exists(), "DELETE left the canonical Git tree behind");
    for directory in filesystem_directories {
        assert!(
            !directory.exists(),
            "DELETE left a repository filesystem namespace at {}",
            directory.display()
        );
        let file_name = directory
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        let tombstones: Vec<_> = std::fs::read_dir(directory.parent().unwrap())
            .expect("read filesystem namespace parent")
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| {
                name.to_string_lossy()
                    .starts_with(&format!("{file_name}.deleted-{repo_id}-"))
            })
            .collect();
        assert!(
            tombstones.is_empty(),
            "DELETE returned success while filesystem tombstones remained: {tombstones:?}"
        );
    }
    for prefix in live_prefixes("delete-blobs", "recycled", repo_id) {
        assert!(
            state
                .blob_storage
                .list(Some(&prefix))
                .await
                .expect("inventory live prefix")
                .is_empty(),
            "DELETE left live objects below {prefix}"
        );
    }
    let tombstones =
        BlobKey::from_segments(["_deleted", "repositories", repo_id.to_string().as_str()]).unwrap();
    assert!(
        state
            .blob_storage
            .list(Some(&tombstones))
            .await
            .expect("inventory deletion tombstones")
            .is_empty(),
        "DELETE returned success while staged repository blobs remained"
    );

    let recreated_id = create_repo(&base, &token, "recycled").await;
    assert_ne!(recreated_id, repo_id, "recreation reused the deleted row");
    for prefix in live_prefixes("delete-blobs", "recycled", recreated_id) {
        assert!(
            state
                .blob_storage
                .list(Some(&prefix))
                .await
                .expect("inventory recreated prefix")
                .is_empty(),
            "recreated repository inherited objects below {prefix}"
        );
    }
}

/// card_374998ffebc1: a mirrored repository owns a second Git tree — the full
/// clone of its upstream under `<repo_root>/<repo_id>.mirror` — and the deletion
/// used to answer `2xx` with that copy still on disk. Worse, it came back:
/// `mirrors` survives the repository's *soft* delete, so the scheduler kept
/// selecting the row, found no `HEAD`, and re-cloned the whole upstream into the
/// path the deletion had just cleared.
///
/// Three independent claims, one per half of the defect and one for the gap
/// between them:
///   * the deletion stages and retires the directory like every other thing the
///     repository owns;
///   * `list_due_sync` no longer offers the row to the sweep;
///   * `sync_mirror` refuses the row even when handed it directly — the sweep
///     picks up a batch and then spends a `git` subprocess per mirror, so a
///     repository deleted mid-pass is not a hypothetical window.
#[tokio::test]
async fn delete_repository_retires_the_mirror_clone_and_the_scheduler_leaves_it_gone() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "delete-mirror", "delete-mirror@example.com").await;
    let repo_id = create_repo(&base, &token, "mirrored").await;

    let client = reqwest::Client::new();
    let response = client
        .post(format!("{base}/api/v1/repos/delete-mirror/mirrored/mirror"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": MIRROR_REMOTE, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("configure the mirror");
    assert_eq!(response.status(), 201, "baseline mirror create");

    // Wind the schedule into the past so the row is genuinely due — a mirror an
    // hour out would leave every assertion below true for the wrong reason — and
    // plant a credential the server cannot read, so a pass that does start dies
    // at the decrypt step instead of resolving and dialing a remote.
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("read the mirror row")
        .expect("the mirror row");
    let mut model: rg_db::entities::mirror::ActiveModel = mirror.into();
    model.next_sync_at =
        sea_orm::ActiveValue::Set(Some(chrono::Utc::now() - chrono::Duration::seconds(60)));
    model.password_encrypted = sea_orm::ActiveValue::Set(Some("hunter2".to_string()));
    rg_db::ops::mirror_ops::update(&db, model)
        .await
        .expect("make the mirror due");
    assert_eq!(
        rg_db::ops::mirror_ops::list_due_sync(&db, 10)
            .await
            .expect("list due mirrors")
            .len(),
        1,
        "the fixture did not make the mirror due, so nothing below is being tested"
    );

    // What one completed sync leaves behind: a bare clone of the upstream.
    let clone = state.repo_root.join(format!("{repo_id}.mirror"));
    std::fs::create_dir_all(clone.join("objects")).expect("seed the mirror clone");
    std::fs::write(clone.join("HEAD"), b"ref: refs/heads/main\n").expect("seed the mirror HEAD");
    std::fs::write(clone.join("objects/pack-payload"), b"upstream bytes")
        .expect("seed the mirror payload");

    let response = client
        .delete(format!("{base}/api/v1/repos/delete-mirror/mirrored"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete the mirrored repository");
    assert_eq!(
        response.status(),
        200,
        "repository deletion failed: {}",
        response.text().await.unwrap_or_default()
    );

    assert!(
        !clone.exists(),
        "DELETE returned success with a full copy of the upstream still at {}",
        clone.display()
    );
    let tombstones: Vec<_> = std::fs::read_dir(&*state.repo_root)
        .expect("read the repository root")
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| {
            name.to_string_lossy()
                .starts_with(&format!("{repo_id}.mirror.deleted-{repo_id}-"))
        })
        .collect();
    assert!(
        tombstones.is_empty(),
        "DELETE returned success while the staged mirror clone remained: {tombstones:?}"
    );

    // The row outlived the soft-delete — that is the point. What must not
    // outlive it is its place in the sweep's work list.
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("read the mirror row")
        .expect("the mirror row survives its repository's soft-delete");
    assert!(
        rg_db::ops::mirror_ops::list_due_sync(&db, 10)
            .await
            .expect("list due mirrors")
            .is_empty(),
        "the scheduler still selects the mirror of a deleted repository"
    );

    assert!(
        !rg_core::mirror::service::sync_mirror(&db, &mirror, &state.repo_root, TEST_ENCRYPTION_KEY)
            .await
            .expect("sync the mirror of a deleted repository"),
        "a mirror handed directly to `sync_mirror` after its repository was deleted still ran a pass"
    );
    assert_eq!(
        rg_core::mirror::service::sync_due_mirrors(&db, &state.repo_root, 10, TEST_ENCRYPTION_KEY)
            .await
            .expect("run one scheduler pass"),
        0,
        "one scheduler interval after the deletion still ran a mirror pass"
    );
    assert!(
        !clone.exists(),
        "a scheduler pass re-cloned the upstream of a deleted repository into {}",
        clone.display()
    );

    // The refusal happens before the credential is decrypted and before the
    // SSRF guard, so an untouched `last_sync_at` is the evidence that nothing
    // reached out to the remote on behalf of a repository that no longer exists.
    let after = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("re-read the mirror row")
        .expect("the mirror row");
    assert!(
        after.last_sync_at.is_none(),
        "a sync pass ran for a deleted repository: {after:?}"
    );
}

/// The other side of the same directory: staged, but the deletion does not
/// commit. The mirror clone has to come back with everything else, because the
/// repository is still live and still mirroring.
#[tokio::test]
async fn a_failed_metadata_delete_restores_the_mirror_clone() {
    let app = spawn_test_app_for_fault_sweep().await;
    let (token, _) = register_full(
        &app.base,
        "delete-mirror-fault",
        "delete-mirror-fault@example.com",
    )
    .await;
    let repo_id = create_repo(&app.base, &token, "kept").await;

    let clone = app.repo_root.join(format!("{repo_id}.mirror"));
    std::fs::create_dir_all(&clone).expect("seed the mirror clone");
    std::fs::write(clone.join("HEAD"), b"ref: refs/heads/main\n").expect("seed the mirror HEAD");

    let db_fault = fail_db_writes(&app.db, "repositories", DbWrite::Update).await;
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/delete-mirror-fault/kept",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete repository with a failing metadata write");
    assert_eq!(
        response.status(),
        500,
        "a failed metadata delete was reported as success"
    );
    db_fault.clear().await;

    assert_eq!(
        std::fs::read(clone.join("HEAD")).expect("the staged mirror clone was not put back"),
        b"ref: refs/heads/main\n",
        "the restored mirror clone does not hold what it was staged with"
    );
    let tombstones: Vec<_> = std::fs::read_dir(&app.repo_root)
        .expect("read the repository root")
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| {
            name.to_string_lossy()
                .starts_with(&format!("{repo_id}.mirror.deleted-"))
        })
        .collect();
    assert!(
        tombstones.is_empty(),
        "the failed deletion left the mirror clone staged aside: {tombstones:?}"
    );
}

/// A prefix move is the prepare step. If the backend cannot perform it, DELETE
/// reports a server error and restores the already-staged Git tree before any
/// database mutation becomes visible.
#[tokio::test]
async fn blob_storage_failure_does_not_partially_delete_an_active_repository() {
    let app = spawn_test_app_for_fault_sweep().await;
    let (token, _) = register_full(
        &app.base,
        "delete-blob-fault",
        "delete-blob-fault@example.com",
    )
    .await;
    let repo_id = create_repo(&app.base, &token, "keep-me").await;
    let package = representative_keys("delete-blob-fault", "keep-me", repo_id)
        .into_iter()
        .next()
        .unwrap();
    let package_path = package
        .as_str()
        .split('/')
        .fold(app.repo_root.clone(), |path, segment| path.join(segment));
    std::fs::create_dir_all(package_path.parent().unwrap()).unwrap();
    std::fs::write(&package_path, b"keep this blob").unwrap();
    let bare = app.repo_root.join("delete-blob-fault/keep-me.git");

    app.blob_faults.fail_delete();
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/delete-blob-fault/keep-me",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete repository with failed blob storage");
    assert_eq!(
        response.status(),
        500,
        "storage failure was reported as success"
    );
    assert!(bare.exists(), "failed DELETE did not restore the Git tree");
    assert_eq!(std::fs::read(&package_path).unwrap(), b"keep this blob");

    app.blob_faults.heal();
    let visible = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/delete-blob-fault/keep-me",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read repository after failed deletion");
    assert_eq!(
        visible.status(),
        200,
        "failed DELETE hid the repository row"
    );
}

/// Once the row is soft-deleted, failure to retire the private tombstone cannot
/// be compensated safely. The canonical namespace is still free, but the
/// response must be a 5xx and the tombstone must name exactly what an operator
/// has to remove.
#[tokio::test]
async fn failed_tombstone_cleanup_is_not_reported_as_a_completed_delete() {
    let app = spawn_test_app_for_fault_sweep().await;
    let (token, _) = register_full(
        &app.base,
        "delete-cleanup-fault",
        "delete-cleanup-fault@example.com",
    )
    .await;
    let repo_id = create_repo(&app.base, &token, "retained").await;
    let package = representative_keys("delete-cleanup-fault", "retained", repo_id)
        .into_iter()
        .next()
        .unwrap();
    let package_path = package
        .as_str()
        .split('/')
        .fold(app.repo_root.clone(), |path, segment| path.join(segment));
    std::fs::create_dir_all(package_path.parent().unwrap()).unwrap();
    std::fs::write(&package_path, b"staged but not retired").unwrap();

    app.blob_faults.fail_delete_prefix();
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/delete-cleanup-fault/retained",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete repository with failed tombstone cleanup");
    assert_eq!(
        response.status(),
        500,
        "post-commit cleanup failure was reported as a completed delete"
    );
    assert!(
        !package_path.exists(),
        "failed retirement put the blob back into the live namespace"
    );
    let tombstone_root = app
        .repo_root
        .join("_deleted")
        .join("repositories")
        .join(repo_id.to_string());
    assert!(
        tombstone_root.exists(),
        "the failed cleanup did not leave a discoverable repository tombstone"
    );

    app.blob_faults.heal();
    let visible = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/delete-cleanup-fault/retained",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read repository after committed deletion");
    assert_eq!(
        visible.status(),
        404,
        "soft-delete did not commit before cleanup"
    );
}

/// card_ed203feab041: the historical `<owner>/<repo>.releases` directory is
/// repository-owned storage — `read_asset_bytes` falls back to it whenever the
/// blob store reports the key missing, and a transfer already moves it — so the
/// deletion has to stage it like every other namespace-bound directory. Staged
/// but uncommitted, it must come back: the row is still live and still serving
/// those assets.
#[tokio::test]
async fn a_failed_metadata_delete_restores_the_legacy_release_directory() {
    let app = spawn_test_app_for_fault_sweep().await;
    let (token, _) = register_full(
        &app.base,
        "delete-release-fault",
        "delete-release-fault@example.com",
    )
    .await;
    create_repo(&app.base, &token, "assets").await;

    let releases = app.repo_root.join("delete-release-fault/assets.releases");
    let asset = releases.join("assets/1/payload.bin");
    std::fs::create_dir_all(asset.parent().unwrap()).expect("seed legacy release assets");
    std::fs::write(&asset, b"legacy release asset").expect("write legacy release asset");

    let db_fault = fail_db_writes(&app.db, "repositories", DbWrite::Update).await;
    let response = reqwest::Client::new()
        .delete(format!(
            "{}/api/v1/repos/delete-release-fault/assets",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete repository with a failing metadata write");
    assert_eq!(
        response.status(),
        500,
        "a failed metadata delete was reported as success"
    );
    db_fault.clear().await;

    assert_eq!(
        std::fs::read(&asset).expect("the staged legacy release directory was not put back"),
        b"legacy release asset",
        "the restored directory does not hold the asset it was staged with"
    );
    let tombstones: Vec<_> = std::fs::read_dir(releases.parent().unwrap())
        .expect("read the repository namespace directory")
        .map(|entry| entry.unwrap().file_name())
        .filter(|name| name.to_string_lossy().contains(".releases.deleted-"))
        .collect();
    assert!(
        tombstones.is_empty(),
        "the failed deletion left the legacy release directory staged aside: {tombstones:?}"
    );

    let visible = reqwest::Client::new()
        .get(format!(
            "{}/api/v1/repos/delete-release-fault/assets",
            app.base
        ))
        .bearer_auth(&token)
        .send()
        .await
        .expect("read repository after failed deletion");
    assert_eq!(
        visible.status(),
        200,
        "failed DELETE hid the repository row"
    );
}
