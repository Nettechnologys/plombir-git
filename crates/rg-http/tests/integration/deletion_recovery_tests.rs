//! A deletion that moves live bytes aside is only reversible by a process that
//! lives long enough to reverse it. `SIGKILL`, the OOM killer and a container
//! restart run no compensation, and what they leave behind is not a scratch
//! file — it is a live row whose bytes are one name away, or bytes no row names
//! at all.
//!
//! These tests fake exactly that state: the journal entry and the rename a
//! deletion had already performed, and either the metadata still live (the
//! process died before its commit) or already gone (it died after). Nothing is
//! killed for real — the point is to pin what the startup pass does with what a
//! kill leaves, and the state a kill leaves is a rename plus a journal entry.
//!
//! See `rg_core::deletion_recovery` for why the marker rather than the database
//! is what tells those two apart.

use std::time::Duration;

use rg_core::blob_storage::{BlobKey, BlobStorage, LocalBlobStorage};
use rg_core::deletion_recovery::{self, RecoveryReport, StagedBytes};

use crate::artifact_deletion_storage_tests::{download, upload_artifact};
use crate::common::{create_repo, fault::spawn_test_app_for_fault_sweep, register_full};

/// Where an artifact deletion parks the object it is about to destroy.
fn staged_key(artifact_id: i64, deletion_id: &str) -> BlobKey {
    BlobKey::from_segments([
        "_deleted",
        "artifact-deletions",
        artifact_id.to_string().as_str(),
        deletion_id,
    ])
    .expect("staged artifact key")
}

/// Reproduce, byte for byte, the state an interrupted deletion leaves on disk:
/// the journal entry it wrote before it moved anything, and the move itself.
async fn stage_as_an_interrupted_deletion(
    storage: &LocalBlobStorage,
    live: &BlobKey,
    staged: &BlobKey,
    deletion_id: &str,
) {
    deletion_recovery::open(
        storage,
        deletion_id,
        "CI artifact",
        vec![StagedBytes::blob_prefix(live, staged)],
    )
    .await
    .expect("open the deletion journal entry");
    assert!(
        storage
            .move_prefix(live, staged)
            .await
            .expect("stage the artifact blob"),
        "the artifact had no live object to stage"
    );
}

/// card_11fd011939a8: killed between the rename and the metadata commit, the row
/// is still live and still points at the live key — while its bytes sit under a
/// private one. The startup pass has to put them back, or every download of a
/// perfectly valid artifact answers for a file that is physically there.
#[tokio::test]
async fn a_deletion_killed_before_its_commit_leaves_the_row_and_its_bytes_consistent() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "deletion-recovery-before";
    let repo = "ci";
    let (token, _) = register_full(&app.base, owner, "deletion-recovery-before@example.com").await;
    let repo_id = create_repo(&app.base, &token, repo).await;
    let (artifact_id, _) = upload_artifact(&app, repo_id, "survives.txt").await;

    let stored = rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
        .await
        .unwrap()
        .expect("artifact row");
    let live = BlobKey::new(stored.file_path.clone()).expect("artifact blob key");
    let deletion_id = "0123456789abcdef0123456789abcdef";
    let staged = staged_key(artifact_id, deletion_id);
    let storage = LocalBlobStorage::new(app.repo_root.clone());

    stage_as_an_interrupted_deletion(&storage, &live, &staged, deletion_id).await;

    // The state the kill left: the row is untouched and answers for bytes that
    // are no longer where it says they are.
    assert!(
        rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
            .await
            .unwrap()
            .is_some(),
        "the fixture deleted the metadata it was supposed to leave alive"
    );
    let broken = download(&app, &token, artifact_id).await;
    assert_ne!(
        broken.status(),
        200,
        "the fixture did not actually move the artifact's bytes out of the live namespace"
    );

    let report =
        deletion_recovery::recover_interrupted_storage_at(&app.db, &app.repo_root, Duration::ZERO)
            .await;
    assert_eq!(
        report,
        RecoveryReport {
            restored: 1,
            ..RecoveryReport::default()
        },
        "the startup pass did not restore the interrupted deletion"
    );

    let repaired = download(&app, &token, artifact_id).await;
    assert_eq!(
        repaired.status(),
        200,
        "the surviving row still points at missing bytes after the startup pass"
    );
    assert_eq!(
        repaired.bytes().await.unwrap().as_ref(),
        b"bytes of survives.txt",
        "the restored object is not the one the row named"
    );
}

/// The other side of the same kill: the metadata commit already happened, so no
/// row names these bytes. Putting them back would leak them into a live
/// namespace forever; the pass has to finish the deletion instead.
#[tokio::test]
async fn a_deletion_killed_after_its_commit_leaves_no_bytes_behind() {
    let app = spawn_test_app_for_fault_sweep().await;
    let owner = "deletion-recovery-after";
    let repo = "ci";
    let (token, _) = register_full(&app.base, owner, "deletion-recovery-after@example.com").await;
    let repo_id = create_repo(&app.base, &token, repo).await;
    let (artifact_id, _) = upload_artifact(&app, repo_id, "goes-away.txt").await;

    let stored = rg_db::ops::artifact_ops::get_by_id(&app.db, artifact_id)
        .await
        .unwrap()
        .expect("artifact row");
    let live = BlobKey::new(stored.file_path.clone()).expect("artifact blob key");
    let deletion_id = "fedcba9876543210fedcba9876543210";
    let staged = staged_key(artifact_id, deletion_id);
    let storage = LocalBlobStorage::new(app.repo_root.clone());

    stage_as_an_interrupted_deletion(&storage, &live, &staged, deletion_id).await;
    // Past the commit, and past the marker that records it — the kill lands in
    // the window between the marker and the unlink.
    deletion_recovery::mark_committed(&storage, deletion_id)
        .await
        .expect("mark the deletion committed");
    assert!(
        rg_db::ops::artifact_ops::delete_by_id(&app.db, artifact_id)
            .await
            .expect("delete the artifact row"),
        "the fixture did not delete the metadata it was supposed to commit"
    );

    let report =
        deletion_recovery::recover_interrupted_storage_at(&app.db, &app.repo_root, Duration::ZERO)
            .await;
    assert_eq!(
        report,
        RecoveryReport {
            destroyed: 1,
            ..RecoveryReport::default()
        },
        "the startup pass did not finish the committed deletion"
    );

    assert!(
        !storage
            .exists(&BlobKey::new(stored.file_path.clone()).unwrap())
            .await
            .unwrap(),
        "bytes whose row is gone were restored into the live namespace"
    );
    assert!(
        storage
            .list(Some(&staged))
            .await
            .expect("list the staged prefix")
            .is_empty(),
        "the tombstone of a committed deletion was left on the volume"
    );
}

/// The recovery entry is not test-fixture-only plumbing: both production doors
/// that can materialise a new `<owner>/<name>.git` path must refuse before they
/// touch that path when the journal cannot be written. Removing either open
/// call makes the corresponding request succeed and this test fail.
#[tokio::test]
async fn create_and_fork_refuse_to_claim_a_path_they_cannot_record() {
    let app = spawn_test_app_for_fault_sweep().await;
    let (source_token, _) =
        register_full(&app.base, "creation-source", "creation-source@example.com").await;
    create_repo(&app.base, &source_token, "source").await;
    let (creator_token, creator_id) =
        register_full(&app.base, "creation-target", "creation-target@example.com").await;

    let deleted = app.repo_root.join("_deleted");
    std::fs::create_dir_all(&deleted).expect("create recovery root");
    std::fs::remove_dir_all(deleted.join("journal"))
        .expect("retire the existing journal directory");
    std::fs::write(deleted.join("journal"), b"not a directory").expect("block the journal prefix");

    let client = reqwest::Client::new();
    let created = client
        .post(format!("{}/api/v1/repos", app.base))
        .bearer_auth(&creator_token)
        .json(&serde_json::json!({"name": "blocked-create"}))
        .send()
        .await
        .expect("request repository creation");
    assert!(
        !created.status().is_success(),
        "repository creation succeeded without recording its final path"
    );
    assert!(
        !app.repo_root
            .join("creation-target/blocked-create.git")
            .exists(),
        "repository creation touched the final path before opening its journal"
    );
    assert!(
        rg_db::ops::repo_ops::find_personal_by_owner_and_name(
            &app.db,
            creator_id,
            "blocked-create"
        )
        .await
        .unwrap()
        .is_none(),
        "repository creation wrote metadata after its journal failed"
    );

    let forked = client
        .post(format!(
            "{}/api/v1/repos/creation-source/source/fork",
            app.base
        ))
        .bearer_auth(&creator_token)
        .send()
        .await
        .expect("request repository fork");
    assert!(
        !forked.status().is_success(),
        "repository fork succeeded without recording its final path"
    );
    assert!(
        !app.repo_root.join("creation-target/source.git").exists(),
        "repository fork touched the final path before opening its journal"
    );
    assert!(
        rg_db::ops::repo_ops::find_personal_by_owner_and_name(&app.db, creator_id, "source")
            .await
            .unwrap()
            .is_none(),
        "repository fork wrote metadata after its journal failed"
    );
}
