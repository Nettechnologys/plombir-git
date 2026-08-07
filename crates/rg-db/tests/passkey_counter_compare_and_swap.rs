//! card_ae8485e5654d: a passkey login cannot roll the WebAuthn signature
//! counter back.
//!
//! `touch_and_update` used to be an unconditional `UPDATE ... WHERE id = ?`.
//! `login_finish` loads the stored credential, verifies the assertion against
//! *that* snapshot, applies `update_credential` to it and writes the result
//! back — so two concurrent logins both verify against one stored state, and
//! whichever statement runs second wins. An assertion carrying the lower
//! counter could therefore overwrite the higher one while both logins were
//! answered a token. The counter and backup state are what the *next* assertion
//! is compared against to spot a cloned or replayed credential, so the loser's
//! write silently disarms that check.
//!
//! The replacement puts the snapshot in the `WHERE`: the write lands only if
//! the row still holds the exact blob the assertion was verified against.
//!
//! What these tests guard:
//!
//! * **The interleaving is driven, not hoped for.** Two writers are held on one
//!   captured snapshot on purpose; the second one's lower counter must not
//!   land, and the row must keep the higher one.
//! * **A lost CAS is a `Conflict`, not a success.** The handler turns that into
//!   a `409` instead of a session token — a loser reported as `Stored` is the
//!   defect wearing a new signature.
//! * **A revoked credential is still distinguishable.** `Missing` and
//!   `Conflict` are different answers, and neither is a `DbErr`: the login path
//!   ends the ceremony for the first two and retries the third.
//! * **MySQL's "changed rows" does not become a phantom conflict.** Re-storing
//!   a byte-identical credential reports `Stored`, because the state the caller
//!   needed kept is kept.
//! * **Not vacuous**: an uncontested store lands and stamps `last_used_at`.

use rg_db::ops::passkey_credential_ops::{self, CounterWrite};
use rg_db::sea_orm::{DatabaseConnection, EntityTrait};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-passkey-counter-cas-{label}-{}.db",
                uuid::Uuid::new_v4().simple()
            )),
        }
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

/// A migrated database with more than one pooled connection, so racing tasks
/// really do run their statements against separate connections.
///
/// `FORGEKEEP_TEST_DATABASE_URL` points the whole file at PostgreSQL or MySQL
/// instead — "the write lands only over the snapshot it was derived from" is a
/// claim the *database* arbitrates, and MySQL in particular counts changed
/// rows rather than matched ones, so it is a claim about each backend and not
/// about SQLite. Same switch `password_reset_token_single_use` uses.
async fn setup(label: &str) -> (DatabaseConnection, Option<TempDb>, i64) {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let (url, temp) = match std::env::var("FORGEKEEP_TEST_DATABASE_URL") {
        Ok(url) if !url.is_empty() => (url, None),
        _ => {
            let temp = TempDb::new(label);
            (temp.url(), Some(temp))
        }
    };
    let db = rg_db::connect_with_pool(&url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 4)
        .await
        .expect("connect to throwaway database");
    rg_db::run_migrations(&db).await.expect("run migrations");
    let user = rg_db::ops::user_ops::create_user(
        &db,
        &format!("rune{label}{suffix}"),
        &format!("rune{label}{suffix}@example.invalid"),
        "",
        "Rune",
    )
    .await
    .expect("create the account the credential belongs to");
    (db, temp, user.id)
}

/// The credential blob is opaque to `touch_and_update` — it stores whatever the
/// handler hands it — so a counter-shaped marker is enough to prove *which*
/// value landed in the column.
fn blob(counter: u32) -> String {
    format!("{{\"counter\":{counter}}}")
}

/// Register one passkey and hand back its row id. The stored blob starts at the
/// snapshot both racing logins will have loaded.
async fn register(db: &DatabaseConnection, user_id: i64, label: &str, snapshot: &str) -> i64 {
    passkey_credential_ops::create(
        db,
        user_id,
        &format!("credential-{label}-{}", uuid::Uuid::new_v4().simple()),
        snapshot,
        "yubikey",
        "passkeys.example.test",
    )
    .await
    .expect("register the fixture passkey")
    .id
}

/// The blob currently stored for a credential.
async fn stored_blob(db: &DatabaseConnection, id: i64) -> String {
    passkey_credential_ops::Entity::find_by_id(id)
        .one(db)
        .await
        .expect("reload the credential")
        .expect("the credential is still registered")
        .passkey
}

/// The interleaving the card is about, driven rather than hoped for: both
/// writers hold the same captured snapshot, and the one that gets there second
/// is carrying the *lower* counter.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lower_counter_cannot_overwrite_a_higher_one_from_the_same_snapshot() {
    let (db, _temp, user_id) = setup("rollback").await;
    let snapshot = blob(1);
    let passkey_id = register(&db, user_id, "rollback", &snapshot).await;

    // Login A: verified against `snapshot`, its authenticator reported 9.
    let winner = passkey_credential_ops::touch_and_update(&db, passkey_id, &snapshot, &blob(9))
        .await
        .expect("store the advanced counter");
    assert_eq!(
        winner,
        CounterWrite::Stored,
        "the first writer holds the snapshot the row still has, so its write must land"
    );

    // Login B: started earlier, verified against the *same* `snapshot`, and its
    // authenticator reported 2. Nothing about this call knows that A has
    // already advanced the row — which is exactly the state the old code wrote
    // straight through.
    let loser = passkey_credential_ops::touch_and_update(&db, passkey_id, &snapshot, &blob(2))
        .await
        .expect("a lost compare-and-swap is an answer, not an error");
    assert_eq!(
        loser,
        CounterWrite::Conflict,
        "a writer whose snapshot is stale must lose, not overwrite"
    );

    assert_eq!(
        stored_blob(&db, passkey_id).await,
        blob(9),
        "the stored counter must still be the higher one the winner reported"
    );
}

/// How many logins arrive at the same instant, all holding the one snapshot.
const CALLERS: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_assertions_on_one_snapshot_leave_exactly_one_winner() {
    let (db, _temp, user_id) = setup("race").await;
    let snapshot = blob(1);
    let passkey_id = register(&db, user_id, "race", &snapshot).await;

    let attempts = (0..CALLERS).map(|caller| {
        let db = db.clone();
        let snapshot = snapshot.clone();
        // Every caller writes a distinct value, so "exactly one winner" is a
        // claim about the row's content and not only about the counters.
        let advanced = blob(10 + caller as u32);
        async move {
            passkey_credential_ops::touch_and_update(&db, passkey_id, &snapshot, &advanced)
                .await
                .map(|outcome| (caller, outcome, advanced))
        }
    });

    let results = join_all(attempts).await;

    // Losing the swap is an ordinary outcome the handler answers with a `409`.
    // A `DbErr` here would be a `500` blaming the server for a race it handled.
    for (caller, result) in results.iter().enumerate() {
        assert!(
            result.is_ok(),
            "caller {caller} lost the race and got an error instead of a conflict: {:?}",
            result.as_ref().err()
        );
    }

    let winners: Vec<(usize, String)> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .filter(|(_, outcome, _)| *outcome == CounterWrite::Stored)
        .map(|(caller, _, advanced)| (*caller, advanced.clone()))
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "one stored snapshot may be advanced by exactly one assertion, got {winners:?}"
    );
    assert_eq!(
        stored_blob(&db, passkey_id).await,
        winners[0].1,
        "the credential the row holds must be the one whose caller was told it landed"
    );
}

/// The three answers the login path branches on have to stay three answers. A
/// revoked credential ends the ceremony (`401`), a lost swap is retryable
/// (`409`), and neither is the retryable outage a `DbErr` stands for (`503`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_revoked_credential_and_a_lost_swap_are_different_answers() {
    let (db, _temp, user_id) = setup("answers").await;
    let snapshot = blob(1);
    let passkey_id = register(&db, user_id, "answers", &snapshot).await;

    // Someone else advanced the row: the snapshot is stale but the credential
    // is still registered.
    let advanced = passkey_credential_ops::touch_and_update(&db, passkey_id, &snapshot, &blob(4))
        .await
        .expect("advance the credential once");
    assert_eq!(advanced, CounterWrite::Stored);
    assert_eq!(
        passkey_credential_ops::touch_and_update(&db, passkey_id, &snapshot, &blob(3))
            .await
            .expect("a stale snapshot is a conflict, not an error"),
        CounterWrite::Conflict,
    );

    // Revoked mid-ceremony: there is no row to compare against at all.
    assert!(
        passkey_credential_ops::delete(&db, user_id, passkey_id)
            .await
            .expect("revoke the fixture passkey"),
        "the fixture passkey must have been registered",
    );
    assert_eq!(
        passkey_credential_ops::touch_and_update(&db, passkey_id, &blob(4), &blob(5))
            .await
            .expect("a vanished row is an answer, not an error"),
        CounterWrite::Missing,
        "a credential that is no longer registered must report absence, not a lost race",
    );
}

/// Non-vacuity, and the trap the row count alone would fall into: MySQL counts
/// *changed* rows, so a credential re-stored byte-identically can report zero
/// while nothing has been lost. That is a `Stored`, not a `Conflict` — the
/// counter-less authenticators that always report `0` take this path on every
/// single login, and a `409` there would break them outright.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_uncontested_store_lands_and_an_identical_restore_is_not_a_conflict() {
    let (db, _temp, user_id) = setup("identical").await;
    let snapshot = blob(1);
    let passkey_id = register(&db, user_id, "identical", &snapshot).await;

    assert_eq!(
        passkey_credential_ops::touch_and_update(&db, passkey_id, &snapshot, &blob(2))
            .await
            .expect("store the advanced counter"),
        CounterWrite::Stored,
        "an uncontested store on the snapshot the row holds must land",
    );
    assert_eq!(stored_blob(&db, passkey_id).await, blob(2));

    let row = passkey_credential_ops::Entity::find_by_id(passkey_id)
        .one(&db)
        .await
        .expect("reload the credential")
        .expect("the credential is still registered");
    assert!(
        row.last_used_at.is_some(),
        "a successful assertion must stamp last_used_at"
    );
    assert_eq!(
        row.rp_id.as_deref(),
        Some("passkeys.example.test"),
        "advancing the counter must not lose the credential's relying-party id"
    );

    // Same expected snapshot, same value written: an authenticator whose
    // counter does not move.
    assert_eq!(
        passkey_credential_ops::touch_and_update(&db, passkey_id, &blob(2), &blob(2))
            .await
            .expect("a no-op store is an answer, not an error"),
        CounterWrite::Stored,
        "re-storing the credential the row already holds must not be reported as a conflict",
    );
}

/// `futures::future::join_all` without taking a dependency on `futures` for one
/// call: poll the futures together by handing them to the runtime as tasks.
async fn join_all<F>(futures: impl IntoIterator<Item = F>) -> Vec<F::Output>
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
