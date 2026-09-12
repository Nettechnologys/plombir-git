//! card_2c447a670cb5: a repository transfer moves every namespace keyed by the
//! `<owner>/<repo>` pair — the bare Git directory, the `packages`/`lfs`/
//! `releases` prefixes, the historical directories and the whole OCI registry —
//! and only then rewrites the ownership row. Killed in between, it leaves the
//! row naming the old owner while every byte sits under the new one. Half the
//! storage families build their keys from that pair at read time, so the
//! repository stops finding its own LFS objects, packages and layers with a
//! perfectly intact database, and `<old>/<repo>.git` is not there for a clone
//! either.
//!
//! The deletion journal already knew how to finish an interrupted move; what it
//! did not know is that a move is not a deletion. Its marker means "the
//! metadata is gone, destroy the tombstone" for a delete and "the row names the
//! destination, leave the bytes there" for a transfer, and reading the second
//! as the first would destroy the storage of a live repository under its new
//! owner. That is the disposition these tests pin, from both sides.
//!
//! Nothing is killed for real: the state a kill leaves is a journal entry plus
//! renames, and that is what the fixtures reproduce.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use rg_core::blob_storage::{BlobKey, BlobMetadata, BlobStorage, LocalBlobStorage};
use rg_core::deletion_recovery::{self, RecoveryReport, StagedBytes};

use crate::common::{
    build_test_app_state_with, create_repo, register_full, setup_test_db,
    spawn_test_app_with_state, wait_for_listener, StateOverrides,
};

/// One object per namespace whose key is built from the `<owner>/<repo>` pair.
///
/// Asserting on the keys *is* asserting on the reads: `lfs_object_key`,
/// `PackageStorage::version_key` and their siblings spell exactly this pair at
/// read time, so an object that is not under the pair the row names is an
/// object no request can reach.
fn namespace_keys(owner: &str, repo: &str) -> Vec<BlobKey> {
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
    ]
}

fn transfer_prefixes(owner: &str, repo: &str) -> Vec<BlobKey> {
    ["packages", "lfs", "releases"]
        .into_iter()
        .map(|kind| BlobKey::from_segments([kind, owner, repo]).unwrap())
        .collect()
}

async fn seed_namespace_objects(storage: &dyn BlobStorage, owner: &str, repo: &str) {
    for (index, key) in namespace_keys(owner, repo).iter().enumerate() {
        storage
            .put(key, format!("payload-{index}").as_bytes())
            .await
            .expect("seed a namespace-keyed repository object");
    }
}

async fn objects_readable_at(storage: &dyn BlobStorage, owner: &str, repo: &str) -> bool {
    for key in namespace_keys(owner, repo) {
        if !storage.exists(&key).await.expect("read a namespace key") {
            return false;
        }
    }
    true
}

/// The journal entry a transfer writes before it moves anything: `live` is the
/// source the un-moved row still names, `staged` the destination.
fn transfer_journal(
    repo_root: &std::path::Path,
    owner: &str,
    destination: &str,
    repo: &str,
) -> Vec<StagedBytes> {
    let mut staged = vec![StagedBytes::path(
        &repo_root.join(format!("{owner}/{repo}.git")),
        &repo_root.join(format!("{destination}/{repo}.git")),
    )
    .unwrap()];
    staged.extend(
        transfer_prefixes(owner, repo)
            .iter()
            .zip(transfer_prefixes(destination, repo).iter())
            .map(|(source, destination)| StagedBytes::blob_prefix(source, destination)),
    );
    staged
}

async fn create_org(base: &str, token: &str, name: &str) {
    let status = reqwest::Client::new()
        .post(format!("{base}/api/v1/orgs"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "visibility": "public" }))
        .send()
        .await
        .expect("create org")
        .status();
    assert_eq!(status, 201, "baseline: the destination namespace exists");
}

async fn transfer(base: &str, token: &str, owner: &str, name: &str, new_owner: &str) -> u16 {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{name}/transfer"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "new_owner": new_owner }))
        .send()
        .await
        .expect("request")
        .status()
        .as_u16()
}

async fn repo_visible_at(base: &str, token: &str, owner: &str, name: &str) -> bool {
    reqwest::Client::new()
        .get(format!("{base}/api/v1/repos/{owner}/{name}"))
        .bearer_auth(token)
        .send()
        .await
        .expect("request")
        .status()
        .is_success()
}

/// Killed before `transfer_owner`: the row still names the source, so every
/// read still spells the source pair — and finds nothing, because the bytes are
/// already under the destination. The pass has to walk the whole move back.
#[tokio::test]
async fn a_transfer_killed_before_its_commit_puts_every_namespace_back() {
    let (base, _db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "tr-before", "tr-before@example.com").await;
    create_repo(&base, &token, "demo").await;
    create_org(&base, &token, "trbeforecorp").await;
    seed_namespace_objects(&*state.blob_storage, "tr-before", "demo").await;

    let source_git = state.repo_root.join("tr-before/demo.git");
    let destination_git = state.repo_root.join("trbeforecorp/demo.git");
    assert!(
        source_git.is_dir(),
        "baseline: the repository has a bare directory to move"
    );

    // Exactly what an interrupted transfer leaves: the entry it wrote before
    // the first rename, and the renames themselves. The ownership row is
    // untouched — that is the half the kill stopped.
    deletion_recovery::open_move(
        &*state.blob_storage,
        "0123456789abcdef0123456789abcdef",
        "repository transfer",
        transfer_journal(&state.repo_root, "tr-before", "trbeforecorp", "demo"),
    )
    .await
    .expect("open the transfer journal entry");
    std::fs::create_dir_all(destination_git.parent().unwrap()).unwrap();
    std::fs::rename(&source_git, &destination_git).expect("move the bare repository");
    for (source, destination) in transfer_prefixes("tr-before", "demo")
        .iter()
        .zip(transfer_prefixes("trbeforecorp", "demo").iter())
    {
        state
            .blob_storage
            .move_prefix(source, destination)
            .await
            .expect("move a repository blob prefix");
    }

    // The state the kill left, stated rather than assumed: the row is still
    // alice's and every one of her reads now points at nothing.
    assert!(
        repo_visible_at(&base, &token, "tr-before", "demo").await,
        "the fixture moved the ownership row it was supposed to leave behind"
    );
    assert!(
        !objects_readable_at(&*state.blob_storage, "tr-before", "demo").await,
        "the fixture did not actually move the repository's objects out of its namespace"
    );

    let report = deletion_recovery::recover_interrupted_storage_at(
        &state.db,
        &state.repo_root,
        Duration::ZERO,
    )
    .await;
    assert!(
        objects_readable_at(&*state.blob_storage, "tr-before", "demo").await,
        "the surviving row still cannot reach its packages, LFS objects or release assets"
    );
    assert!(
        source_git.is_dir(),
        "the surviving row still has no bare repository to clone from"
    );
    assert!(
        !destination_git.exists(),
        "the undone transfer left a copy under the destination that no row names"
    );
    assert_eq!(
        report,
        RecoveryReport {
            restored: 1,
            ..RecoveryReport::default()
        },
        "the startup pass did not undo the interrupted transfer"
    );
}

/// The other side of the same kill, and the one the disposition exists for: the
/// ownership row committed, so the bytes under the destination are the bytes a
/// live repository is being served from. Treating the marker the way a deletion
/// does would delete them.
#[tokio::test]
async fn a_transfer_killed_after_its_commit_keeps_the_bytes_the_new_row_names() {
    let (base, _db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "tr-after", "tr-after@example.com").await;
    create_repo(&base, &token, "demo").await;
    create_org(&base, &token, "trafteracorp").await;
    seed_namespace_objects(&*state.blob_storage, "tr-after", "demo").await;

    // A real transfer, so the bytes really are where production put them.
    assert_eq!(
        transfer(&base, &token, "tr-after", "demo", "trafteracorp").await,
        200,
        "baseline: the transfer this test recovers from has to succeed first"
    );
    assert!(
        objects_readable_at(&*state.blob_storage, "trafteracorp", "demo").await,
        "baseline: the transfer moved every namespace-keyed object"
    );

    // Now the kill: between the ownership commit and the entry being dropped,
    // both the entry and its marker are on disk and the next startup reads
    // them. Written here because a successful transfer clears its own.
    deletion_recovery::open_move(
        &*state.blob_storage,
        "fedcba9876543210fedcba9876543210",
        "repository transfer",
        transfer_journal(&state.repo_root, "tr-after", "trafteracorp", "demo"),
    )
    .await
    .expect("open the transfer journal entry");
    deletion_recovery::mark_committed(&*state.blob_storage, "fedcba9876543210fedcba9876543210")
        .await
        .expect("mark the transfer committed");

    let report = deletion_recovery::recover_interrupted_storage_at(
        &state.db,
        &state.repo_root,
        Duration::ZERO,
    )
    .await;
    // Consequences first: a report that says the right number about the wrong
    // bytes is not what this test is for.
    assert!(
        objects_readable_at(&*state.blob_storage, "trafteracorp", "demo").await,
        "the startup pass destroyed the storage of a repository its row still names"
    );
    assert!(
        state.repo_root.join("trafteracorp/demo.git").is_dir(),
        "the startup pass took the bare repository away from its committed owner"
    );
    assert!(
        repo_visible_at(&base, &token, "trafteracorp", "demo").await,
        "the transferred repository stopped answering after the startup pass"
    );
    assert_eq!(
        report,
        RecoveryReport {
            kept: 1,
            ..RecoveryReport::default()
        },
        "a committed transfer must be left alone — not undone, and above all not destroyed"
    );
}

/// The wiring, not the pass: a transfer that cannot record what it is about to
/// move must refuse *before* the first rename, exactly the way a deletion does.
/// Otherwise the entry the two tests above act on is one only a fixture ever
/// writes.
///
/// The fault is a file where the journal prefix has to be a directory — the one
/// way to make the local backend refuse a `put` without touching production
/// code.
#[tokio::test]
async fn a_transfer_that_cannot_be_recorded_moves_nothing() {
    let (base, _db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "tr-record", "tr-record@example.com").await;
    create_repo(&base, &token, "demo").await;
    create_org(&base, &token, "trrecordcorp").await;
    seed_namespace_objects(&*state.blob_storage, "tr-record", "demo").await;

    std::fs::create_dir_all(state.repo_root.join("_deleted")).unwrap();
    std::fs::remove_dir_all(state.repo_root.join("_deleted/journal"))
        .expect("retire the existing journal directory");
    std::fs::write(state.repo_root.join("_deleted/journal"), b"not a directory")
        .expect("block the journal prefix");

    let status = transfer(&base, &token, "tr-record", "demo", "trrecordcorp").await;
    assert_ne!(
        status, 200,
        "a transfer that could not record what it was about to move reported success"
    );

    assert!(
        objects_readable_at(&*state.blob_storage, "tr-record", "demo").await,
        "the refused transfer moved objects it had not recorded"
    );
    assert!(
        state.repo_root.join("tr-record/demo.git").is_dir(),
        "the refused transfer moved the bare repository it had not recorded"
    );
    assert!(
        !state.repo_root.join("trrecordcorp/demo.git").exists(),
        "the refused transfer left the bare repository under the destination"
    );
    assert!(
        repo_visible_at(&base, &token, "tr-record", "demo").await,
        "the refused transfer left the repository unreachable at its own owner"
    );
}

/// A transfer that finishes leaves nothing for the next startup to decide.
///
/// Without this, an entry surviving its own successful transfer would sit in
/// the journal until it aged past the bound and then be acted on — and which
/// way it went would depend on whether the marker write had landed too.
#[tokio::test]
async fn a_finished_transfer_leaves_no_journal_entry_behind() {
    let (base, _db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "tr-clean", "tr-clean@example.com").await;
    create_repo(&base, &token, "demo").await;
    create_org(&base, &token, "trcleancorp").await;
    seed_namespace_objects(&*state.blob_storage, "tr-clean", "demo").await;

    assert_eq!(
        transfer(&base, &token, "tr-clean", "demo", "trcleancorp").await,
        200,
        "baseline: the transfer succeeded"
    );

    for prefix in ["_deleted/journal", "_deleted/committed"] {
        let entries = state
            .blob_storage
            .list(Some(&BlobKey::new(prefix).unwrap()))
            .await
            .expect("read the journal prefix");
        assert!(
            entries.is_empty(),
            "a finished transfer left {} behind under {prefix}",
            entries.len()
        );
    }

    // And the pass agrees there is nothing to finish.
    let report = deletion_recovery::recover_interrupted_storage_at(
        &state.db,
        &state.repo_root,
        Duration::ZERO,
    )
    .await;
    assert_eq!(
        report,
        RecoveryReport::default(),
        "the startup pass found work in a transfer that had already finished"
    );
    assert!(
        repo_visible_at(&base, &token, "trcleancorp", "demo").await,
        "the startup pass moved a finished transfer's repository"
    );
}

/// A blob store that remembers, in order, every key written through it.
///
/// The journal entry and its commit marker are two writes, and which one lands
/// first is the whole protocol: an entry written after the first rename leaves
/// the window this module closes, and a marker written before the ownership
/// commit would authorize keeping bytes whose row never moved. Neither
/// ordering survives in the storage tree — a finished transfer clears both —
/// so the only way to pin it is to watch the writes go by.
struct RecordingBlobStorage {
    inner: LocalBlobStorage,
    writes: Arc<Mutex<Vec<String>>>,
}

impl RecordingBlobStorage {
    fn wrap(root: &std::path::Path) -> (Arc<dyn BlobStorage>, Arc<Mutex<Vec<String>>>) {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let storage = Arc::new(Self {
            inner: LocalBlobStorage::new(root.to_path_buf()),
            writes: Arc::clone(&writes),
        });
        (storage, writes)
    }

    fn record(&self, key: &BlobKey) {
        self.writes.lock().unwrap().push(key.as_str().to_string());
    }
}

impl BlobStorage for RecordingBlobStorage {
    fn backend_name(&self) -> &'static str {
        self.inner.backend_name()
    }

    fn put<'a>(
        &'a self,
        key: &'a BlobKey,
        data: &'a [u8],
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        self.record(key);
        self.inner.put(key, data)
    }

    fn put_file<'a>(
        &'a self,
        key: &'a BlobKey,
        source: &'a std::path::Path,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        self.record(key);
        self.inner.put_file(key, source)
    }

    fn get<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<Vec<u8>>> {
        self.inner.get(key)
    }

    fn metadata<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<BlobMetadata>> {
        self.inner.metadata(key)
    }

    fn exists<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<bool>> {
        self.inner.exists(key)
    }

    fn delete<'a>(
        &'a self,
        key: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<bool>> {
        self.inner.delete(key)
    }

    fn list<'a>(
        &'a self,
        prefix: Option<&'a BlobKey>,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<Vec<BlobMetadata>>> {
        self.inner.list(prefix)
    }

    // Both have to be delegated rather than inherited: the trait's default
    // `move_prefix` refuses ("backend does not support atomic prefix move"),
    // and a wrapper that inherits it turns every transfer into a `500`.
    fn move_prefix<'a>(
        &'a self,
        source: &'a BlobKey,
        destination: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<bool>> {
        self.inner.move_prefix(source, destination)
    }

    fn delete_prefix<'a>(
        &'a self,
        prefix: &'a BlobKey,
    ) -> BoxFuture<'a, rg_core::blob_storage::Result<bool>> {
        self.inner.delete_prefix(prefix)
    }

    fn local_path(&self, key: &BlobKey) -> Option<std::path::PathBuf> {
        self.inner.local_path(key)
    }
}

/// The protocol itself, watched as production performs it: a transfer declares
/// what it is about to move, and marks itself committed once the ownership row
/// has moved.
///
/// Both writes are erased by the transfer that made them, which is why the
/// tests above can only assert the state a kill leaves. Without this one, a
/// build that never wrote the marker would pass every other test here and still
/// hand a repository back to its previous owner on the next restart.
#[tokio::test]
async fn a_transfer_declares_itself_before_moving_and_marks_itself_after_committing() {
    // Assembled here rather than through a spawn helper because the recorder
    // has to wrap the store rooted at *this* server's repository root: a
    // journal filed anywhere else is a journal the rest of the transfer never
    // sees.
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let (storage, writes) = RecordingBlobStorage::wrap(&repo_root);
    let state = build_test_app_state_with(
        db,
        repo_root,
        StateOverrides {
            blob_storage: Some(storage),
            ..Default::default()
        },
    );
    let app = rg_http::create_router_for_test(state);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let base = format!("http://{addr}");
    tokio::spawn(async move {
        let _dir = dir;
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr.to_string()).await;

    let (token, _) = register_full(&base, "tr-order", "tr-order@example.com").await;
    create_repo(&base, &token, "demo").await;
    create_org(&base, &token, "trordercorp").await;

    writes.lock().unwrap().clear();
    assert_eq!(
        transfer(&base, &token, "tr-order", "demo", "trordercorp").await,
        200,
        "baseline: the transfer succeeded"
    );

    let writes = writes.lock().unwrap().clone();
    let entry = writes
        .iter()
        .position(|key| key.starts_with("_deleted/journal/"))
        .expect("the transfer never recorded what it was about to move");
    let marker = writes
        .iter()
        .position(|key| key.starts_with("_deleted/committed/"))
        .expect(
            "the transfer never marked itself committed — a restart between the ownership commit \
             and the journal being cleared would move the repository back to its previous owner",
        );
    assert!(
        entry < marker,
        "the commit marker was written before the journal entry it belongs to: {writes:?}"
    );
    assert_eq!(
        writes[entry].strip_prefix("_deleted/journal/"),
        writes[marker].strip_prefix("_deleted/committed/"),
        "the marker names a different operation than the entry"
    );
}
