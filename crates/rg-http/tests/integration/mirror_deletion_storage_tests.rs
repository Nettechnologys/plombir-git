//! card_8ee32201d626: `DELETE /mirror` retires the mirror's row and the clone
//! that row is metadata *for*.
//!
//! The same cross-store class as card_374998ffebc1, one level down. That card
//! was about deleting the *repository* leaving `<repo_root>/<repo_id>.mirror`
//! behind; this one is about deleting the *mirror*, which did exactly two
//! things — find the row and delete it — and never touched the directory.
//!
//! Two consequences, the second worse than the first:
//!
//!   * a full clone of a third-party upstream stayed on disk forever, under a
//!     name keyed by `repo_id` that no later namespace collides with and no
//!     sweep anywhere walks;
//!   * the *next* mirror configured on the same repository inherited it.
//!     `run_sync_pass` branches on whether `HEAD` exists, found the orphan, and
//!     ran `git remote update` — which takes its address from the inherited
//!     clone's own config. The new `mirrors.url` was never dialled, while the
//!     row reported `status=active` with a fresh `last_sync_at`, and the SSRF
//!     guard cleared a URL `git` never reached.

use crate::common::{create_repo, register_full, spawn_test_app_with_state};

const REMOTE: &str = "https://example.com/upstream.git";
const OTHER_REMOTE: &str = "https://example.com/somebody-else.git";

fn stale_before() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() - rg_db::ops::mirror_ops::SYNC_LEASE_STALE_AFTER
}

/// Configure a mirror on `owner/name` and seed the clone one completed pass
/// leaves behind, so "the deletion took the bytes" is a claim about real bytes.
async fn seed_mirror(
    base: &str,
    token: &str,
    repo_root: &std::path::Path,
    owner: &str,
    name: &str,
    repo_id: i64,
    url: &str,
) -> std::path::PathBuf {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/repos/{owner}/{name}/mirror"))
        .bearer_auth(token)
        .json(&serde_json::json!({"url": url, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("configure the mirror");
    assert_eq!(
        response.status(),
        201,
        "baseline mirror create: {}",
        response.text().await.unwrap_or_default()
    );

    let clone = repo_root.join(format!("{repo_id}.mirror"));
    std::fs::create_dir_all(clone.join("objects")).expect("seed the mirror clone");
    std::fs::write(clone.join("HEAD"), b"ref: refs/heads/main\n").expect("seed the mirror HEAD");
    std::fs::write(clone.join("objects/pack-payload"), b"upstream bytes")
        .expect("seed the mirror payload");
    clone
}

/// The staged tombstones a deletion of this repository's mirror would leave if
/// it removed the row and then failed to remove the bytes.
fn tombstones(repo_root: &std::path::Path, repo_id: i64) -> Vec<String> {
    std::fs::read_dir(repo_root)
        .expect("read the repository root")
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with(&format!("{repo_id}.mirror.deleted-{repo_id}-")))
        .collect()
}

/// The first half of the card: `204` has to mean the bytes are gone, not merely
/// that the row is.
#[tokio::test]
async fn deleting_a_mirror_retires_its_clone_and_leaves_no_tombstone() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "drop-mirror", "drop-mirror@example.com").await;
    let repo_id = create_repo(&base, &token, "mirrored").await;
    let clone = seed_mirror(
        &base,
        &token,
        &state.repo_root,
        "drop-mirror",
        "mirrored",
        repo_id,
        REMOTE,
    )
    .await;

    let response = reqwest::Client::new()
        .delete(format!("{base}/api/v1/repos/drop-mirror/mirrored/mirror"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete the mirror");
    assert_eq!(
        response.status(),
        204,
        "mirror deletion failed: {}",
        response.text().await.unwrap_or_default()
    );

    assert!(
        !clone.exists(),
        "DELETE returned success with a full copy of the upstream still at {}",
        clone.display()
    );
    assert!(
        tombstones(&state.repo_root, repo_id).is_empty(),
        "DELETE returned success while the staged clone remained: {:?}",
        tombstones(&state.repo_root, repo_id)
    );
    assert!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
            .await
            .expect("re-read the mirror row")
            .is_none(),
        "the row survived its own deletion"
    );
    // The repository itself is not what was deleted, and its Git tree must not
    // have been swept up with the mirror's.
    assert!(
        state
            .repo_root
            .join("drop-mirror/mirrored.git/HEAD")
            .exists(),
        "deleting the mirror retired the repository's own Git tree"
    );
}

/// The second half, and the one an operator would never diagnose: a mirror
/// re-created on a *different* URL used to inherit the previous mirror's clone
/// and keep fetching the previous mirror's upstream.
///
/// Asserted at the branch point rather than through a live remote: the pass
/// picks `git clone --mirror` over `git remote update` exactly when the clone
/// directory has no `HEAD`, so "the directory is gone after the delete" *is*
/// "the next pass clones the new upstream". The other half of the same defect —
/// a clone that outlives a URL change through `PATCH` — is pinned by
/// `rg_core::mirror::service`'s own
/// `a_refresh_points_the_clone_at_the_row_s_url_before_it_fetches`.
#[tokio::test]
async fn a_mirror_recreated_on_another_url_does_not_inherit_the_old_clone() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "repoint", "repoint@example.com").await;
    let repo_id = create_repo(&base, &token, "mirrored").await;
    let clone = seed_mirror(
        &base,
        &token,
        &state.repo_root,
        "repoint",
        "mirrored",
        repo_id,
        REMOTE,
    )
    .await;

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/repoint/mirrored/mirror");
    assert_eq!(
        client
            .delete(&url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("delete the first mirror")
            .status(),
        204
    );

    let response = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"url": OTHER_REMOTE, "sync_interval_seconds": 3600}))
        .send()
        .await
        .expect("configure a mirror of a different upstream");
    assert_eq!(
        response.status(),
        201,
        "re-creating the mirror failed: {}",
        response.text().await.unwrap_or_default()
    );

    assert!(
        !clone.join("HEAD").exists(),
        "the new mirror inherited the previous mirror's clone at {} — its first pass will run \
         `git remote update` against the *old* upstream and report success",
        clone.display()
    );
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("read the new mirror row")
        .expect("the new mirror row");
    assert_eq!(
        mirror.url, OTHER_REMOTE,
        "the row does not name the upstream the operator asked for"
    );
}

/// A pass owns that directory by absolute path for as long as its `git`
/// subprocess runs, so the deletion has to refuse — and refuse without moving
/// the bytes out from under it.
///
/// Three claims, matching the repository deletion's own contract:
///   * a held lease refuses with `409`, leaving row and bytes alone;
///   * the refusal is total, not merely early — the transaction that removes
///     the row rolls back rather than deleting a row a pass is still writing
///     storage for;
///   * a released lease stops refusing, and the deletion then owns the clone.
#[tokio::test]
async fn deleting_a_mirror_refuses_while_a_sync_is_in_flight() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "busy-mirror", "busy-mirror@example.com").await;
    let repo_id = create_repo(&base, &token, "mirrored").await;
    let clone = seed_mirror(
        &base,
        &token,
        &state.repo_root,
        "busy-mirror",
        "mirrored",
        repo_id,
        REMOTE,
    )
    .await;

    let holder = "pass-in-flight";
    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(&db, repo_id, holder, stale_before())
            .await
            .expect("take the mirror sync lease"),
        rg_db::ops::mirror_ops::SyncLeaseBid::Granted,
        "the fixture could not take the lease, so nothing below is being tested"
    );

    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/repos/busy-mirror/mirrored/mirror");
    let response = client
        .delete(&url)
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete a mirror whose pass is in flight");
    assert_eq!(
        response.status(),
        409,
        "the deletion did not refuse a sync in flight"
    );
    assert_eq!(
        std::fs::read(clone.join("objects/pack-payload"))
            .expect("the clone was staged aside by a refused deletion"),
        b"upstream bytes",
        "the refused deletion moved the directory its `git` subprocess is writing"
    );
    let mirror = rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
        .await
        .expect("re-read the mirror row")
        .expect("the refused deletion removed the row anyway");

    // The gate above runs before anything is staged, so on its own it only
    // narrows the window: a pass that takes the lease after the gate and before
    // the delete would still be running while the row went away.
    assert_eq!(
        rg_db::ops::mirror_ops::delete_by_id_unless_syncing(
            &db,
            mirror.id,
            repo_id,
            stale_before()
        )
        .await
        .expect("run the guarded delete under a held lease"),
        rg_db::ops::mirror_ops::MirrorRetirement::MirrorSyncInFlight,
        "the transaction that removes the row cannot see the lease its own gate reads"
    );
    assert!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
            .await
            .expect("re-read the mirror row after the guarded delete")
            .is_some(),
        "the guarded delete refused and removed the row anyway"
    );

    // The pass finished. Nothing is writing that directory any more.
    assert!(
        rg_db::ops::mirror_ops::release_sync_lease(&db, repo_id, holder)
            .await
            .expect("release the mirror sync lease"),
        "the holder could not release its own lease"
    );
    assert_eq!(
        client
            .delete(&url)
            .bearer_auth(&token)
            .send()
            .await
            .expect("delete a mirror whose pass has finished")
            .status(),
        204,
        "the gate kept refusing after the pass had finished"
    );
    assert!(
        !clone.exists(),
        "DELETE returned success with a full copy of the upstream still at {}",
        clone.display()
    );
}

/// The other order of the same race, which the gate alone cannot decide: the
/// deletion wins, and a pass admitted a moment later would put the whole
/// upstream back into the directory that deletion just retired.
///
/// `bid_for_sync_lease` is where that is settled, so it is asserted there —
/// and the released lease proves a refused bid does not leave the mirror
/// blocked behind a lease nothing holds.
#[tokio::test]
async fn a_pass_bidding_after_the_mirror_is_deleted_is_refused() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "gone-mirror", "gone-mirror@example.com").await;
    let repo_id = create_repo(&base, &token, "mirrored").await;
    let clone = seed_mirror(
        &base,
        &token,
        &state.repo_root,
        "gone-mirror",
        "mirrored",
        repo_id,
        REMOTE,
    )
    .await;

    assert_eq!(
        reqwest::Client::new()
            .delete(format!("{base}/api/v1/repos/gone-mirror/mirrored/mirror"))
            .bearer_auth(&token)
            .send()
            .await
            .expect("delete the mirror")
            .status(),
        204
    );

    assert_eq!(
        rg_db::ops::mirror_ops::bid_for_sync_lease(&db, repo_id, "late-pass", stale_before())
            .await
            .expect("bid for the lease of a deleted mirror"),
        rg_db::ops::mirror_ops::SyncLeaseBid::MirrorGone,
        "a pass was admitted to a mirror that no longer exists — it would clone the upstream \
         straight back into the directory the deletion retired"
    );
    assert!(
        rg_db::ops::mirror_ops::sync_lease_in_flight(&db, repo_id, stale_before())
            .await
            .expect("read the lease after a refused bid")
            .is_none(),
        "a refused bid left its lease behind, blocking the repository's own deletion"
    );
    assert!(
        !clone.exists(),
        "the refused bid re-created the clone at {}",
        clone.display()
    );
}

/// A deletion that removed the row but could not remove the bytes is not a
/// deletion, and must not answer `204`. The row is gone by then — that half is
/// committed and correct — so what the error buys is an operator who knows
/// there is a tombstone to clear rather than one who never finds out.
#[cfg(unix)]
#[tokio::test]
async fn a_clone_that_cannot_be_removed_is_not_reported_as_a_deletion() {
    use std::os::unix::fs::PermissionsExt;

    let (base, db, state) = spawn_test_app_with_state().await;
    let (token, _) = register_full(&base, "stuck-mirror", "stuck-mirror@example.com").await;
    let repo_id = create_repo(&base, &token, "mirrored").await;
    let clone = seed_mirror(
        &base,
        &token,
        &state.repo_root,
        "stuck-mirror",
        "mirrored",
        repo_id,
        REMOTE,
    )
    .await;

    // `remove_dir_all` has to descend into `objects` to unlink what is in it,
    // and a directory with no write bit refuses that. The rename that stages the
    // clone aside is unaffected — it touches the parent, not this — so the
    // deletion gets all the way past the row and fails where it removes bytes.
    let locked = clone.join("objects");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500))
        .expect("make the clone's payload directory unremovable");

    let response = reqwest::Client::new()
        .delete(format!("{base}/api/v1/repos/stuck-mirror/mirrored/mirror"))
        .bearer_auth(&token)
        .send()
        .await
        .expect("delete a mirror whose clone cannot be removed");
    let status = response.status();

    // Put the permissions back before asserting, so a failing assertion cannot
    // leave the test's tempdir undeletable. The directory moved: staging renamed
    // the clone aside, so the locked leaf is under the tombstone now.
    let left_behind = tombstones(&state.repo_root, repo_id);
    for tombstone in &left_behind {
        std::fs::set_permissions(
            state.repo_root.join(tombstone).join("objects"),
            std::fs::Permissions::from_mode(0o700),
        )
        .expect("restore the permissions");
    }
    // …and on the live path too, for the one case that leaves it there: a
    // deletion that never staged anything. That is a failing run, but it must
    // not also leave the tempdir behind undeletable.
    if locked.exists() {
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700))
            .expect("restore the permissions of an unstaged clone");
    }

    assert_eq!(
        status, 500,
        "a deletion that left a full copy of the upstream on disk reported success"
    );
    assert!(
        rg_db::ops::mirror_ops::find_by_repo_id(&db, repo_id)
            .await
            .expect("re-read the mirror row")
            .is_none(),
        "the row is the half that committed; reporting the failure must not un-delete it"
    );
    assert!(
        !clone.exists(),
        "the live path must be free even when the tombstone could not be removed"
    );
    assert!(
        !left_behind.is_empty(),
        "the failure was reported without there being anything left to clean up, so this test is \
         no longer exercising the path it names"
    );
}
