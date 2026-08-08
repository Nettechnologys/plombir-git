//! card_66e981aa1234: `oauth_account_ops::upsert` promises create-or-update but
//! reads and writes in two statements. Two callbacks of the same *first* SSO
//! login both see no row and both insert; `(provider, provider_user_id)` is
//! UNIQUE, so one of them used to come back with a constraint error that the
//! HTTP layer turned into a 500 on an otherwise valid sign-in.
//!
//! What each test guards:
//!
//! * **The race resolves to one row.** Concurrent first calls all succeed and
//!   leave a single identity — not two rows, and not one success plus one 500.
//! * **A real write failure is still a failure.** The retry is armed only by a
//!   UNIQUE violation; a foreign-key failure must not be re-read into a
//!   fabricated success.
//! * **The classifier answers from the backend's code.** Including through the
//!   `anyhow` context `user_ops` attaches, which is the shape the SSO
//!   first-login path actually sees.

use rg_db::sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-oauth-upsert-{label}-{}.db",
            uuid::Uuid::new_v4().simple()
        ));
        Self { path }
    }

    fn url(&self) -> String {
        format!("sqlite://{}?mode=rwc", self.path.display())
    }
}

impl Drop for TempDb {
    #[allow(
        clippy::let_underscore_must_use,
        reason = "cleanup must not mask the assertion that failed the test"
    )]
    fn drop(&mut self) {
        for suffix in ["", "-wal", "-shm"] {
            let _ = std::fs::remove_file(format!("{}{suffix}", self.path.display()));
        }
    }
}

/// A migrated database with more than one pooled connection, so concurrent
/// tasks really do run their statements against separate connections.
async fn setup(label: &str) -> (DatabaseConnection, TempDb) {
    let temp = TempDb::new(label);
    let db = rg_db::connect_with_pool(&temp.url(), rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    (db, temp)
}

async fn scalar(db: &DatabaseConnection, sql: &str) -> i64 {
    db.query_one(Statement::from_string(DatabaseBackend::Sqlite, sql))
        .await
        .expect("query")
        .expect("one row")
        .try_get::<i64>("", "n")
        .expect("count column")
}

// Multi-threaded on purpose: the window this guards sits between a `SELECT`
// and an `INSERT` on two different pooled connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_first_links_of_one_identity_all_succeed_and_leave_one_row() {
    let (db, _temp) = setup("race").await;

    let user =
        rg_db::ops::user_ops::create_user(&db, "sso_alice", "alice@example.com", "", "Alice")
            .await
            .expect("create the account the link hangs off");

    // Eight callbacks of the same first login, each offering its own access
    // token. They start together, so several of them read "no such link"
    // before any of them has written one.
    let attempts = (0..8).map(|i| {
        let db = db.clone();
        async move {
            rg_db::ops::oauth_account_ops::upsert(
                &db,
                user.id,
                "gitea",
                "provider-uid-1",
                "alice",
                "alice@example.com",
                Some(&format!("access-token-{i}")),
                Some(&format!("refresh-token-{i}")),
                None,
            )
            .await
        }
    });

    let results = futures_join_all(attempts).await;
    for (i, result) in results.iter().enumerate() {
        assert!(
            result.is_ok(),
            "callback {i} of a concurrent first login failed: {:?}",
            result.as_ref().err()
        );
    }

    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM oauth_accounts \
             WHERE provider = 'gitea' AND provider_user_id = 'provider-uid-1'",
        )
        .await,
        1,
        "the identity must occupy exactly one row",
    );

    // Every winner and every loser wrote a real token onto that one row.
    let linked =
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "gitea", "provider-uid-1")
            .await
            .expect("read the link back")
            .expect("the link exists");
    assert_eq!(linked.user_id, user.id);
    let stored = linked.access_token.expect("an access token was stored");
    assert!(
        stored.starts_with("access-token-"),
        "stored access token came from one of the callbacks, got {stored:?}",
    );
}

#[tokio::test]
async fn an_insert_that_fails_on_something_other_than_uniqueness_is_still_an_error() {
    let (db, _temp) = setup("fk").await;

    // No user 9999 — `oauth_accounts.user_id` has a foreign key onto `users`,
    // and SQLite enforces it (`connect_sqlite` sets `foreign_keys = ON`).
    let result = rg_db::ops::oauth_account_ops::upsert(
        &db,
        9999,
        "gitea",
        "orphan-uid",
        "nobody",
        "nobody@example.com",
        Some("access"),
        None,
        None,
    )
    .await;

    let error = result.expect_err("a foreign-key failure must stay a failure");
    assert!(
        !rg_db::is_unique_violation(&error),
        "an FK failure is not a uniqueness loss: {error}",
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM oauth_accounts").await,
        0,
        "nothing may be written when the insert failed",
    );
}

#[tokio::test]
async fn a_second_call_updates_the_existing_link_without_dropping_a_stored_refresh_token() {
    let (db, _temp) = setup("update").await;

    let user = rg_db::ops::user_ops::create_user(&db, "sso_bob", "bob@example.com", "", "Bob")
        .await
        .expect("create user");

    rg_db::ops::oauth_account_ops::upsert(
        &db,
        user.id,
        "gitea",
        "uid-bob",
        "bob",
        "bob@example.com",
        Some("access-1"),
        Some("refresh-1"),
        None,
    )
    .await
    .expect("first link");

    // A refresh response that carries no new refresh token means "keep the one
    // you have" — overwriting it with NULL would end the session at the next
    // refresh.
    rg_db::ops::oauth_account_ops::upsert(
        &db,
        user.id,
        "gitea",
        "uid-bob",
        "bob",
        "bob@example.com",
        Some("access-2"),
        None,
        None,
    )
    .await
    .expect("second link");

    let linked = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "gitea", "uid-bob")
        .await
        .expect("read back")
        .expect("exists");
    assert_eq!(linked.access_token.as_deref(), Some("access-2"));
    assert_eq!(linked.refresh_token.as_deref(), Some("refresh-1"));
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM oauth_accounts").await,
        1,
    );
}

#[tokio::test]
async fn a_unique_violation_is_recognised_through_the_anyhow_context_user_ops_attaches() {
    let (db, _temp) = setup("classify").await;

    rg_db::ops::user_ops::create_user(&db, "carol", "carol@example.com", "", "Carol")
        .await
        .expect("first account");

    // `user_ops` returns `anyhow::Result` and wraps every call in a `db: ...`
    // context, so the SSO first-login path never sees the `DbErr` directly.
    let error = rg_db::ops::user_ops::create_user(&db, "carol", "carol2@example.com", "", "Carol")
        .await
        .expect_err("the username is UNIQUE");
    assert!(
        rg_db::is_unique_violation_anyhow(&error),
        "a wrapped UNIQUE violation must still be recognised: {error:#}",
    );

    // And the classifier does not fire on some other database error.
    let not_found = anyhow::Error::from(rg_db::sea_orm::DbErr::RecordNotFound("user".into()))
        .context("db: find user by id");
    assert!(!rg_db::is_unique_violation_anyhow(&not_found));
}

/// `futures::future::join_all` without taking a dependency on `futures` for one
/// call: poll the futures together by handing them to the runtime as tasks.
async fn futures_join_all<F>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output>
where
    F: std::future::Future + Send + 'static,
    F::Output: Send + 'static,
{
    let handles: Vec<_> = futures.into_iter().map(tokio::spawn).collect();
    let mut out = Vec::with_capacity(handles.len());
    for handle in handles {
        out.push(handle.await.expect("task panicked"));
    }
    out
}
