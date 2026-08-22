//! Regression coverage for card_3462bdbcc4b8: what `POST /users/login` answers
//! when it could not decide, as opposed to when it decided against the caller.
//!
//! `POST /users/login` read the account's `mfa_enabled` flag with a `match` that
//! folded `Ok(None)` and `Err` into `false`, so a database that could not answer
//! "does this user owe a second factor?" was read as "no, they don't" — and the
//! handler went on to mint the JWT and set the auth cookie. The bypass lasted
//! exactly as long as the degradation and left nothing in the log.
//!
//! The three-way split itself is proven in `api::users::mfa_requirement_tests`:
//! both the password check and the MFA lookup read `users`, so no fault that
//! breaks the second one leaves the first standing, and the branch is only
//! reachable by a database that fails *between* two queries of one request.
//! What is provable from out here is what that branch exists to protect, and
//! this file drives it over real HTTP: an account that owes a second factor is
//! never handed a session by the password alone, a database the endpoint could
//! not read is a retryable server error rather than a session or a `401`, and
//! the three genuine credential verdicts still answer `401`.

use crate::common::{register_full, spawn_test_app_with_db};
use sea_orm::ConnectionTrait as _;

const PASSWORD: &str = "Qz7$wRtm";

async fn attempt_login_with(base: &str, username: &str, password: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({ "login": username, "password": password }))
        .send()
        .await
        .expect("login request")
}

async fn attempt_login(base: &str, username: &str) -> reqwest::Response {
    attempt_login_with(base, username, PASSWORD).await
}

/// The value of the auth cookie the response sets, if it sets one to anything.
///
/// The MFA branch deliberately emits `forgekeep_token=` with `Max-Age=0` to
/// clear a stale session, so "a `Set-Cookie` naming the auth cookie" is not the
/// question — "a session the browser will send back" is.
fn issued_session(response: &reqwest::Response) -> Option<String> {
    response
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .filter_map(|cookie| cookie.split(';').next())
        .filter_map(|pair| pair.trim().strip_prefix("forgekeep_token="))
        .find(|token| !token.is_empty())
        .map(str::to_string)
}

#[tokio::test]
async fn a_password_alone_never_opens_an_mfa_account() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_token, user_id) = register_full(&base, "mfagate", "mfagate@example.com").await;

    // Baseline. Without it the assertions below cannot tell "the second factor
    // is enforced" from "this endpoint stopped issuing sessions to anyone".
    let response = attempt_login(&base, "mfagate").await;
    assert_eq!(response.status(), 200);
    assert!(
        issued_session(&response).is_some(),
        "an account without MFA must still get its session from the password"
    );
    let body: serde_json::Value = response.json().await.expect("login body");
    assert_ne!(body["token"].as_str(), Some(""));

    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .expect("enable mfa");

    let response = attempt_login(&base, "mfagate").await;
    assert_eq!(response.status(), 200);
    assert_eq!(
        issued_session(&response),
        None,
        "the password alone handed out a session for an account that owes a second factor"
    );
    let body: serde_json::Value = response.json().await.expect("login body");
    assert_eq!(body["mfa_required"], true);
    assert_eq!(
        body["token"].as_str(),
        Some(""),
        "the challenge response must not carry a usable token in its body either"
    );
}

#[tokio::test]
async fn a_login_that_cannot_reach_the_database_issues_no_session() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_token, user_id) = register_full(&base, "mfaoutage", "mfaoutage@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .expect("enable mfa");

    // Healthy first, for the same reason as above: an endpoint that answers 5xx
    // to everything would satisfy the outage assertions vacuously.
    let response = attempt_login(&base, "mfaoutage").await;
    assert_eq!(response.status(), 200);
    assert_eq!(issued_session(&response), None);

    db.close().await.expect("close the login fixture pool");

    let response = attempt_login(&base, "mfaoutage").await;
    let status = response.status();
    assert!(
        status.is_server_error(),
        "a database the login could not read is ours to own, got {status}"
    );
    assert_eq!(
        status,
        reqwest::StatusCode::SERVICE_UNAVAILABLE,
        "an unreachable database must stay retryable, not become a flat 500"
    );
    assert_eq!(
        issued_session(&response),
        None,
        "a login that never resolved the account still set a session cookie"
    );

    let body = response.text().await.expect("outage body");
    assert!(
        !body.contains("db:") && !body.to_lowercase().contains("users"),
        "the outage response leaked the database error to the client: {body}"
    );
}

/// The other side of that split, and the guard on how it is drawn.
///
/// `login` recognises a credential *verdict* by the message the service bails
/// with and treats everything else as its own failure. That makes the list of
/// verdicts a contract with `rg_core::user::service`, and a message renamed
/// over there would turn a mistyped password into a `500` for every account on
/// the instance. Each of the three is driven here so the rename cannot land
/// quietly.
#[tokio::test]
async fn every_credential_verdict_is_still_the_callers_401() {
    let (base, db) = spawn_test_app_with_db().await;
    register_full(&base, "verdictwrong", "verdictwrong@example.com").await;
    let (_token, disabled_id) = register_full(&base, "verdictoff", "verdictoff@example.com").await;
    let (_token, locked_id) = register_full(&base, "verdictlock", "verdictlock@example.com").await;

    assert_eq!(
        attempt_login_with(&base, "verdictwrong", "not-the-password")
            .await
            .status(),
        401,
        "a mistyped password is the caller's problem, not a server error"
    );
    assert!(
        attempt_login(&base, "verdictwrong")
            .await
            .status()
            .is_success(),
        "the baseline account must still be able to log in"
    );

    db.execute_unprepared(&format!(
        "UPDATE users SET is_active = 0 WHERE id = {disabled_id}"
    ))
    .await
    .expect("deactivate the account");
    assert_eq!(
        attempt_login(&base, "verdictoff").await.status(),
        401,
        "a deactivated account is a verdict, and the right password must not turn it into a 500"
    );

    for _ in 0..rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS {
        rg_db::ops::user_ops::record_failed_login(
            &db,
            locked_id,
            rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS,
        )
        .await
        .expect("advance the brute-force counter");
    }
    assert_eq!(
        attempt_login(&base, "verdictlock").await.status(),
        401,
        "a locked account is a verdict too, however correct the password is"
    );
}
