//! A reset link is not a second factor.
//!
//! `POST /users/reset-password` ended by minting a seven-day JWT and setting
//! the auth cookie, without ever reading `mfa_enabled` — so whoever held the
//! mail walked past the factor MFA is bought for, on the one door that is
//! reachable *because* a mailbox or a password is already compromised. The web
//! login for the same account, with the same new password, answers a challenge
//! and no session.
//!
//! The refusal is about the session only: the password is written either way,
//! or an MFA account could never recover a lost one. Every test below carries
//! that as a baseline in the same run, so a refusal proves the policy rather
//! than a broken fixture.
//!
//! The password-door half of the same class lives in
//! `registry_mfa_tests.rs` and `rg-ssh/tests/ssh_mfa_password_tests.rs`.

use crate::common::{register_full, spawn_test_app_with_db};

const OLD_PASSWORD: &str = "Qz7$wRtm";
const NEW_PASSWORD: &str = "Nw9#pLqz";

/// Plant a reset token straight into the database — the raw value only ever
/// leaves the server by email, which the test harness cannot read.
async fn issue_reset_token(db: &rg_db::DatabaseConnection, user_id: i64, raw: &str) -> String {
    use sha2::Digest;
    let hash = hex::encode(sha2::Sha256::digest(raw.as_bytes()));
    rg_db::ops::password_reset_token_ops::create(
        db,
        user_id,
        &hash,
        chrono::Utc::now() + chrono::Duration::minutes(15),
    )
    .await
    .expect("create reset token");
    raw.to_string()
}

async fn reset(base: &str, token: &str, password: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/api/v1/users/reset-password", base))
        .json(&serde_json::json!({ "token": token, "new_password": password }))
        .send()
        .await
        .unwrap()
}

async fn login(base: &str, username: &str, password: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({ "login": username, "password": password }))
        .send()
        .await
        .unwrap()
}

async fn me_status(client: &reqwest::Client, base: &str, token: &str) -> u16 {
    client
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
        .as_u16()
}

/// Every `Set-Cookie` value on a response, in order.
fn set_cookies(resp: &reqwest::Response) -> Vec<String> {
    resp.headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .map(|value| value.to_string())
        .collect()
}

/// A cookie is only a session if it carries a value; the clearing header shares
/// its name and sets nothing.
fn session_cookie(resp: &reqwest::Response) -> Option<String> {
    set_cookies(resp).into_iter().find(|cookie| {
        cookie.starts_with("forgekeep_token=") && !cookie.starts_with("forgekeep_token=;")
    })
}

fn challenge_cookie(resp: &reqwest::Response) -> Option<String> {
    set_cookies(resp).into_iter().find_map(|cookie| {
        cookie
            .strip_prefix("forgekeep_mfa_challenge=")
            .and_then(|rest| rest.split(';').next())
            .filter(|token| !token.is_empty())
            .map(|token| token.to_string())
    })
}

/// The whole finding: an account whose browser is asked for a TOTP code must
/// not be handed a session by the reset flow instead.
#[tokio::test]
async fn a_reset_gives_an_mfa_account_a_challenge_and_no_session() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, plain_id) = register_full(&base, "reset_plain", "reset_plain@example.com").await;
    let (_jwt2, mfa_id) = register_full(&base, "reset_mfa", "reset_mfa@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, mfa_id)
        .await
        .expect("enable mfa");

    // Baseline in the same run: without a second factor the reset still logs in.
    let plain_token = issue_reset_token(&db, plain_id, "raw-token-plain").await;
    let plain_resp = reset(&base, &plain_token, NEW_PASSWORD).await;
    assert_eq!(plain_resp.status(), 200, "baseline reset failed");
    assert!(
        session_cookie(&plain_resp).is_some(),
        "baseline: an account without MFA still gets its auth cookie from a reset"
    );
    let plain_body: serde_json::Value = plain_resp.json().await.unwrap();
    assert!(
        plain_body["token"].as_str().is_some_and(|t| !t.is_empty()),
        "baseline: an account without MFA still gets a session token from a reset"
    );

    let mfa_token = issue_reset_token(&db, mfa_id, "raw-token-mfa").await;
    let mfa_resp = reset(&base, &mfa_token, NEW_PASSWORD).await;
    assert_eq!(
        mfa_resp.status(),
        200,
        "the reset itself must still succeed for an MFA account"
    );
    assert!(
        session_cookie(&mfa_resp).is_none(),
        "the reset flow set an auth cookie for an account that owes a second factor"
    );
    let challenge = challenge_cookie(&mfa_resp)
        .expect("an MFA account must be answered with a challenge cookie, as the login door is");
    let mfa_body: serde_json::Value = mfa_resp.json().await.unwrap();
    assert_eq!(
        mfa_body["mfa_required"],
        serde_json::json!(true),
        "the reset answer must say the second factor is still owed"
    );
    assert_eq!(
        mfa_body["token"].as_str(),
        Some(""),
        "the reset flow returned a session token in the body for an MFA account"
    );

    // The challenge is the same credential the login door issues: a five-minute
    // token, signed with the challenge key, naming this account — so
    // `POST /users/mfa/verify` accepts it and nothing else does.
    let claims = rg_core::auth::jwt::validate_mfa_challenge(&challenge, "test-secret-key")
        .expect("the reset must issue a real MFA challenge, not an opaque string");
    assert_eq!(claims.sub, mfa_id.to_string());
    assert_eq!(claims.username, "reset_mfa");
    assert!(
        rg_core::auth::jwt::validate_token(&challenge, "test-secret-key").is_none(),
        "the challenge must not be usable as a session JWT"
    );
}

/// Refusing the session must not refuse the reset: an account with MFA has to
/// be able to recover a forgotten password like anyone else, and then finish
/// through the second factor.
#[tokio::test]
async fn a_reset_still_changes_the_password_of_an_mfa_account() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, mfa_id) = register_full(&base, "reset_mfa_pw", "reset_mfa_pw@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, mfa_id)
        .await
        .expect("enable mfa");

    let token = issue_reset_token(&db, mfa_id, "raw-token-mfa-pw").await;
    assert_eq!(reset(&base, &token, NEW_PASSWORD).await.status(), 200);

    let old = login(&base, "reset_mfa_pw", OLD_PASSWORD).await;
    assert_eq!(
        old.status(),
        401,
        "the old password still works after a reset that reported success"
    );

    let new = login(&base, "reset_mfa_pw", NEW_PASSWORD).await;
    assert_eq!(
        new.status(),
        200,
        "the new password does not work — the reset refused the session AND the reset"
    );
    let body: serde_json::Value = new.json().await.unwrap();
    assert_eq!(
        body["mfa_required"],
        serde_json::json!(true),
        "the login door must still ask this account for its second factor"
    );
}

/// A spent reset token is spent whether or not it ended in a session — the MFA
/// branch returns early, and an early return is the classic place to leave the
/// bookkeeping undone.
#[tokio::test]
async fn the_reset_token_of_an_mfa_account_is_single_use() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, mfa_id) = register_full(&base, "reset_mfa_once", "reset_mfa_once@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, mfa_id)
        .await
        .expect("enable mfa");

    let token = issue_reset_token(&db, mfa_id, "raw-token-mfa-once").await;
    assert_eq!(reset(&base, &token, NEW_PASSWORD).await.status(), 200);

    let replay = reset(&base, &token, "Ot8@vXbn").await;
    assert_eq!(
        replay.status(),
        400,
        "the reset token survived the MFA branch and could be spent twice"
    );
    assert_eq!(
        login(&base, "reset_mfa_once", "Ot8@vXbn").await.status(),
        401,
        "the replayed reset changed the password anyway"
    );
}

/// A password reset is the recovery path for a stolen session. The old bearer
/// token must therefore fail, while the response's replacement token proves the
/// revocation generation did not lock the recovering owner out as well.
#[tokio::test]
async fn a_password_reset_revokes_old_sessions_and_keeps_its_replacement_live() {
    let (base, db) = spawn_test_app_with_db().await;
    let (old_token, user_id) =
        register_full(&base, "reset_revoke", "reset_revoke@example.com").await;
    let client = reqwest::Client::new();

    assert_eq!(
        me_status(&client, &base, &old_token).await,
        200,
        "baseline: the bearer token must be live before the reset"
    );

    let raw_token = issue_reset_token(&db, user_id, "raw-token-revoke").await;
    let response = reset(&base, &raw_token, NEW_PASSWORD).await;
    assert_eq!(response.status(), 200, "password reset failed");
    let body: serde_json::Value = response.json().await.unwrap();
    let replacement = body["token"]
        .as_str()
        .expect("a non-MFA reset must return its replacement session token");

    assert_eq!(
        me_status(&client, &base, &old_token).await,
        401,
        "a bearer token issued before the password reset still works"
    );
    assert_eq!(
        me_status(&client, &base, replacement).await,
        200,
        "the session issued after the password reset was revoked with the old one"
    );
}

/// Logout is a server-side revocation, not merely a browser cookie operation.
/// The request deliberately presents the token as a Bearer credential to prove
/// that a copy stolen before logout cannot keep using the API afterwards.
#[tokio::test]
async fn logout_revokes_the_bearer_token_it_authenticated() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _user_id) =
        register_full(&base, "logout_revoke", "logout_revoke@example.com").await;
    let client = reqwest::Client::new();

    assert_eq!(
        me_status(&client, &base, &token).await,
        200,
        "baseline: the bearer token must be live before logout"
    );
    let logout = client
        .post(format!("{base}/api/v1/users/logout"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 200, "logout failed");

    assert_eq!(
        me_status(&client, &base, &token).await,
        401,
        "the bearer token stolen before logout still works"
    );
}
