//! Repository deletion owns the blob namespaces whose keys are derived from
//! that repository. A successful DELETE must leave neither the live prefixes
//! nor private tombstones behind, and a storage failure must leave the active
//! repository intact rather than deleting a random subset of its objects.

use rg_core::blob_storage::BlobKey;

use crate::common::{
    create_repo, fault::spawn_test_app_for_fault_sweep, register_full, spawn_test_app_with_state,
};

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
