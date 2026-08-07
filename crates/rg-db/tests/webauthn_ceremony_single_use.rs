//! card_7c7ada2d6a72: one WebAuthn ceremony answers its challenge once.
//!
//! `unseal_state` is a pure function of the signing key and the clock, so a
//! completed passkey ceremony used to consume nothing — there was nothing to
//! consume. An intercepted `POST /users/passkeys/login/finish` (the ceremony
//! cookie plus the assertion body, exactly the pair a phishing proxy sees) was
//! therefore answered a session as many times as it was presented, for the
//! whole 300-second life of the cookie. Single use is the protection WebAuthn
//! actually defines against that; a short expiry is not a substitute for it.
//!
//! The signature-counter compare-and-swap (`passkey_credential_ops::
//! touch_and_update`) closes only the half where the counter moved. Platform
//! passkeys — iCloud Keychain and the like — report `signCount = 0` forever, so
//! a replay writes back a byte-identical credential and gets an honest
//! `Stored`. On the most widespread authenticator there is, the replay went
//! through.
//!
//! The answer is `webauthn_ceremony_spend` plus one statement whose row is the
//! verdict — the shape `mfa_backup_code_ops::verify_and_consume`,
//! `password_reset_token_ops::consume` and `user_ops::consume_totp_step` took,
//! for the same reason. The conditional lives in a UNIQUE index rather than a
//! `WHERE`, because there is no prior row to name: the statement that creates
//! one is the statement that arbitrates.
//!
//! What these tests guard:
//!
//! * **One spend wins.** Eight callers racing one ceremony produce exactly one
//!   `true`, and one row.
//! * **A replay is refused.** Answering the same ceremony twice is a `false`,
//!   including when an unrelated ceremony was answered in between — the case a
//!   single "last challenge" column would wave through.
//! * **The refusal is a `false`, not an error.** The loser has to be answerable
//!   with the same `401 passkey authentication failed` a bad signature gets, so
//!   a replay cannot learn that the material it carried was genuine.
//! * **An honest second login still passes.** Single use must not be the reason
//!   a new ceremony is refused.
//! * **The record outlives the challenge, and no longer.** A live spend
//!   survives the sweep the next spend performs; an expired one does not.

use rg_db::ops::webauthn_ceremony_ops;
use rg_db::sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, PaginatorTrait, QueryFilter};

/// A throwaway SQLite database file, removed with its WAL siblings on drop.
struct TempDb {
    path: std::path::PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        Self {
            path: std::env::temp_dir().join(format!(
                "forgekeep-webauthn-ceremony-single-use-{label}-{}.db",
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
/// instead — "exactly one caller spends the challenge" is a claim the
/// *database* arbitrates, so it is a claim about each backend and not about
/// SQLite. Same switch `totp_step_single_use` and
/// `passkey_counter_compare_and_swap` use.
async fn setup(label: &str) -> (DatabaseConnection, Option<TempDb>) {
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
    (db, temp)
}

/// A ceremony id shaped like the real thing, unique per call so repeated runs
/// against one server database do not collide.
fn ceremony(label: &str) -> String {
    format!("{label}-{}", uuid::Uuid::new_v4().simple())
}

/// The retention a live ceremony is spent with: comfortably past the point its
/// cookie stops unsealing, exactly as `rg-http` passes it.
fn live() -> chrono::DateTime<chrono::Utc> {
    chrono::Utc::now() + chrono::Duration::seconds(480)
}

async fn spend_count(db: &DatabaseConnection, ceremony_id: &str) -> u64 {
    webauthn_ceremony_ops::Entity::find()
        .filter(rg_db::entities::webauthn_ceremony_spend::Column::CeremonyId.eq(ceremony_id))
        .count(db)
        .await
        .expect("count the spend records of one ceremony")
}

/// How many replays arrive at the same instant. Enough that several of them
/// reach the insert before any of them has committed one.
const CALLERS: usize = 8;

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn concurrent_replays_of_one_ceremony_leave_exactly_one_winner() {
    let (db, _temp) = setup("race").await;
    let id = ceremony("race");

    let attempts = (0..CALLERS).map(|caller| {
        let db = db.clone();
        let id = id.clone();
        async move {
            webauthn_ceremony_ops::spend(&db, &id, live())
                .await
                .map(|spent| (caller, spent))
        }
    });

    let results = join_all(attempts).await;

    // A refused spend is an ordinary outcome, not a failure: the caller that
    // lost is answered the same `401 passkey authentication failed` a bad
    // signature gets, and a `DbErr` here would surface as a `500` instead.
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
        "one passkey assertion may be answered exactly once, got {winners:?}"
    );
    assert_eq!(
        spend_count(&db, &id).await,
        1,
        "the winner was told it spent the challenge, but the ledger does not record exactly one spend"
    );
}

/// The three states the gate has to arbitrate on its own, in the order a replay
/// actually arrives in.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_spent_ceremony_is_refused_and_a_fresh_one_is_still_accepted() {
    let (db, _temp) = setup("states").await;
    let first = ceremony("first");
    let second = ceremony("second");

    assert!(
        webauthn_ceremony_ops::spend(&db, &first, live())
            .await
            .expect("spend an unanswered ceremony"),
        "an uncontested first spend must land",
    );
    assert!(
        !webauthn_ceremony_ops::spend(&db, &first, live())
            .await
            .expect("a replay is a refusal, not an error"),
        "one challenge must not be answered twice",
    );

    // An honest second login: single use must not be the reason it fails.
    assert!(
        webauthn_ceremony_ops::spend(&db, &second, live())
            .await
            .expect("spend a fresh ceremony"),
        "a new ceremony carries a new challenge and must still be spendable",
    );

    // The case that decides the design. A single "last challenge" column would
    // now hold `second`, so replaying `first` would no longer collide with
    // anything and would be waved through. The window a replay actually uses is
    // the whole TTL, not just the instant before the next honest login.
    assert!(
        !webauthn_ceremony_ops::spend(&db, &first, live())
            .await
            .expect("an out-of-order replay is a refusal, not an error"),
        "a challenge answered before a newer one must stay spent",
    );
    assert_eq!(
        spend_count(&db, &first).await,
        1,
        "a refused replay must not add a second record"
    );
}

/// The retention contract, from both sides. A record dropped while its cookie
/// still unseals is the replay window reopening; a record kept forever is a
/// table that only grows.
#[tokio::test]
async fn the_record_outlives_the_challenge_and_no_longer() {
    let (db, _temp) = setup("retention").await;
    let live_ceremony = ceremony("live");
    let dead_ceremony = ceremony("dead");

    assert!(webauthn_ceremony_ops::spend(&db, &live_ceremony, live())
        .await
        .expect("spend a live ceremony"),);
    // A ceremony whose cookie stopped unsealing minutes ago: nothing it could
    // still let through, so the record has no further work to do.
    assert!(webauthn_ceremony_ops::spend(
        &db,
        &dead_ceremony,
        chrono::Utc::now() - chrono::Duration::seconds(1)
    )
    .await
    .expect("spend a ceremony that is already past its retention"),);

    let swept = webauthn_ceremony_ops::delete_expired(&db)
        .await
        .expect("sweep expired spend records");
    assert_eq!(swept, 1, "the sweep took the wrong number of records");
    assert_eq!(
        spend_count(&db, &dead_ceremony).await,
        0,
        "a record whose ceremony can no longer unseal was kept"
    );
    assert_eq!(
        spend_count(&db, &live_ceremony).await,
        1,
        "the sweep dropped a record whose challenge is still live — the replay window is open again"
    );

    // And the live one is still refused after a sweep has run, which is the
    // property the count above stands for.
    assert!(
        !webauthn_ceremony_ops::spend(&db, &live_ceremony, live())
            .await
            .expect("a replay after a sweep is a refusal, not an error"),
        "a swept table must still refuse a challenge that has been answered",
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
