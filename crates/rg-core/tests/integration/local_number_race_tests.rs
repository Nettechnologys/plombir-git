//! card_06d88c4f02c2: issue and pull request numbers are repository-local, and
//! both were allocated by reading `MAX(number) + 1` in a statement of their own
//! and writing it later. `(repo_id, number)` is UNIQUE on both tables, so the
//! data was never corrupted — but the loser of a race the server opened between
//! its own read and its own write was handed a raw constraint failure, which
//! `AppError::from` renders as a 5xx. Two people filing an issue at the same
//! moment is not a malformed request, and an importer running next to live
//! traffic should not have to retry rows by hand.
//!
//! The allocator's own primitives (a number taken between the read and the
//! write, a failure that re-reading cannot fix) are pinned next to the code, in
//! `rg_core::issue::service::number_allocation_tests` and its pull request
//! twin, where the private test seam can open the window on every run. What
//! this file adds is the whole public path under real concurrency: nothing the
//! service does around the insert — the label junction, the transaction, the
//! webhook — may turn a correct create into a failure or into a repeated
//! number.

async fn fresh_db(directory: &std::path::Path) -> sea_orm::DatabaseConnection {
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", directory.join("test.db").display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        4,
    )
    .await
    .expect("connect sqlite");
    rg_db::run_migrations(&db).await.expect("run migrations");
    db
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

/// Eight filings at once on a four connection pool. The assertion is an
/// invariant — every correct call succeeds, and the eight numbers are exactly
/// 1..=8 — so no scheduling outcome makes it flaky, and the pre-fix allocator
/// fails it whichever way the tasks interleave.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn eight_simultaneous_filings_take_eight_consecutive_numbers() {
    const FILINGS: usize = 8;

    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let author_id =
        rg_db::ops::user_ops::create_user(&db, "numrace", "numrace@example.invalid", "", "numrace")
            .await
            .expect("create user")
            .id;
    let repo_id = bare_repo_row(&db, author_id, "numrace").await;

    let start = std::sync::Arc::new(tokio::sync::Barrier::new(FILINGS));
    let mut filings = Vec::with_capacity(FILINGS);
    for i in 0..FILINGS {
        let db = db.clone();
        let start = start.clone();
        filings.push(tokio::spawn(async move {
            start.wait().await;
            rg_core::issue::service::create_issue(
                &db,
                repo_id,
                author_id,
                format!("concurrent filing {i}"),
                None,
                None,
                None,
            )
            .await
        }));
    }

    let mut numbers = Vec::with_capacity(FILINGS);
    for (i, filing) in filings.into_iter().enumerate() {
        let issue = filing
            .await
            .unwrap_or_else(|error| panic!("filing task {i} panicked: {error}"))
            .unwrap_or_else(|error| panic!("filing {i} failed: {error:#}"));
        numbers.push(issue.number);
    }

    numbers.sort_unstable();
    assert_eq!(
        numbers,
        (1..=FILINGS as i64).collect::<Vec<_>>(),
        "every correct filing keeps a number, and no number is handed out twice"
    );

    let stored = rg_db::ops::issue_ops::list_by_repo(&db, repo_id, None)
        .await
        .expect("read the issues back");
    assert_eq!(
        stored.len(),
        FILINGS,
        "the successful calls and the stored rows must be the same set"
    );
}

/// card_f0fd0aaa87b5: the attempt budget is a runaway guard, not a concurrency
/// budget — and until this test nothing held it to that.
///
/// The two outcomes the allocator retries on are told apart by what the next
/// attempt can hope for. A UNIQUE violation means someone committed, so
/// `MAX(number) + 1` has already moved and retrying at once is right. A busy
/// backend means nothing has moved and nothing will until the holder commits,
/// and retrying at once is a busy-spin: thirty-two attempts finish in
/// milliseconds, all of them refused by the same held lock, and a correct
/// create is answered with `after 32 concurrent conflicts` — a 5xx.
///
/// SQLite produces exactly that refusal here and `busy_timeout` does not absorb
/// it. The allocator reads inside a deferred transaction and writes inside the
/// same one, so its `INSERT` has to promote a read lock to a write lock; SQLite
/// refuses a promotion *immediately* rather than waiting, because waiting on it
/// could deadlock. Every attempt is therefore fast and free, which is what lets
/// the whole budget burn while the writer below is still holding the lock.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_lock_held_by_someone_else_does_not_burn_the_attempt_budget() {
    use sea_orm::{ConnectionTrait, Statement, TransactionTrait};

    /// Long enough that an unwaiting loop is certainly exhausted inside it
    /// (thirty-two refusals cost single-digit milliseconds), and short enough
    /// to sit far under what the backoff schedule can wait out.
    const HOLD: std::time::Duration = std::time::Duration::from_millis(100);

    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let author_id = rg_db::ops::user_ops::create_user(
        &db,
        "lockhold",
        "lockhold@example.invalid",
        "",
        "lockhold",
    )
    .await
    .expect("create user")
    .id;
    let repo_id = bare_repo_row(&db, author_id, "lockhold").await;

    // A pool of its own, so the holder never competes with the filing for a
    // connection — the point is the database's write lock, not the pool's.
    let holder = fresh_db(directory.path()).await;
    let held = holder.begin().await.expect("begin the holding transaction");
    held.execute(Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        "UPDATE repositories SET description = 'holding the write lock' WHERE id = 1".to_string(),
    ))
    .await
    .expect("take the write lock");

    let filing = tokio::spawn({
        let db = db.clone();
        async move {
            rg_core::issue::service::create_issue(
                &db,
                repo_id,
                author_id,
                "filed while the write lock was held".to_string(),
                None,
                None,
                None,
            )
            .await
        }
    });

    tokio::time::sleep(HOLD).await;
    held.commit().await.expect("release the write lock");

    let issue = filing
        .await
        .expect("the filing task panicked")
        .expect("a create that only had to wait for a held write lock was refused");
    assert_eq!(
        issue.number, 1,
        "the filing landed, but not on the first free number"
    );
}
