//! `POST /api/v1/users/login` must not answer "does this account exist?".
//!
//! The response *body* has always been unified to `invalid credentials`, but a
//! login for a real account paid for an Argon2 verification (tens of
//! milliseconds, on purpose) while an unknown username was rejected the moment
//! the database lookup came back empty. That difference is an order of
//! magnitude above network noise, which made a dictionary sweep over usernames
//! a perfectly good enumeration oracle.

use super::common::{register_user, spawn_test_app};

const PASSWORD: &str = "Qz7$wRtm";

/// One failed login, returning the status and the body.
async fn failed_login(
    client: &reqwest::Client,
    base: &str,
    login: &str,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = client
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({"login": login, "password": "definitely-not-the-password"}))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    let mut body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(status, 401, "expected a rejection for '{login}': {body}");
    // Per-request correlation id — noise for a body comparison.
    if let Some(error) = body.get_mut("error").and_then(|e| e.as_object_mut()) {
        error.remove("request_id");
    }
    (status, body)
}

#[tokio::test]
async fn login_rejects_unknown_and_known_accounts_with_the_same_answer() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    register_user(&base, "enum_known", "enum_known@example.com", PASSWORD).await;

    let (known_status, known_body) = failed_login(&client, &base, "enum_known").await;
    let (unknown_status, unknown_body) = failed_login(&client, &base, "enum_nobody").await;

    assert_eq!(known_status, unknown_status);
    assert_eq!(
        known_body, unknown_body,
        "the rejection body distinguishes an existing account from a missing one"
    );
}

/// Drive an account into its brute-force lock without going through a door.
async fn lock(db: &rg_db::DatabaseConnection, user_id: i64) {
    for _ in 0..rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS {
        rg_db::ops::user_ops::record_failed_login(
            db,
            user_id,
            rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS,
        )
        .await
        .expect("advance the brute-force counter");
    }
}

fn without_request_id(mut body: serde_json::Value) -> serde_json::Value {
    if let Some(error) = body.get_mut("error").and_then(|e| e.as_object_mut()) {
        error.remove("request_id");
    }
    body
}

/// The lock must not bring the oracle back through its own words. It used to
/// be read before the password and answered "account is temporarily locked",
/// so five requests against a guessed name confirmed it without a password.
/// It still refuses the right password — and says no more than a name nobody
/// owns, whichever password was sent.
#[tokio::test]
async fn a_locked_account_answers_like_a_missing_one() {
    let (base, db) = super::common::spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (_token, user_id) =
        super::common::register_full(&base, "enum_locked", "enum_locked@example.com").await;
    lock(&db, user_id).await;
    let locked_until = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .unwrap()
        .unwrap()
        .locked_until;

    let right = client
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({"login": "enum_locked", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(right.status(), 401, "the lock refuses the right password");
    let right = without_request_id(right.json().await.unwrap());
    let (_, wrong) = failed_login(&client, &base, "enum_locked").await;
    let (_, unknown) = failed_login(&client, &base, "enum_nobody").await;
    assert_eq!(right, unknown, "the right password names the lock");
    assert_eq!(wrong, unknown, "a wrong password names the lock");

    // The owner's correct retry is not a strike: the lock is not extended.
    let after = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .unwrap()
        .unwrap();
    assert!(after.locked_until.is_some());
    assert!(
        after.locked_until <= locked_until.map(|until| until + chrono::Duration::seconds(1)),
        "a correct password extended the lock"
    );
}

/// `POST /users/password/initial` is anonymous as well, and it worded the
/// lock apart from a wrong password whatever password was sent.
#[tokio::test]
async fn the_initial_password_door_does_not_name_the_lock_either() {
    let (base, db) = super::common::spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (_token, user_id) =
        super::common::register_full(&base, "enum_initial", "enum_initial@example.com").await;
    lock(&db, user_id).await;

    let attempt = |login: &'static str| {
        client
            .post(format!("{base}/api/v1/users/password/initial"))
            .json(&serde_json::json!({
                "login": login,
                "password": "definitely-not-the-password",
                "new_password": "Zx9#kLmq-new",
            }))
            .send()
    };
    let locked = attempt("enum_initial").await.unwrap();
    let unknown = attempt("enum_nobody_initial").await.unwrap();
    assert_eq!(locked.status(), unknown.status());
    assert_eq!(
        without_request_id(locked.json().await.unwrap()),
        without_request_id(unknown.json().await.unwrap()),
        "the initial-password door names the lock"
    );
}
