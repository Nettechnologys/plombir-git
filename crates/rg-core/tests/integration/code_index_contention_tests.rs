//! card_0b936c68e1ea: a code-index refresh must outlast the writer ahead of it.
//!
//! The refresh transaction reads (`lock_repository_for_refresh`) before it
//! writes, and on SQLite a connection that already holds a read inside its
//! transaction cannot be put to sleep while it upgrades to a write — the
//! refusal arrives immediately, without `busy_timeout` being consulted. A
//! budget of thirty-two attempts is therefore thirty-two instant refusals plus
//! their jittered waits: about a third of a second, however long the writer
//! ahead actually needs.
//!
//! The writer ahead is very often another refresh, which holds SQLite's
//! database-wide write lock for as long as it takes to insert a whole
//! repository snapshot. Under the old budget that made the loser fail *by
//! construction* rather than occasionally — measured at five failures out of
//! five against a three-second holder — and its three consumers turned that
//! into a silently stale index, a `5xx`, and a failed operator command.
//!
//! So the assertion here is not "a refresh survives contention" in the abstract.
//! It is: a holder that keeps the database for longer than the old budget could
//! ever have waited does not cost the refresh its snapshot.

use std::path::Path;
use std::time::Duration;

use rg_core::search::code_indexer::CodeIndexer;
use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::common::git;

/// How long the competing writer keeps SQLite's write lock.
///
/// Comfortably past the ~0.34 s the thirty-two-attempt budget could reach, and
/// the same three seconds the original probe used, so this test fails against
/// the old shape for the reason the card measured rather than by being tuned to
/// the edge of it. Comfortably under `BULK_WRITE_BUDGET`, so a loaded machine
/// cannot make the new shape look broken either.
const HOLD: Duration = Duration::from_secs(3);

async fn fresh_db(dir: &Path) -> sea_orm::DatabaseConnection {
    // Four connections: the holder parks one for the whole test, and the
    // refresh needs its own alongside the fixture's reads.
    crate::common::migrated_sqlite(&dir.join("test.db"), 4).await
}

/// A repository with one indexable file on `main`, already carrying a snapshot.
async fn indexed_repository(
    db: &sea_orm::DatabaseConnection,
    repo_root: &Path,
) -> (i64, std::path::PathBuf) {
    let owner = rg_db::ops::user_ops::create_user(
        db,
        "contentionowner",
        "contentionowner@example.invalid",
        "",
        "Contention Owner",
    )
    .await
    .expect("create the account the repository hangs off");

    let repo = rg_core::repo::service::create_repo(
        db,
        owner.id,
        "contentionrepo",
        None,
        false,
        repo_root,
        None,
    )
    .await
    .expect("create repo");

    let bare_path = repo_root.join("contentionowner/contentionrepo.git");
    let worktree = tempfile::tempdir().expect("create fixture worktree");
    let path = worktree.path();
    let path_arg = path.to_str().expect("UTF-8 worktree path");
    let bare_arg = bare_path.to_str().expect("UTF-8 bare repository path");

    git(&["init", "-q", "-b", "main", path_arg], None);
    git(&["config", "user.name", "Contention test"], Some(path));
    git(
        &["config", "user.email", "contention@example.invalid"],
        Some(path),
    );
    std::fs::write(path.join("indexed.rs"), "fn zzcontention() {}\n").expect("write the file");
    git(&["add", "."], Some(path));
    git(&["commit", "-qm", "indexed"], Some(path));
    git(&["remote", "add", "origin", bare_arg], Some(path));
    git(&["push", "-q", "origin", "main"], Some(path));

    let indexer = CodeIndexer::new(db.clone());
    assert_eq!(
        indexer
            .index_repository(repo.id, &bare_path, "main")
            .await
            .expect("build the first snapshot"),
        1,
        "the fixture must have something to refresh"
    );

    // `worktree` is dropped here; the bare repository is what the refresh reads.
    drop(worktree);
    (repo.id, bare_path)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refresh_outlasts_a_writer_that_holds_the_database_for_seconds() {
    let dir = tempfile::tempdir().unwrap();
    let db = fresh_db(dir.path()).await;
    let repo_root = dir.path().join("repos");
    let (repo_id, bare_path) = indexed_repository(&db, &repo_root).await;

    // Park a writer on SQLite's single writer slot and keep it there. It is
    // deliberately not a second refresh: what matters is the *duration* a
    // holder can have, and an ordinary transaction reproduces it without
    // needing a repository big enough to take three seconds to index.
    let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel::<()>();
    let holder_db = db.clone();
    let holder = tokio::spawn(async move {
        let transaction = holder_db.begin().await.expect("open the holding writer");
        transaction
            .execute_unprepared("UPDATE users SET updated_at = updated_at")
            .await
            .expect("take SQLite's write lock");
        held_tx.send(()).expect("announce the lock is held");
        release_rx.await.expect("wait to be released");
        transaction.commit().await.expect("release the write lock");
    });
    held_rx.await.expect("the writer took the lock");

    let releaser = tokio::spawn(async move {
        tokio::time::sleep(HOLD).await;
        // The holder is still parked on the `await` at this point, so the only
        // way the receiver is gone is that its task died — which the `holder`
        // join below reports far better than a panic from here would.
        if release_tx.send(()).is_err() {
            eprintln!("the holding writer went away before it was released");
        }
    });

    let started = std::time::Instant::now();
    let indexed = CodeIndexer::new(db.clone())
        .index_repository(repo_id, &bare_path, "main")
        .await;

    holder.await.expect("the holding writer finished");
    releaser.await.expect("the releaser finished");

    let indexed = indexed.unwrap_or_else(|error| {
        panic!(
            "a refresh gave up while another writer held the database for {HOLD:?}: {error:#}\n\
             the retry budget has to be measured against how long a holder keeps the lock, not \
             against a number of instant refusals"
        )
    });
    assert_eq!(indexed, 1, "the refresh published a complete snapshot");
    assert!(
        started.elapsed() >= HOLD,
        "the refresh returned in {:?}, before the holder let go — the fixture did not actually \
         contend and proves nothing",
        started.elapsed()
    );

    // The snapshot really is the new one, not the one the fixture built before
    // the contention: a refresh that returned `Ok` without writing would pass
    // every assertion above.
    let indexer = CodeIndexer::new(db.clone());
    assert_eq!(
        indexer
            .indexed_file_count(repo_id)
            .await
            .expect("count the published snapshot"),
        1
    );
}
