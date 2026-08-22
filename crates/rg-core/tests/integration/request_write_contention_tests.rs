//! card_d5612b049af6: a request-bound write must outlast the writer ahead of it.
//!
//! `card_0b936c68e1ea` fixed the code-index refresh's budget. This is the other
//! half of the same lock: that refresh publishes a whole repository snapshot in
//! ONE transaction, which on SQLite is the database-wide writer lock held for
//! as long as the insert takes — seconds on a tree of a few thousand files.
//! Every remaining read-then-write loop was budgeted at thirty-two attempts,
//! and on SQLite such a transaction is refused *instantly* rather than waiting
//! on `busy_timeout`, so thirty-two attempts came to about a third of a second
//! however long the holder actually needed. A push into a large repository
//! therefore made a concurrent `POST .../issues`, wiki edit or board
//! drag-and-drop lose **by construction**, and the caller was told 5xx for
//! something they did nothing wrong in.
//!
//! So the assertion is not "a create survives contention" in the abstract. It
//! is: a holder that keeps the database for longer than the old budget could
//! ever have waited does not cost a correct caller its write.
//!
//! The holder is an ordinary transaction rather than a second index refresh,
//! for the same reason `code_index_contention_tests` gives: what matters is the
//! *duration* a holder can have, and an `UPDATE` reproduces it without needing
//! a repository big enough to take three seconds to index.

use std::path::Path;
use std::time::Duration;

use sea_orm::{ConnectionTrait, TransactionTrait};

/// How long the competing writer keeps SQLite's write lock.
///
/// Comfortably past the ~0.34 s the thirty-two-attempt budget could reach, so
/// this fails against the old shape for the reason the card measured rather
/// than by being tuned to the edge of it — and comfortably under
/// `REQUEST_WRITE_BUDGET`, so a loaded machine cannot make the new shape look
/// broken either.
const HOLD: Duration = Duration::from_secs(3);

async fn fresh_db(directory: &Path) -> sea_orm::DatabaseConnection {
    // Four connections: the holder parks one for the whole test, and the
    // writers under test need their own alongside the fixture's reads.
    crate::common::migrated_sqlite(&directory.join("test.db"), 4).await
}

async fn account(db: &sea_orm::DatabaseConnection, username: &str) -> i64 {
    rg_db::ops::user_ops::create_user(
        db,
        username,
        &format!("{username}@example.invalid"),
        "",
        "Contention Owner",
    )
    .await
    .expect("create the account the repository hangs off")
    .id
}

/// A repository row without a directory behind it — issues never touch the
/// filesystem.
async fn bare_repo_row(db: &sea_orm::DatabaseConnection, owner_id: i64, name: &str) -> i64 {
    let now = chrono::Utc::now();
    rg_db::ops::repo_ops::create(
        db,
        rg_db::entities::repository::ActiveModel {
            id: sea_orm::NotSet,
            owner_id: sea_orm::Set(owner_id),
            name: sea_orm::Set(name.to_string()),
            description: sea_orm::Set(None),
            is_private: sea_orm::Set(false),
            default_branch: sea_orm::Set("main".to_string()),
            fork_id: sea_orm::Set(None),
            stars_count: sea_orm::Set(0),
            forks_count: sea_orm::Set(0),
            org_id: sea_orm::Set(None),
            created_at: sea_orm::Set(now),
            updated_at: sea_orm::Set(now),
            deleted_at: sea_orm::Set(None),
            origin_repo_id: sea_orm::Set(None),
        },
    )
    .await
    .expect("create the repository the issues hang off")
    .id
}

/// Park a writer on SQLite's single writer slot, release it after [`HOLD`], and
/// return a handle that has to be awaited so a writer that died is reported as
/// itself rather than as the subject's failure.
async fn hold_the_write_lock(
    db: &sea_orm::DatabaseConnection,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
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
        // The holder is still parked on its `await` here, so the only way the
        // receiver is gone is that its task died — which the join below reports
        // far better than a panic from here would.
        if release_tx.send(()).is_err() {
            eprintln!("the holding writer went away before it was released");
        }
    });
    (holder, releaser)
}

/// The path the card measured: filing an issue while a push indexes.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn filing_an_issue_outlasts_a_writer_that_holds_the_database_for_seconds() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner = account(&db, "issuecontention").await;
    let repo_id = bare_repo_row(&db, owner, "contended").await;

    let (holder, releaser) = hold_the_write_lock(&db).await;

    let started = std::time::Instant::now();
    let filed = rg_core::issue::create_issue(
        &db,
        repo_id,
        owner,
        "filed while the database was busy".to_string(),
        None,
        None,
        None,
    )
    .await;

    holder.await.expect("the holding writer finished");
    releaser.await.expect("the releaser finished");

    let issue = filed.unwrap_or_else(|error| {
        panic!(
            "a correct filing gave up while another writer held the database for {HOLD:?}: \
             {error:#}\nthe retry budget has to be measured against how long a holder keeps the \
             lock, not against a number of instant refusals"
        )
    });
    assert_eq!(
        issue.number, 1,
        "the filing took the repository's first number"
    );
    assert!(
        started.elapsed() >= HOLD,
        "the filing returned in {:?}, before the holder let go — the fixture did not actually \
         contend and proves nothing",
        started.elapsed()
    );
}

/// The wiki edit is the same shape — read the page, snapshot it, overwrite —
/// and was on the same budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wiki_edit_outlasts_a_writer_that_holds_the_database_for_seconds() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner = account(&db, "wikicontention").await;
    let repo_id = bare_repo_row(&db, owner, "contendedwiki").await;

    rg_core::wiki::service::create_page(&db, repo_id, "Home", "first", None, Some(owner))
        .await
        .expect("seed the page the edit overwrites");

    let (holder, releaser) = hold_the_write_lock(&db).await;

    let started = std::time::Instant::now();
    let edited =
        rg_core::wiki::service::update_page(&db, repo_id, "Home", "second", None, Some(owner))
            .await;

    holder.await.expect("the holding writer finished");
    releaser.await.expect("the releaser finished");

    let page = edited.unwrap_or_else(|error| {
        panic!(
            "a correct wiki edit gave up while another writer held the database for {HOLD:?}: \
             {error:#}"
        )
    });
    assert_eq!(
        page.content, "second",
        "the edit that reported success stored its content"
    );
    assert!(
        started.elapsed() >= HOLD,
        "the edit returned in {:?}, before the holder let go — the fixture did not actually \
         contend and proves nothing",
        started.elapsed()
    );
}
