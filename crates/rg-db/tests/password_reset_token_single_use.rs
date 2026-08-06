//! card_dca5ca190649: a reset link is spent by one caller or by nobody.
//!
//! `reset_password` read the token with `find_by_hash`, decided `!used &&
//! not expired` in application memory, and only then — a full Argon2 pass
//! later — called `mark_used`, which filtered on the id alone and threw away
//! `rows_affected`. Two requests carrying the same link both passed that check,
//! both wrote `users.password_hash`, both revoked the sessions, and both were
//! answered `200`. Only the later write survived, so the loser walked away with
//! a session and a password the account does not have.
//!
//! `consume` is the replacement: one conditional statement whose `WHERE` names
//! the state the caller believed it was acting on, and whose row count is the
//! answer.
//!
//! What these tests guard:
//!
//! * **One spend wins.** Eight callers racing one live link produce exactly one
//!   `true`, and the row is spent once.
//! * **The refusal is a `false`, not an error.** The loser has to be answerable
//!   as an ordinary holder of a dead link, which is a `400`, not a `500`.
//! * **The gate does not lean on the caller.** An expired link is refused by
//!   `consume` itself, without anyone having checked the clock first.
//! * **Not vacuous**: an uncontested spend on a live link still lands.

use rg_db::sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-reset-token-single-use-{label}-{}.db",
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

/// A migrated database with more than one pooled connection, so the racing
/// tasks really do run their statements against separate connections.
///
/// `FORGEKEEP_TEST_DATABASE_URL` points the whole file at PostgreSQL or MySQL
/// instead — the claim is a statement the *database* arbitrates, so "exactly
/// one winner" is a claim about each backend and not about SQLite. Same switch
/// `multi_backend_smoke` uses. The account name carries a uuid so repeated runs
/// against one server database do not collide.
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
        &format!("dana{label}{suffix}"),
        &format!("dana{label}{suffix}@example.com"),
        "",
        "Dana",
    )
    .await
    .expect("create the account the link belongs to");
    (db, temp, user.id)
}

/// Plant a link with the given lifetime and hand back its row id.
async fn issue(db: &DatabaseConnection, user_id: i64, label: &str, minutes: i64) -> i64 {
    rg_db::ops::password_reset_token_ops::create(
        db,
        user_id,
        &format!("hash-{label}-{}", uuid::Uuid::new_v4().simple()),
        chrono::Utc::now() + chrono::Duration::minutes(minutes),
    )
    .await
    .expect("issue a reset link")
    .id
}

/// Whether the stored row is marked spent.
async fn is_spent(db: &DatabaseConnection, token_id: i64) -> bool {
    rg_db::entities::password_reset_token::Entity::find()
        .filter(rg_db::entities::password_reset_token::Column::Id.eq(token_id))
        .one(db)
        .await
        .expect("reload the link")
        .expect("the link still exists")
        .used
}

/// How many requests arrive at the same instant. Enough that several of them
/// read the same live row before any of them has written to it.
const CALLERS: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_spends_of_one_reset_link_leave_exactly_one_winner() {
    let (db, _temp, user_id) = setup("race").await;
    let token_id = issue(&db, user_id, "race", 15).await;

    let attempts = (0..CALLERS).map(|caller| {
        let db = db.clone();
        async move {
            rg_db::ops::password_reset_token_ops::consume(&db, token_id)
                .await
                .map(|spent| (caller, spent))
        }
    });

    let results = join_all(attempts).await;

    // A refused spend is an ordinary outcome, not a failure: the caller that
    // lost has to be answerable with the same `400` an expired link gets, and a
    // `DbErr` here would surface as a `500` instead.
    for (caller, result) in results.iter().enumerate() {
        assert!(
            result.is_ok(),
            "caller {caller} lost the race and got an error instead of a refusal: {:?}",
            result.as_ref().err()
        );
    }

    let winners: Vec<usize> = results
        .iter()
        .filter_map(|result| result.as_ref().ok())
        .filter(|(_, spent)| *spent)
        .map(|(caller, _)| *caller)
        .collect();
    assert_eq!(
        winners.len(),
        1,
        "one reset link may be spent by exactly one caller, got {winners:?}"
    );
    assert!(
        is_spent(&db, token_id).await,
        "the winner was told it spent the link, but the row is still unspent"
    );
}

/// Non-vacuity, plus the two states the caller used to check on its own: the
/// statement has to refuse a spent link and an expired one by itself, or the
/// gate is still a convention that every future call site must remember.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_spend_lands_and_spent_or_expired_links_are_refused() {
    let (db, _temp, user_id) = setup("states").await;

    let live = issue(&db, user_id, "live", 15).await;
    assert!(
        rg_db::ops::password_reset_token_ops::consume(&db, live)
            .await
            .expect("spend an untouched live link"),
        "an uncontested spend on a live link must land",
    );
    assert!(
        !rg_db::ops::password_reset_token_ops::consume(&db, live)
            .await
            .expect("a second spend is a refusal, not an error"),
        "a link that is already spent must not be spendable again",
    );

    // Expired, and nobody checked the clock before calling.
    let stale = issue(&db, user_id, "stale", -1).await;
    assert!(
        !rg_db::ops::password_reset_token_ops::consume(&db, stale)
            .await
            .expect("spending an expired link is a refusal, not an error"),
        "an expired link must be refused by the statement itself",
    );
    assert!(
        !is_spent(&db, stale).await,
        "a refused spend must not mark the row",
    );

    assert!(
        !rg_db::ops::password_reset_token_ops::consume(&db, live + 10_000)
            .await
            .expect("spending a link that does not exist is a refusal, not an error"),
        "a link that no longer exists must be refused",
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
