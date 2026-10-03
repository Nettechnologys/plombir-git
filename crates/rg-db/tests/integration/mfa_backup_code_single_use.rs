//! card_cf1d31e78b17: one MFA backup code passes the second factor once.
//!
//! `verify_and_consume` used to read the row with `find().filter(Used.eq(false))
//! .one(db)`, decide "still unspent" in application memory, and only then issue
//! a separate `UPDATE` through the same pool. Nothing held the row between the
//! two statements, so two `POST /users/mfa/verify` carrying the same code both
//! found it unspent, both marked it used, and both were answered a session. A
//! single-use recovery credential that passes the second factor twice is the
//! whole defect — the `used` column exists precisely to stop that.
//!
//! The replacement is one conditional statement whose `WHERE` names the state
//! the caller believed it was acting on, and whose row count is the answer —
//! the same shape `password_reset_token_ops::consume` took for the same reason.
//!
//! What these tests guard:
//!
//! * **One spend wins.** Eight callers racing one live code produce exactly one
//!   `true`, and the row ends up spent once.
//! * **The winner is the one that is stamped.** `used_at` must be set, so the
//!   audit answer matches the verdict that was handed out.
//! * **The refusal is a `false`, not an error.** A loser has to be answerable
//!   with the same `401 invalid backup code` an unknown code gets, not a `500`.
//! * **The gate does not lean on the caller.** An already-spent code and a code
//!   belonging to a different account are refused by the statement itself.
//! * **Not vacuous**: an uncontested spend on a live code still lands.

use rg_db::ops::mfa_backup_code_ops::{self, hash_code};
use rg_db::sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-mfa-backup-single-use-{label}-{}.db",
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
/// `PLOMBIR_GIT_TEST_DATABASE_URL` points the whole file at PostgreSQL or MySQL
/// instead — "exactly one caller spends the code" is a claim the *database*
/// arbitrates, so it is a claim about each backend and not about SQLite. Same
/// switch `password_reset_token_single_use` and `multi_backend_smoke` use. The
/// account names carry a uuid so repeated runs against one server database do
/// not collide.
async fn setup(label: &str) -> (DatabaseConnection, Option<TempDb>, i64) {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    let (url, temp) = match std::env::var("PLOMBIR_GIT_TEST_DATABASE_URL") {
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
        &format!("mira{label}{suffix}"),
        &format!("mira{label}{suffix}@example.invalid"),
        "",
        "Mira",
    )
    .await
    .expect("create the account the codes belong to");
    (db, temp, user.id)
}

/// A second account in the *same* database, for the cross-account check.
async fn second_account(db: &DatabaseConnection) -> i64 {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    rg_db::ops::user_ops::create_user(
        db,
        &format!("noel{suffix}"),
        &format!("noel{suffix}@example.invalid"),
        "",
        "Noel",
    )
    .await
    .expect("create the second account")
    .id
}

/// Enrol one distinguishable code for a user and hand the plaintext back.
async fn issue(db: &DatabaseConnection, user_id: i64, label: &str) -> String {
    let code = format!("{label}{}", uuid::Uuid::new_v4().simple());
    mfa_backup_code_ops::set_codes(db, user_id, std::slice::from_ref(&code))
        .await
        .expect("enrol a backup code");
    code
}

/// The stored row for a code, as `(used, used_at.is_some())`.
async fn state(db: &DatabaseConnection, user_id: i64, code: &str) -> (bool, bool) {
    let row = mfa_backup_code_ops::Entity::find()
        .filter(rg_db::entities::mfa_backup_code::Column::UserId.eq(user_id))
        .filter(rg_db::entities::mfa_backup_code::Column::CodeHash.eq(hash_code(code)))
        .one(db)
        .await
        .expect("reload the backup code")
        .expect("the code is still enrolled");
    (row.used, row.used_at.is_some())
}

/// How many requests arrive at the same instant. Enough that several of them
/// read the same live row before any of them has written to it.
const CALLERS: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_uses_of_one_backup_code_leave_exactly_one_winner() {
    let (db, _temp, user_id) = setup("race").await;
    let code = issue(&db, user_id, "RACE").await;

    let attempts = (0..CALLERS).map(|caller| {
        let db = db.clone();
        let code = code.clone();
        async move {
            mfa_backup_code_ops::verify_and_consume(&db, user_id, &code)
                .await
                .map(|spent| (caller, spent))
        }
    });

    let results = join_all(attempts).await;

    // A refused spend is an ordinary outcome, not a failure: the caller that
    // lost is answered the same `401 invalid backup code` an unknown code gets,
    // and a `DbErr` here would surface as a `500` instead.
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
        "one backup code may pass the second factor exactly once, got {winners:?}"
    );

    let (used, stamped) = state(&db, user_id, &code).await;
    assert!(
        used,
        "the winner was told it spent the code, but the row is still unspent"
    );
    assert!(
        stamped,
        "the spend that was handed out has to be the spend that is recorded: used_at is unset"
    );
}

/// Non-vacuity, plus the two states the caller used to check on its own. The
/// statement has to refuse an already-spent code and another account's code by
/// itself, or the gate is still a convention every future call site must
/// remember.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_first_use_lands_and_spent_or_foreign_codes_are_refused() {
    let (db, _temp, user_id) = setup("states").await;
    let code = issue(&db, user_id, "LIVE").await;

    assert!(
        mfa_backup_code_ops::verify_and_consume(&db, user_id, &code)
            .await
            .expect("spend an untouched live code"),
        "an uncontested spend on a live code must land",
    );
    let (used, stamped) = state(&db, user_id, &code).await;
    assert!(
        used && stamped,
        "the first spend must mark and stamp the row"
    );

    assert!(
        !mfa_backup_code_ops::verify_and_consume(&db, user_id, &code)
            .await
            .expect("a second spend is a refusal, not an error"),
        "a code that is already spent must not be spendable again",
    );

    assert!(
        !mfa_backup_code_ops::verify_and_consume(&db, user_id, "NOSUCHCODE")
            .await
            .expect("an unknown code is a refusal, not an error"),
        "a code that was never enrolled must be refused",
    );

    // The code is live — for its owner. Scoping is part of the same statement,
    // so a code cannot pass the second factor for the wrong account.
    let other_id = second_account(&db).await;
    let foreign = issue(&db, other_id, "MINE").await;
    assert!(
        !mfa_backup_code_ops::verify_and_consume(&db, user_id, &foreign)
            .await
            .expect("another account's code is a refusal, not an error"),
        "a backup code must only be spendable by the account it was issued to",
    );
    let (foreign_used, _) = state(&db, other_id, &foreign).await;
    assert!(
        !foreign_used,
        "a refused spend must not mark the other account's row",
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
