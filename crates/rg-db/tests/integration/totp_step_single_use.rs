//! card_9585caf5692d: one TOTP time step passes the second factor once.
//!
//! `verify_code` is a pure function of the shared secret and the clock, so a
//! successful check used to consume nothing — there was no column to consume.
//! With `skew = 1` over a 30-second step, one code is accepted for the previous,
//! current and next step, so an intercepted code passed the second factor for
//! around 90 seconds, as many times as it was presented, and two concurrent
//! `POST /users/mfa/verify` carrying it were answered two sessions. RFC 6238
//! §5.2 requires the opposite: "the verifier MUST NOT accept the second attempt
//! of the OTP after the successful validation has been issued for the first
//! OTP".
//!
//! The answer is `users.totp_last_step` plus one conditional statement whose
//! `WHERE` names the state the caller believed it was acting on, and whose row
//! count is the verdict — the same shape `mfa_backup_code_ops::verify_and_consume`
//! and `password_reset_token_ops::consume` took for the same reason. The step is
//! passed in explicitly here rather than derived from `now()`, so every claim
//! below is about the statement and not about when the test happened to run.
//!
//! What these tests guard:
//!
//! * **One spend wins.** Eight callers racing one step produce exactly one
//!   `true`, and the row ends up holding that step.
//! * **A replay is refused.** Spending the same step twice is a `false`.
//! * **Monotonic, not merely different.** After step *n* is spent, *n-1* — still
//!   inside the skew window, and the code an attacker replaying a slightly stale
//!   interception would carry — is refused too.
//! * **Clock skew survives.** A *newer* step that nobody has spent is still
//!   accepted, so tolerating a drifting authenticator is not the price of
//!   single use.
//! * **The refusal is a `false`, not an error.** A loser has to be answerable
//!   with the same `401 invalid TOTP code` a wrong code gets, not a `500`.
//! * **Scoped to one account.** Spending a step for one user leaves another
//!   user's column untouched, and an account that does not exist is a refusal.

use rg_db::entities::user;
use rg_db::ops::user_ops;
use rg_db::sea_orm::DatabaseConnection;

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "plombir-git-totp-step-single-use-{label}-{}.db",
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
/// instead — "exactly one caller spends the step" is a claim the *database*
/// arbitrates, so it is a claim about each backend and not about SQLite. Same
/// switch `mfa_backup_code_single_use` and `passkey_counter_compare_and_swap`
/// use. The account names carry a uuid so repeated runs against one server
/// database do not collide.
async fn setup(label: &str) -> (DatabaseConnection, Option<TempDb>, i64) {
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
    let user_id = account(&db, "iris").await;
    (db, temp, user_id)
}

/// A fresh account in the given database, whose TOTP step is still unspent.
async fn account(db: &DatabaseConnection, name: &str) -> i64 {
    let suffix = uuid::Uuid::new_v4().simple().to_string();
    let suffix = &suffix[..10];
    user_ops::create_user(
        db,
        &format!("{name}{suffix}"),
        &format!("{name}{suffix}@example.invalid"),
        "",
        name,
    )
    .await
    .expect("create the account the step belongs to")
    .id
}

/// The step the row currently records, or `None` for an account that has never
/// completed a TOTP login.
async fn stored_step(db: &DatabaseConnection, user_id: i64) -> Option<i64> {
    user_ops::find_by_id(db, user_id)
        .await
        .expect("reload the account")
        .expect("the account still exists")
        .totp_last_step
}

/// A step number in the range a real clock produces, so the `BIGINT` column is
/// exercised with the value it will actually hold rather than a small integer.
const STEP: u64 = 1_777_777_777 / 30;

/// How many requests arrive at the same instant. Enough that several of them
/// read the same unspent row before any of them has written to it.
const CALLERS: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_uses_of_one_totp_step_leave_exactly_one_winner() {
    let (db, _temp, user_id) = setup("race").await;

    let attempts = (0..CALLERS).map(|caller| {
        let db = db.clone();
        async move {
            user_ops::consume_totp_step(&db, user_id, STEP)
                .await
                .map(|spent| (caller, spent))
        }
    });

    let results = join_all(attempts).await;

    // A refused spend is an ordinary outcome, not a failure: the caller that
    // lost is answered the same `401 invalid TOTP code` a wrong code gets, and a
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
        "one TOTP code may pass the second factor exactly once, got {winners:?}"
    );

    assert_eq!(
        stored_step(&db, user_id).await,
        Some(STEP as i64),
        "the winner was told it spent the step, but the row does not record it"
    );
}

/// The three states the gate has to arbitrate on its own, in the order a replay
/// actually arrives in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spent_step_is_refused_and_a_newer_one_is_still_accepted() {
    let (db, _temp, user_id) = setup("states").await;

    assert_eq!(
        stored_step(&db, user_id).await,
        None,
        "a fresh account must start with no spent step"
    );
    assert!(
        user_ops::consume_totp_step(&db, user_id, STEP)
            .await
            .expect("spend an unspent step"),
        "an uncontested first spend must land",
    );
    assert_eq!(stored_step(&db, user_id).await, Some(STEP as i64));

    assert!(
        !user_ops::consume_totp_step(&db, user_id, STEP)
            .await
            .expect("a replay is a refusal, not an error"),
        "the same code must not pass the second factor twice",
    );

    // `skew = 1` keeps the previous step valid for another 30 seconds, which is
    // exactly the code a replay of a slightly stale interception carries. A `!=`
    // comparison would wave it through.
    assert!(
        !user_ops::consume_totp_step(&db, user_id, STEP - 1)
            .await
            .expect("an older step is a refusal, not an error"),
        "a step older than the one already spent must be refused",
    );
    assert_eq!(
        stored_step(&db, user_id).await,
        Some(STEP as i64),
        "a refused spend must not move the recorded step"
    );

    // The other half of the same window: an authenticator running slightly fast
    // presents the *next* step, nobody has spent it, and single use must not be
    // the reason an honest login is refused.
    assert!(
        user_ops::consume_totp_step(&db, user_id, STEP + 1)
            .await
            .expect("spend the next step"),
        "a newer, unspent step must still be accepted",
    );
    assert_eq!(stored_step(&db, user_id).await, Some((STEP + 1) as i64));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn spending_a_step_is_scoped_to_one_account() {
    let (db, _temp, user_id) = setup("scope").await;
    let other_id = account(&db, "noel").await;

    assert!(user_ops::consume_totp_step(&db, user_id, STEP)
        .await
        .expect("spend the first account's step"));

    assert_eq!(
        stored_step(&db, other_id).await,
        None,
        "spending one account's step marked another account's row"
    );
    assert!(
        user_ops::consume_totp_step(&db, other_id, STEP)
            .await
            .expect("spend the second account's step"),
        "the same step must still be spendable by a different account",
    );

    // An account that does not exist is a refusal for the same reason an
    // already-spent step is: the caller answers both with `401`.
    assert!(
        !user_ops::consume_totp_step(&db, i64::MAX, STEP)
            .await
            .expect("an unknown account is a refusal, not an error"),
        "a step must not be spendable for an account that does not exist",
    );

    // Non-vacuity of the column itself: the entity really reads back what the
    // statement wrote, on every backend this file runs against.
    assert_eq!(
        (
            stored_step(&db, user_id).await,
            stored_step(&db, other_id).await
        ),
        (Some(STEP as i64), Some(STEP as i64)),
    );
    let _ = user::Column::TotpLastStep;
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
