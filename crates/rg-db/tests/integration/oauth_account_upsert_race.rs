//! card_66e981aa1234: the old `oauth_account_ops::upsert` promised
//! create-or-update but
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
//! * **A newer unlink wins.** Touching an already-observed identity is
//!   update-only and returns typed absence instead of recreating the row.
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
            "plombir-git-oauth-upsert-{label}-{}.db",
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

    // Eight callbacks of the same first login. They start together, so several
    // of them read "no such link" before any of them has written one.
    let attempts = (0..8).map(|_| {
        let db = db.clone();
        async move {
            rg_db::ops::oauth_account_ops::link(
                &db,
                user.id,
                "gitea",
                "provider-uid-1",
                "alice",
                "alice@example.com",
            )
            .await
        }
    });

    let results = futures_join_all(attempts).await;
    for (i, result) in results.iter().enumerate() {
        assert!(
            matches!(result, Ok(Some(_))),
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

    // Winners and losers alike end up looking at that one row, and it belongs
    // to the account the callbacks were signing in.
    let linked =
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "gitea", "provider-uid-1")
            .await
            .expect("read the link back")
            .expect("the link exists");
    assert_eq!(linked.user_id, user.id);
    assert_eq!(linked.provider_user_id, "provider-uid-1");
}

#[tokio::test]
async fn an_insert_that_fails_on_something_other_than_uniqueness_is_still_an_error() {
    let (db, _temp) = setup("fk").await;

    // No user 9999 — `oauth_accounts.user_id` has a foreign key onto `users`,
    // and SQLite enforces it (`connect_sqlite` sets `foreign_keys = ON`).
    let result = rg_db::ops::oauth_account_ops::link(
        &db,
        9999,
        "gitea",
        "orphan-uid",
        "nobody",
        "nobody@example.com",
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

/// A second sign-in through a link that already exists updates it in place.
///
/// This test used to be about the provider's tokens — the second call had to
/// refresh the access token without dropping the stored refresh token. Those
/// columns are gone (`m20260822_000002_drop_oauth_account_tokens`,
/// card_51dd82b6dc82), and what is left to pin is the part that never depended
/// on them: one identity occupies one row no matter how often it signs in, and
/// the row records the latest sign-in rather than staying frozen at the first.
#[tokio::test]
async fn a_second_call_updates_the_existing_link_instead_of_adding_a_second_row() {
    let (db, _temp) = setup("update").await;

    let user = rg_db::ops::user_ops::create_user(&db, "sso_bob", "bob@example.com", "", "Bob")
        .await
        .expect("create user");

    let first = rg_db::ops::oauth_account_ops::link(
        &db,
        user.id,
        "gitea",
        "uid-bob",
        "bob",
        "bob@example.com",
    )
    .await
    .expect("first link")
    .expect("the first link remains present");

    let stale_updated_at = first.updated_at - chrono::Duration::minutes(1);
    db.execute(Statement::from_sql_and_values(
        DatabaseBackend::Sqlite,
        "UPDATE oauth_accounts SET updated_at = ? WHERE id = ?",
        [stale_updated_at.into(), first.id.into()],
    ))
    .await
    .expect("make the pre-login timestamp observably stale");

    let touched = rg_db::ops::oauth_account_ops::touch_existing(&db, first.id)
        .await
        .expect("touch the existing link")
        .expect("the existing link remains present");
    assert_eq!(touched.id, first.id);

    let linked = rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "gitea", "uid-bob")
        .await
        .expect("read back")
        .expect("exists");
    assert_eq!(
        linked.id, first.id,
        "the second call must not mint a new link"
    );
    assert_eq!(
        linked.created_at, first.created_at,
        "the link was made once and that moment does not move",
    );
    assert!(
        linked.updated_at > stale_updated_at,
        "signing in again has to leave a trace on the link that was used",
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM oauth_accounts").await,
        1,
    );
}

#[tokio::test]
async fn an_unlink_after_lookup_is_not_undone_by_the_losing_touch() {
    let (db, _temp) = setup("unlink-wins").await;
    let user = rg_db::ops::user_ops::create_user(
        &db,
        "sso_unlinked",
        "unlinked@example.com",
        "",
        "Unlinked",
    )
    .await
    .expect("create user");
    let observed = rg_db::ops::oauth_account_ops::link(
        &db,
        user.id,
        "gitea",
        "uid-unlinked",
        "unlinked",
        "unlinked@example.com",
    )
    .await
    .expect("create link")
    .expect("the created link remains present");

    assert!(
        rg_db::ops::oauth_account_ops::delete_by_id(&db, observed.id, user.id)
            .await
            .expect("unlink the observed identity")
    );
    assert!(
        rg_db::ops::oauth_account_ops::touch_existing(&db, observed.id)
            .await
            .expect("absence is not a database failure")
            .is_none(),
        "the stale callback must observe the newer unlink"
    );
    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM oauth_accounts").await,
        0,
        "touching a stale id must not recreate the removed identity"
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
