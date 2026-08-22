//! card_149b38646429: losing to a concurrent writer is "come back", not "we broke".
//!
//! A transaction that reads before it writes is refused outright on SQLite once
//! another connection holds the database-wide write lock, and
//! `rg_db::contention::retry_transaction` exists to run it again. What the
//! retry cannot do is win against a holder that outlasts its budget — and what
//! reached the client then was a `500`, on the argument that a statement-level
//! `DbErr` is a bug. For this class the argument is simply wrong: nothing is
//! wrong with the request and nothing is wrong with the server, the database
//! was busy, and the request succeeds the moment the writer ahead of it lets
//! go. `503` says that; `500` tells the client and every proxy in front of it
//! not to bother.
//!
//! The lock here is a real one. A hand-built `DbErr` would prove only that the
//! predicate matches the shape a test author guessed: which refusal SQLite
//! produces (`SQLITE_BUSY` immediately, or `database is locked` once
//! `busy_timeout` runs out) is decided by the backend, and the classification
//! has to hold for whichever one arrives.

use std::time::Duration;

use sea_orm::{ConnectionTrait, TransactionTrait};

use crate::common::{register_full, setup_test_db_with_connections, spawn_test_app_over_db};

/// A writer parked on SQLite's single writer slot.
///
/// Deliberately an ordinary `UPDATE` rather than a second enrolment: what the
/// endpoint under test meets is the *duration* a holder can have, and this
/// reproduces it without needing a second request whose own timing would then
/// be part of the fixture.
struct HeldWriteLock {
    release: tokio::sync::oneshot::Sender<()>,
    holder: tokio::task::JoinHandle<()>,
}

impl HeldWriteLock {
    async fn take(db: &rg_db::DatabaseConnection) -> Self {
        let (held_tx, held_rx) = tokio::sync::oneshot::channel::<()>();
        let (release, release_rx) = tokio::sync::oneshot::channel::<()>();
        let holder_db = db.clone();
        let holder = tokio::spawn(async move {
            let transaction = holder_db.begin().await.expect("open the holding writer");
            transaction
                .execute_unprepared("UPDATE users SET updated_at = updated_at")
                .await
                .expect("take SQLite's write lock");
            held_tx.send(()).expect("announce the lock is held");
            release_rx.await.expect("wait to be released");
            transaction.commit().await.expect("release the write lock");
        });
        held_rx.await.expect("the writer took the lock");
        Self { release, holder }
    }

    async fn release(self) {
        self.release.send(()).expect("the holding writer went away");
        self.holder.await.expect("the holding writer finished");
    }
}

/// The TOTP secret `POST /users/mfa/setup` hands out, and the code an
/// authenticator app would be showing for it right now.
async fn enrolled_secret(base: &str, token: &str) -> String {
    let setup: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(token)
        .send()
        .await
        .expect("setup request")
        .json()
        .await
        .expect("setup response body");
    setup["secret"]
        .as_str()
        .expect("setup returned no secret")
        .to_string()
}

fn current_code(secret: &str) -> String {
    let bytes = totp_rs::Secret::Encoded(secret.to_string())
        .to_bytes()
        .expect("the secret the server handed out is not base32");
    totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        None,
        String::new(),
    )
    .expect("build the authenticator side of the handshake")
    .generate_current()
    .expect("read the current time step")
}

async fn enable(base: &str, token: &str, secret: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "code": current_code(secret) }))
        .send()
        .await
        .expect("enable request")
}

async fn mfa_enabled(db: &rg_db::DatabaseConnection, user_id: i64) -> bool {
    rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .expect("load user")
        .expect("user exists")
        .mfa_enabled
}

/// `POST /users/mfa/enable` runs its whole enrolment through
/// `retry_transaction`, so a holder that outlasts the budget is the shortest
/// real path to "the retry ran out". The status it answers is the subject.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_that_spends_its_retry_budget_on_a_busy_database_is_a_503() {
    // Four connections: the holder parks one for the whole scenario and the
    // request needs its own alongside the assertions' reads.
    let (db, _dir) = setup_test_db_with_connections(4).await;
    let base = spawn_test_app_over_db(db.clone()).await;
    let (token, user_id) = register_full(&base, "contended", "contended@example.com").await;
    let secret = enrolled_secret(&base, &token).await;

    let lock = HeldWriteLock::take(&db).await;
    let refused = enable(&base, &token, &secret).await;
    let status = refused.status();
    lock.release().await;

    assert_eq!(
        status, 503,
        "a request that lost to a concurrent writer was told its own retry is pointless — \
         it succeeds as soon as the holder commits, which is what 503 exists to say"
    );
    assert!(
        !mfa_enabled(&db, user_id).await,
        "the refused enrolment still switched the second factor on"
    );

    // The other half of the claim: nothing was wrong with the request. The same
    // one goes through once the database is free, so the 503 above described the
    // contention and not a fault this caller could have fixed.
    let accepted = tokio::time::timeout(Duration::from_secs(30), enable(&base, &token, &secret))
        .await
        .expect("the retry after the lock was released did not answer");
    assert_eq!(
        accepted.status(),
        200,
        "the request the server called a server error succeeds unchanged"
    );
    assert!(mfa_enabled(&db, user_id).await);
}
