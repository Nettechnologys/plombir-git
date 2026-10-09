//! `POST /api/v1/users/login` must not answer "does this account exist?".
//!
//! The response *body* has always been unified to `invalid credentials`, but a
//! login for a real account paid for an Argon2 verification (tens of
//! milliseconds, on purpose) while an unknown username was rejected the moment
//! the database lookup came back empty. That difference is an order of
//! magnitude above network noise, which made a dictionary sweep over usernames
//! a perfectly good enumeration oracle.
//!
//! The brute-force lock was the second half of the same oracle: it was read
//! before the verification and answered "account is temporarily locked", so
//! five requests against a candidate name said "this account exists" without a
//! password — and refused the account's owner for fifteen minutes.

use super::common::{register_full, register_user, spawn_test_app, spawn_test_app_with_db};

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

/// The lock must not bring the oracle back through its own text.
///
/// Before the fix, a locked account answered "account is temporarily locked"
/// *instead of* verifying the password: five requests against a guessed name
/// produced a body no other login produced, which confirmed the name and
/// denied the owner their correct password for fifteen minutes. The lock still
/// refuses a correct password, but an attacker and a mistyped password now get
/// one indistinguishable answer.
#[tokio::test]
async fn a_locked_account_answers_like_a_missing_one() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (_token, locked_id) = register_full(&base, "enum_locked", "enum_locked@example.com").await;

    for _ in 0..rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS {
        rg_db::ops::user_ops::record_failed_login(
            &db,
            locked_id,
            rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS,
        )
        .await
        .expect("advance the brute-force counter");
    }

    // The right password, and the account stays closed...
    let response = client
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({"login": "enum_locked", "password": PASSWORD}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        401,
        "the lock must still refuse the account its own password"
    );
    let mut locked_body: serde_json::Value = response.json().await.unwrap();
    if let Some(error) = locked_body.get_mut("error").and_then(|e| e.as_object_mut()) {
        error.remove("request_id");
    }

    // ... and says no more about itself than a name nobody owns.
    let (_, unknown_body) = failed_login(&client, &base, "enum_nobody").await;
    assert_eq!(
        locked_body, unknown_body,
        "the rejection separates a locked account from a missing one"
    );
}
