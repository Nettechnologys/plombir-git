//! card_8e6bac4c98db: four more read-then-insert pairs that answered the loser
//! of a UNIQUE race with a raw constraint failure — which `AppError::from`
//! renders as a 5xx, blaming the server for something the caller can neither
//! cause nor fix.
//!
//! The four: `wiki::service::create_page`, `release::service::create_release`,
//! `repo::service::create_repo_with_opts` and `repo::service::fork_repo`. The
//! same generator as `mirror_create_race_tests.rs` (card_d6ff3d89e9a2), and the
//! same two things are guarded per site:
//!
//! * **The race resolves to one row and one answer.** Exactly one attempt
//!   creates the row; every loser is told the name is taken, in the same words
//!   the pre-read branch uses and with no constraint text.
//!
//!   The type checked here is `Conflict` (409), not `InvalidRequest` (400):
//!   card_cfba32a77acd moved the whole "that name is taken" family off 400,
//!   because the request is correct and it is an existing row that refuses it.
//!   What this file guards is unchanged either way — the loser must get the
//!   pre-read branch's answer rather than a database failure, and both branches
//!   must keep answering identically so the status code cannot become a side
//!   channel for "you lost the race".
//! * **The fold stays narrow.** A write that fails for any other reason is
//!   still an error, or a storage outage would be reported to the client as
//!   their own conflict.
//!
//! Wiki pages and releases are raced for real — eight tasks on a four
//! connection pool, and the assertions are invariants ("exactly one row, one
//! success, N-1 refusals") so no scheduler outcome makes them flaky.
//! Repositories are not: their create path also makes a directory on disk, so
//! two real racers collide in `gix init` before either reaches the insert. That
//! window is opened deterministically instead, with a `BEFORE INSERT` trigger
//! that plants the conflicting row — the insert then loses the race every run.
//!
//! card_a113f0339048 added the fifth and last site of the generator,
//! `collaborator::service::add_collaborator`, raced for real like the wiki and
//! release pairs. Its answer names the permission the membership already
//! carries, which the losing attempt does not know — so it re-reads to say it,
//! and what the race test proves is precisely that the loser still ends up with
//! the pre-read branch's whole sentence.

use sea_orm::ConnectionTrait;

async fn fresh_db(directory: &std::path::Path) -> sea_orm::DatabaseConnection {
    let db = crate::common::migrated_sqlite(&directory.join("test.db"), 4).await;
    db
}

async fn create_user(db: &sea_orm::DatabaseConnection, name: &str) -> i64 {
    rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.invalid"), "", name)
        .await
        .unwrap_or_else(|error| panic!("create user {name}: {error:#}"))
        .id
}

/// A repository row without a directory behind it — enough for wiki pages and
/// releases, which never touch the filesystem.
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
    .expect("create the repository the rows hang off")
    .id
}

async fn count(db: &sea_orm::DatabaseConnection, sql: &str) -> i64 {
    db.query_one(sea_orm::Statement::from_string(
        sea_orm::DatabaseBackend::Sqlite,
        sql.to_string(),
    ))
    .await
    .expect("count query")
    .expect("one row")
    .try_get::<i64>("", "n")
    .expect("count column")
}

/// What the loser must come back with: the pre-read branch's own wording, the
/// client-error type, and nothing of the database underneath.
fn assert_is_the_caller_s_conflict(error: &anyhow::Error, expected: &str) {
    assert_eq!(
        error.to_string(),
        expected,
        "the loser of the race must get the pre-read branch's own answer, not a \
         database failure: {error:#}"
    );
    assert!(
        error.downcast_ref::<rg_core::error::Conflict>().is_some(),
        "the answer must carry the client-error type, or the handler still renders \
         a 5xx: {error:#}"
    );
    let chain = format!("{error:#}").to_ascii_lowercase();
    for leak in ["unique", "constraint", "db:", "sqlite"] {
        assert!(
            !chain.contains(leak),
            "the message reaches the client verbatim and must not carry {leak:?}: {error:#}"
        );
    }
}

// ── wiki pages ────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_wiki_page_creations_leave_one_row_and_one_caller_conflict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "wikiracer").await;
    let repo_id = bare_repo_row(&db, owner_id, "wikiraced").await;

    let attempts: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move {
                rg_core::wiki::service::create_page(
                    &db,
                    repo_id,
                    "Home",
                    "first",
                    None,
                    Some(owner_id),
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
                assert_is_the_caller_s_conflict(
                    &error,
                    "wiki page 'Home' already exists in this repository",
                );
            }
        }
    }

    assert_eq!(created, 1, "exactly one attempt may create the page");
    assert_eq!(
        refused, 7,
        "every other attempt must be answered, not dropped"
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT COUNT(*) AS n FROM wiki_pages WHERE repo_id = {repo_id}")
        )
        .await,
        1,
        "the repository must end up with exactly one page called Home"
    );
}

#[tokio::test]
async fn a_wiki_insert_that_fails_for_another_reason_is_not_reported_as_a_duplicate_page() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "wikioutage").await;
    let repo_id = bare_repo_row(&db, owner_id, "wikidown").await;

    // Fails the INSERT only, so the pre-read still answers "no such page" and
    // the failure is reached where the classification lives.
    db.execute_unprepared(
        "CREATE TRIGGER wiki_pages_storage_outage BEFORE INSERT ON wiki_pages \
         BEGIN SELECT RAISE(ABORT, 'storage is unavailable'); END;",
    )
    .await
    .expect("install the write fault");

    let error =
        rg_core::wiki::service::create_page(&db, repo_id, "Home", "first", None, Some(owner_id))
            .await
            .expect_err("the insert was refused");

    assert!(
        error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .is_none(),
        "a refused write is not the caller's conflict: {error:#}"
    );
}

// ── releases ──────────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_release_creations_leave_one_row_and_one_caller_conflict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "releaser").await;
    let repo_id = bare_repo_row(&db, owner_id, "released").await;

    let attempts: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move {
                rg_core::release::service::create_release(
                    &db,
                    repo_id,
                    owner_id,
                    "v1.0.0",
                    "First",
                    None,
                    "main",
                    false,
                    false,
                    std::path::Path::new("."),
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
                assert_is_the_caller_s_conflict(&error, "release with tag 'v1.0.0' already exists");
            }
        }
    }

    assert_eq!(created, 1, "exactly one attempt may publish the tag");
    assert_eq!(
        refused, 7,
        "every other attempt must be answered, not dropped"
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT COUNT(*) AS n FROM releases WHERE repo_id = {repo_id}")
        )
        .await,
        1,
        "the repository must end up with exactly one release for the tag"
    );
}

#[tokio::test]
async fn a_release_insert_that_fails_for_another_reason_is_not_reported_as_a_duplicate_tag() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "releaseoutage").await;
    let repo_id = bare_repo_row(&db, owner_id, "releasedown").await;

    db.execute_unprepared(
        "CREATE TRIGGER releases_storage_outage BEFORE INSERT ON releases \
         BEGIN SELECT RAISE(ABORT, 'storage is unavailable'); END;",
    )
    .await
    .expect("install the write fault");

    let error = rg_core::release::service::create_release(
        &db,
        repo_id,
        owner_id,
        "v1.0.0",
        "First",
        None,
        "main",
        false,
        false,
        std::path::Path::new("."),
    )
    .await
    .expect_err("the insert was refused");

    assert!(
        error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .is_none(),
        "a refused write is not the caller's conflict: {error:#}"
    );
}

// ── repository creation and forks ─────────────────────────────────────────

/// Plants a row in the namespace the next matching insert is about to claim, so
/// that insert loses the UNIQUE race on `uq_repositories_namespace_name` — the
/// same loss two concurrent requests produce, minus the scheduling luck.
///
/// `guard` is the trigger's `WHEN`; the planted row carries a description no
/// guard matches, so it cannot re-arm the injector.
async fn plant_namespace_conflict(db: &sea_orm::DatabaseConnection, name: &str, guard: &str) {
    db.execute_unprepared(&format!(
        "CREATE TRIGGER {name} BEFORE INSERT ON repositories WHEN {guard} \
         BEGIN \
           INSERT INTO repositories \
             (owner_id, name, description, is_private, default_branch, \
              stars_count, forks_count, created_at, updated_at) \
           VALUES \
             (NEW.owner_id, NEW.name, 'planted by the race injector', NEW.is_private, \
              NEW.default_branch, 0, 0, NEW.created_at, NEW.updated_at); \
         END;"
    ))
    .await
    .expect("install the race injector");
}

#[tokio::test]
async fn a_repository_create_that_loses_the_namespace_race_is_the_caller_s_conflict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "creator").await;
    let repo_root = directory.path().join("repos");

    plant_namespace_conflict(
        &db,
        "repositories_create_race",
        "NEW.name = 'racy' AND NEW.description IS NULL",
    )
    .await;

    let error =
        rg_core::repo::service::create_repo(&db, owner_id, "racy", None, false, &repo_root, None)
            .await
            .expect_err("the insert lost the race");

    assert_is_the_caller_s_conflict(&error, "repository 'racy' already exists");
    // SQLite rolls a failed statement back together with what its trigger did,
    // so the planted row goes with it. What matters is the same either way: the
    // losing attempt left nothing of its own behind.
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) AS n FROM repositories WHERE name = 'racy'"
        )
        .await,
        0,
        "the losing attempt must not leave a row behind"
    );
    assert!(
        !repo_root.join("creator/racy.git").exists(),
        "the directory of the losing attempt must still be discarded — leaving it \
         makes the name permanently un-creatable"
    );
}

#[tokio::test]
async fn a_repository_create_that_fails_for_another_reason_stays_an_error() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "creatoroutage").await;
    let repo_root = directory.path().join("repos");

    db.execute_unprepared(
        "CREATE TRIGGER repositories_storage_outage BEFORE INSERT ON repositories \
         BEGIN SELECT RAISE(ABORT, 'storage is unavailable'); END;",
    )
    .await
    .expect("install the write fault");

    let error =
        rg_core::repo::service::create_repo(&db, owner_id, "downy", None, false, &repo_root, None)
            .await
            .expect_err("the insert was refused");

    assert!(
        error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .is_none(),
        "a refused write is not the caller's conflict: {error:#}"
    );
}

#[tokio::test]
async fn a_fork_that_loses_the_namespace_race_is_the_caller_s_conflict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let source_owner_id = create_user(&db, "upstream").await;
    let forker_id = create_user(&db, "forker").await;
    let repo_root = directory.path().join("repos");

    let source = rg_core::repo::service::create_repo(
        &db,
        source_owner_id,
        "cloneme",
        None,
        false,
        &repo_root,
        None,
    )
    .await
    .expect("create the source repository");

    // Armed only now, so the source repository above is created untouched.
    plant_namespace_conflict(
        &db,
        "repositories_fork_race",
        "NEW.origin_repo_id IS NOT NULL",
    )
    .await;

    let error =
        match rg_core::repo::service::fork_repo(&db, forker_id, "upstream", &source, &repo_root)
            .await
        {
            Ok(forked) => panic!(
                "the fork must lose the planted race, but it created repository {}",
                forked.repo.id
            ),
            Err(error) => error,
        };

    assert_is_the_caller_s_conflict(
        &error,
        "repository 'cloneme' already exists in your account",
    );
    assert!(
        !repo_root.join("forker/cloneme.git").exists(),
        "the clone of the losing attempt must still be discarded"
    );
}

// ── collaborators ─────────────────────────────────────────────────────────

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_collaborator_additions_leave_one_row_and_one_caller_conflict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "collabowner").await;
    let friend_id = create_user(&db, "collabfriend").await;
    let repo_id = bare_repo_row(&db, owner_id, "collabraced").await;

    let attempts: Vec<_> = (0..8)
        .map(|_| {
            let db = db.clone();
            tokio::spawn(async move {
                rg_core::collaborator::service::add_collaborator(
                    &db,
                    "collabowner",
                    "collabraced",
                    friend_id,
                    "write".to_string(),
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
                // The whole sentence, permission clause included: the loser
                // re-reads the winning row so it answers in the pre-read
                // branch's own words rather than a shortened variant that
                // would tell the caller which of the two noticed.
                assert_is_the_caller_s_conflict(
                    &error,
                    &format!("user {friend_id} is already a collaborator (permission: write)"),
                );
            }
        }
    }

    assert_eq!(created, 1, "exactly one attempt may add the collaborator");
    assert_eq!(
        refused, 7,
        "every other attempt must be answered, not dropped"
    );
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) AS n FROM repo_collaborators \
                 WHERE repo_id = {repo_id} AND user_id = {friend_id}"
            )
        )
        .await,
        1,
        "the repository must end up with exactly one membership for the user"
    );
}

/// The eight-task race above cannot promise that any given run has a loser
/// reaching the *insert* rather than the pre-read, so the classification itself
/// is also opened deterministically: a `BEFORE INSERT` trigger plants the
/// conflicting membership, and the insert loses every run.
///
/// SQLite rolls a failed statement back together with what its trigger did, so
/// the planted row is gone by the time the loser re-reads it — which is exactly
/// the "membership removed again in the meantime" case. The answer must still
/// be the caller's 409, just without the permission clause it can no longer
/// name; degrading to a 500 there would put the whole fix back.
#[tokio::test]
async fn a_collaborator_insert_that_loses_the_membership_race_is_the_caller_s_conflict() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "collabracer").await;
    let friend_id = create_user(&db, "collabracedfriend").await;
    let repo_id = bare_repo_row(&db, owner_id, "collabplanted").await;

    // The planted row carries a permission the guard does not match, so it
    // cannot re-arm the injector.
    db.execute_unprepared(
        "CREATE TRIGGER repo_collaborators_add_race BEFORE INSERT ON repo_collaborators \
         WHEN NEW.permission = 'write' \
         BEGIN \
           INSERT INTO repo_collaborators (repo_id, user_id, permission, created_at) \
           VALUES (NEW.repo_id, NEW.user_id, 'read', NEW.created_at); \
         END;",
    )
    .await
    .expect("install the race injector");

    let error = rg_core::collaborator::service::add_collaborator(
        &db,
        "collabracer",
        "collabplanted",
        friend_id,
        "write".to_string(),
    )
    .await
    .expect_err("the insert lost the race");

    assert_is_the_caller_s_conflict(
        &error,
        &format!("user {friend_id} is already a collaborator"),
    );
    assert_eq!(
        count(
            &db,
            &format!("SELECT COUNT(*) AS n FROM repo_collaborators WHERE repo_id = {repo_id}")
        )
        .await,
        0,
        "the losing attempt must not leave a row behind"
    );
}

#[tokio::test]
async fn a_collaborator_insert_that_fails_for_another_reason_is_not_reported_as_a_duplicate() {
    let directory = tempfile::tempdir().expect("temp dir");
    let db = fresh_db(directory.path()).await;
    let owner_id = create_user(&db, "collabouroutage").await;
    let friend_id = create_user(&db, "collabfrienddown").await;
    bare_repo_row(&db, owner_id, "collabdown").await;

    // Fails the INSERT only, so the pre-read still answers "not a collaborator"
    // and the failure is reached where the classification lives.
    db.execute_unprepared(
        "CREATE TRIGGER repo_collaborators_storage_outage BEFORE INSERT ON repo_collaborators \
         BEGIN SELECT RAISE(ABORT, 'storage is unavailable'); END;",
    )
    .await
    .expect("install the write fault");

    let error = rg_core::collaborator::service::add_collaborator(
        &db,
        "collabouroutage",
        "collabdown",
        friend_id,
        "write".to_string(),
    )
    .await
    .expect_err("the insert was refused");

    assert!(
        error.downcast_ref::<rg_core::error::Conflict>().is_none(),
        "a refused write is not the caller's conflict: {error:#}"
    );
    assert!(
        error
            .downcast_ref::<rg_core::error::InvalidRequest>()
            .is_none(),
        "nor is it a malformed request: {error:#}"
    );
}
