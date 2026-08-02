//! card_d6ff3d89e9a2: `mirror::service::create_mirror` reads `mirrors` for the
//! repository and then inserts, in two statements. Two concurrent
//! `POST /repos/{owner}/{name}/mirror` can both read "no mirror yet"; one wins,
//! and the loser used to come back with the raw `UNIQUE(mirrors.repo_id)`
//! failure, which `AppError::from` renders as a 5xx. The caller did nothing
//! wrong and nothing is broken — the mirror simply already exists.
//!
//! What these tests guard:
//!
//! * **The race resolves to one row and one answer.** Exactly one attempt is
//!   created; every other one is told the mirror already exists, in the same
//!   words the pre-read branch uses and with no constraint text.
//! * **The fold stays narrow.** An insert that fails for any other reason is
//!   still an error, or a storage outage would be reported to the client as
//!   their own conflict.

use sea_orm::ConnectionTrait;

const REMOTE: &str = "https://example.com/upstream.git";
const ENCRYPTION_KEY: &str = "test-encryption-key-for-mirror-race";

/// A migrated database with more than one pooled connection, so concurrent
/// tasks really do run their statements against separate connections.
async fn setup(directory: &std::path::Path) -> (sea_orm::DatabaseConnection, i64) {
    let db = rg_db::connect_with_pool(
        &format!("sqlite://{}?mode=rwc", directory.join("test.db").display()),
        rg_db::TEST_CONNECT_TIMEOUT_SECS,
        60,
        4,
    )
    .await
    .expect("connect sqlite");
    rg_db::run_migrations(&db).await.expect("run migrations");

    let owner = rg_db::ops::user_ops::create_user(&db, "mirrorer", "m@example.invalid", "", "M")
        .await
        .expect("create the account the repository hangs off");
    let now = chrono::Utc::now();
    let repo = rg_db::ops::repo_ops::create(
        &db,
        rg_db::entities::repository::ActiveModel {
            id: sea_orm::NotSet,
            owner_id: sea_orm::Set(owner.id),
            name: sea_orm::Set("mirrored".to_string()),
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
    .expect("create the repository the mirror hangs off");

    (db, repo.id)
}

async fn mirror_rows(db: &sea_orm::DatabaseConnection, repo_id: i64) -> i64 {
    db.query_one(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        format!("SELECT COUNT(*) AS n FROM mirrors WHERE repo_id = {repo_id}"),
    ))
    .await
    .expect("count mirrors")
    .expect("one row")
    .try_get::<i64>("", "n")
    .expect("count column")
}

// Multi-threaded on purpose: the window this guards sits between the `SELECT`
// and the `INSERT` on two different pooled connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_mirror_registrations_leave_one_row_and_one_caller_conflict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id) = setup(directory.path()).await;

    // Eight operators pressing the same button. They start together, so several
    // of them read "no mirror yet" before any of them has written one.
    let attempts: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move {
                rg_core::mirror::service::create_mirror(
                    &db,
                    repo_id,
                    REMOTE.to_string(),
                    None,
                    None,
                    3600,
                    ENCRYPTION_KEY,
                )
                .await
            })
        })
        .collect();

    let mut created = 0;
    let mut refused = 0;
    for attempt in attempts {
        match attempt.await.expect("task panicked") {
            Ok(_) => created += 1,
            Err(error) => {
                refused += 1;
                assert_eq!(
                    error.to_string(),
                    "mirror already exists for this repository",
                    "the loser of the race must get the pre-read branch's own answer, \
                     not a database failure: {error:#}"
                );
                assert!(
                    error
                        .downcast_ref::<rg_core::error::InvalidRequest>()
                        .is_some(),
                    "the answer must carry the client-error type, or the handler \
                     still renders a 5xx: {error:#}"
                );
                let chain = format!("{error:#}").to_ascii_lowercase();
                for leak in ["unique", "constraint", "db:", "sqlite"] {
                    assert!(
                        !chain.contains(leak),
                        "the message reaches the client verbatim and must not carry {leak:?}: \
                         {error:#}"
                    );
                }
            }
        }
    }

    assert_eq!(created, 1, "exactly one attempt may register the mirror");
    assert_eq!(refused, 7, "every other attempt must be answered, not dropped");
    assert_eq!(
        mirror_rows(&db, repo_id).await,
        1,
        "the repository must end up with exactly one mirror row"
    );
}

/// The fold is armed by the UNIQUE violation alone. A write that fails for any
/// other reason — here a storage layer that refuses the insert — must stay an
/// error, or an outage would be reported to the client as their own conflict.
#[tokio::test]
async fn an_insert_that_fails_for_another_reason_is_not_reported_as_an_existing_mirror() {
    let directory = tempfile::tempdir().expect("temp dir");
    let (db, repo_id) = setup(directory.path()).await;

    // Fails the INSERT only, so the pre-read still answers "no mirror yet" and
    // the failure is reached where the classification lives.
    db.execute_unprepared(
        "CREATE TRIGGER mirrors_storage_outage BEFORE INSERT ON mirrors \
         BEGIN SELECT RAISE(ABORT, 'storage is unavailable'); END;",
    )
    .await
    .expect("install the write fault");

    let error = rg_core::mirror::service::create_mirror(
        &db,
        repo_id,
        REMOTE.to_string(),
        None,
        None,
        3600,
        ENCRYPTION_KEY,
    )
    .await
    .expect_err("the insert was refused");

    assert!(
        error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .is_none(),
        "a refused write is not the caller's conflict: {error:#}"
    );
    assert_eq!(
        mirror_rows(&db, repo_id).await,
        0,
        "nothing may be left behind by the refused write"
    );
}
