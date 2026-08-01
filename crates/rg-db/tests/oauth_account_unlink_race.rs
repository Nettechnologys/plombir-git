//! card_7940cec09c77: `DELETE /api/v1/auth/sso/{slug}/unlink` reads the user's
//! links, picks the one matching the slug, and then deletes it — two separate
//! statements. `oauth_account_ops::delete_by_id` used to re-read the row and
//! delete it only `if let Some(m)`, returning `Ok(())` either way, so a second
//! concurrent unlink that found nothing left to delete still answered
//! `200 {"unlinked": true}` — a confirmation of work it did not do, on an
//! endpoint whose own OpenAPI already promises `404 No OAuth account linked`.
//!
//! What each test guards:
//!
//! * **One deletion, one confirmation.** Concurrent unlinks of the same link
//!   report exactly one `true`; every loser gets `false` and no error.
//! * **Scoping survived the rewrite.** The `user_id` filter still bounds the
//!   delete, so someone else's link is neither removed nor reported as removed.
//! * **A row that was there is reported as deleted.** The `false` branch means
//!   "nothing to delete", not "delete is broken".

use rg_db::sea_orm::{ConnectionTrait, DatabaseBackend, DatabaseConnection, Statement};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "forgekeep-oauth-unlink-{label}-{}.db",
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

/// Link `provider_user_id` to a freshly created account and hand back both ids.
async fn link(db: &DatabaseConnection, username: &str, provider_user_id: &str) -> (i64, i64) {
    let user = rg_db::ops::user_ops::create_user(
        db,
        username,
        &format!("{username}@example.com"),
        "",
        username,
    )
    .await
    .expect("create the account the link hangs off");
    let account = rg_db::ops::oauth_account_ops::upsert(
        db,
        user.id,
        "gitea",
        provider_user_id,
        username,
        &format!("{username}@example.com"),
        Some("access"),
        Some("refresh"),
        None,
    )
    .await
    .expect("link the identity");
    (user.id, account.id)
}

// Multi-threaded on purpose: the window this guards sits between the handler's
// `SELECT` of the user's links and the `DELETE` that follows it, on two
// different pooled connections.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_unlinks_of_one_link_report_exactly_one_deletion() {
    let (db, _temp) = setup("race").await;
    let (user_id, account_id) = link(&db, "sso_alice", "provider-uid-1").await;

    // Eight unlink requests for the same link, started together — the shape of
    // a double-clicked "Disconnect" button.
    let attempts = (0..8).map(|_| {
        let db = db.clone();
        async move { rg_db::ops::oauth_account_ops::delete_by_id(&db, account_id, user_id).await }
    });

    let results = futures_join_all(attempts).await;
    let mut deleted = 0;
    for (i, result) in results.iter().enumerate() {
        match result {
            Ok(true) => deleted += 1,
            Ok(false) => {}
            Err(error) => panic!("unlink {i} failed outright: {error}"),
        }
    }
    assert_eq!(
        deleted, 1,
        "exactly one of the concurrent unlinks removed the row, \
         so exactly one of them may answer 200",
    );

    assert_eq!(
        scalar(&db, "SELECT COUNT(*) AS n FROM oauth_accounts").await,
        0,
        "the link is gone once, not deleted repeatedly",
    );
}

#[tokio::test]
async fn a_link_owned_by_someone_else_is_neither_deleted_nor_reported_as_deleted() {
    let (db, _temp) = setup("scope").await;
    let (owner_id, account_id) = link(&db, "sso_bob", "uid-bob").await;
    let (stranger_id, _) = link(&db, "sso_mallory", "uid-mallory").await;

    assert!(
        !rg_db::ops::oauth_account_ops::delete_by_id(&db, account_id, stranger_id)
            .await
            .expect("a scoped delete that matches nothing is not an error"),
        "another user's link must not be reported as unlinked",
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM oauth_accounts WHERE provider_user_id = 'uid-bob'",
        )
        .await,
        1,
        "and it must still be there",
    );

    // The owner's own unlink is the one that works.
    assert!(
        rg_db::ops::oauth_account_ops::delete_by_id(&db, account_id, owner_id)
            .await
            .expect("the owner's unlink"),
        "deleting a row that exists reports the deletion",
    );
    assert_eq!(
        scalar(
            &db,
            "SELECT COUNT(*) AS n FROM oauth_accounts WHERE provider_user_id = 'uid-bob'",
        )
        .await,
        0,
    );
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
