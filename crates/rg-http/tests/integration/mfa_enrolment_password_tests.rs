//! Arming a second factor asks for the account password — on the first
//! enrolment, not only on a rotation (security audit finding #6).
//!
//! A first enrolment used to be deliberately password-less: the caller was
//! "adding a protection, not removing one". It was the other way round. The
//! factor a stolen session arms is the *thief's* protection against the owner:
//! the owner's password reset then ends at a second factor only the thief can
//! pass, and nothing short of an administrator gets the account back. So the
//! password is asked for at the moment the factor is armed, through the same
//! lockout-aware door `POST /users/mfa/disable` uses — and an account that has
//! no password here (it signs in through an identity provider) is told so with
//! a `400`, not handed a `500` for a hash the verifier could not parse.
//!
//! The PAT half of the same finding — a `user`-scoped token must not reach any
//! of these doors — lives in `pat_api_tests`.

use rg_db::sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::common::{register_full, spawn_test_app_with_db};

const PASSWORD: &str = "Qz7$wRtm";

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

/// One `POST /users/mfa/setup`, returning the secret it handed out.
async fn setup(base: &str, token: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(token)
        .send()
        .await
        .expect("setup request");
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("setup response body");
    assert_eq!(status, 200, "setup failed: {body}");
    body["secret"]
        .as_str()
        .expect("setup returned no secret")
        .to_string()
}

/// One `POST /users/mfa/enable`, as `(status, body)`.
async fn enable(
    base: &str,
    token: &str,
    code: &str,
    password: Option<&str>,
) -> (u16, serde_json::Value) {
    let mut request = serde_json::json!({ "code": code });
    if let Some(password) = password {
        request["password"] = serde_json::Value::String(password.to_string());
    }
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(token)
        .json(&request)
        .send()
        .await
        .expect("enable request");
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().await.expect("enable response body");
    (status, body)
}

async fn user(db: &rg_db::DatabaseConnection, user_id: i64) -> rg_db::entities::user::Model {
    rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .expect("load user")
        .expect("user exists")
}

async fn failed_log_rows(
    db: &rg_db::DatabaseConnection,
    username: &str,
) -> Vec<rg_db::entities::login_log::Model> {
    rg_db::ops::login_log_ops::Entity::find()
        .filter(rg_db::entities::login_log::Column::Username.eq(username))
        .filter(rg_db::entities::login_log::Column::Success.eq(false))
        .order_by_asc(rg_db::entities::login_log::Column::Id)
        .all(db)
        .await
        .expect("read login log")
}

fn message(body: &serde_json::Value) -> &str {
    body["error"]["message"].as_str().unwrap_or_default()
}

#[tokio::test]
async fn a_first_enrolment_requires_the_account_password() {
    let (base, db) = spawn_test_app_with_db().await;
    let username = "mfa_first_pw";
    let (token, user_id) = register_full(&base, username, "mfa_first_pw@example.com").await;
    let secret = setup(&base, &token).await;

    // A valid code and no password: the body that used to arm the factor.
    let (status, body) = enable(&base, &token, &current_code(&secret), None).await;
    assert_eq!(
        status, 400,
        "a first enrolment armed a factor without the password: {body}"
    );
    assert!(
        message(&body).contains("password"),
        "the refusal must say what is missing: {body}"
    );
    assert!(
        !user(&db, user_id).await.mfa_enabled,
        "a refused enrolment switched the factor on"
    );

    // A wrong password is a strike against the account, in the same ledger
    // the login form writes to — the door sits behind a session, and a guess
    // that advanced no counter is the attacker's preferred kind.
    let (status, body) = enable(
        &base,
        &token,
        &current_code(&secret),
        Some("not-the-account-password"),
    )
    .await;
    assert_eq!(status, 401, "a wrong password armed the factor: {body}");
    let after_guess = user(&db, user_id).await;
    assert!(!after_guess.mfa_enabled);
    assert_eq!(
        after_guess.login_attempts, 1,
        "a wrong password on enrolment did not advance the brute-force counter"
    );
    let rows = failed_log_rows(&db, username).await;
    assert_eq!(
        rows.len(),
        1,
        "the guess is missing from login_log: {rows:?}"
    );
    assert_eq!(
        rows[0].auth_provider, "mfa-enable",
        "the attempt record must name the door it was made at"
    );

    // With the password, the enrolment goes through as it always did.
    let (status, body) = enable(&base, &token, &current_code(&secret), Some(PASSWORD)).await;
    assert_eq!(
        status, 200,
        "a confirmed first enrolment was refused: {body}"
    );
    assert!(!body["backup_codes"]
        .as_array()
        .expect("an enrolment publishes a recovery set")
        .is_empty());
    assert!(user(&db, user_id).await.mfa_enabled);
}

/// An account that signs in through an identity provider has no password here.
///
/// Its `password_hash` column is empty, and an empty string is not a PHC hash:
/// handed to the verifier it is an unusable-hash *error*, which the transport
/// turns into a `500` — reading, to the operator, as a broken hash on a healthy
/// row. Nothing is wrong with the row; this door is simply one the account
/// cannot open, and the answer is a `400` that says so. And no strike: nothing
/// was guessed.
#[tokio::test]
async fn an_account_with_no_password_here_is_told_so_rather_than_answered_500() {
    let (base, db) = spawn_test_app_with_db().await;
    let username = "mfa_sso_only";
    let (token, user_id) = register_full(&base, username, "mfa_sso_only@example.com").await;

    // The shape `find_or_create_sso_user` leaves behind: a provider name and
    // no local secret.
    rg_db::entities::user::Entity::update_many()
        .col_expr(
            rg_db::entities::user::Column::PasswordHash,
            rg_db::sea_orm::sea_query::Expr::value(String::new()),
        )
        .col_expr(
            rg_db::entities::user::Column::AuthProvider,
            rg_db::sea_orm::sea_query::Expr::value("oidc".to_string()),
        )
        .filter(rg_db::entities::user::Column::Id.eq(user_id))
        .exec(&db)
        .await
        .expect("turn the fixture into an SSO-only account");

    let secret = setup(&base, &token).await;
    let (status, body) = enable(&base, &token, &current_code(&secret), Some("anything")).await;
    assert_eq!(
        status, 400,
        "an SSO-only account enabling MFA must get a clear refusal, not a server error: {body}"
    );
    assert!(
        message(&body).contains("no password"),
        "the refusal must say the account has no password here: {body}"
    );

    let disable = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/disable"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "password": "anything" }))
        .send()
        .await
        .expect("disable request");
    assert_eq!(
        disable.status(),
        400,
        "the same door, the same answer: {}",
        disable.text().await.unwrap_or_default()
    );

    let account = user(&db, user_id).await;
    assert!(!account.mfa_enabled);
    assert_eq!(
        account.login_attempts, 0,
        "a password that could not be checked must not count as a wrong one"
    );
    assert!(
        failed_log_rows(&db, username).await.is_empty(),
        "nothing was guessed, so nothing belongs in login_log"
    );
}
