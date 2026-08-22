//! A second factor the registry never asks for is not a second factor.
//!
//! `POST /users/login` answers an `mfa_enabled` account with a challenge and no
//! session. `GET /v2/auth/token` took the same username and password over HTTP
//! Basic, verified the same Argon2 hash, and minted a full-scope token — so the
//! account whose browser is being asked for a TOTP code opened to a bare
//! `docker login`. The gate was a convention of one door.
//!
//! A registry client has nowhere to prompt for a code, so the policy is the one
//! GitHub and Gitea settle on: the password stops working there and the account
//! authenticates with a personal access token instead. That token path is what
//! makes the refusal a policy rather than an eviction, so every test below
//! carries it as a baseline in the same run.
//!
//! The SSH half of the same class lives in
//! `rg-ssh/tests/ssh_mfa_password_tests.rs`.

use base64::Engine as _;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

use crate::common::{register_full, spawn_test_app_with_db};

const PASSWORD: &str = "Qz7$wRtm";

fn basic(username: &str, secret: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{secret}"))
    )
}

async fn create_pat(base: &str, jwt: &str, scopes: &str) -> String {
    let resp = reqwest::Client::new()
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({ "name": "docker-cli", "scopes": scopes }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

/// One `docker login`-shaped token request.
async fn token_request(base: &str, auth: &str) -> reqwest::Response {
    reqwest::Client::new()
        .get(format!("{}/v2/auth/token", base))
        .query(&[
            ("service", "forgekeep-registry"),
            ("scope", "repository:reg_mfa/image:pull"),
        ])
        .header(reqwest::header::AUTHORIZATION, auth)
        .send()
        .await
        .unwrap()
}

/// The subject the registry minted for a successful token request.
async fn token_subject(base: &str, auth: &str) -> String {
    let resp = token_request(base, auth).await;
    assert_eq!(resp.status(), 200, "token request failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    rg_core::auth::oci_token::validate_oci_token(body["token"].as_str().unwrap(), "test-secret-key")
        .expect("valid OCI token")
        .sub
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
        .all(db)
        .await
        .expect("read login log")
}

/// The headline: switching the second factor on closes `docker login` by
/// password, and says so — while the token path stays open.
#[tokio::test]
async fn mfa_closes_the_registry_password_door() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "reg_mfa", "reg_mfa@example.com").await;
    let pat = create_pat(&base, &jwt, "repo").await;

    assert_eq!(
        token_subject(&base, &basic("reg_mfa", PASSWORD)).await,
        "reg_mfa",
        "baseline: the password authenticates while the account has no second factor"
    );

    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .expect("enable MFA");

    // This is only reachable *with* the right password, so it can be explained
    // — and has to be, or the owner watches `docker login` quietly fail.
    let refused = token_request(&base, &basic("reg_mfa", PASSWORD)).await;
    assert_eq!(
        refused.status(),
        401,
        "an account with MFA still got a token from the registry for its bare password"
    );
    let body: serde_json::Value = refused.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "UNAUTHORIZED");
    assert!(
        body["errors"][0]["message"]
            .as_str()
            .unwrap()
            .contains("second factor"),
        "the refusal does not say why: {body}"
    );

    assert_eq!(
        token_subject(&base, &basic("reg_mfa", &pat)).await,
        "reg_mfa",
        "the credential the policy leaves an MFA account — a personal access token — \
         stopped working, so the refusal above proves a broken fixture and not the gate"
    );

    let rows = failed_log_rows(&db, "reg_mfa").await;
    assert_eq!(rows.len(), 1, "the second-factor refusal went unrecorded");
    assert_eq!(rows[0].failure_reason.as_deref(), Some("mfa_required"));
    assert_eq!(rows[0].auth_provider, "registry");
    assert_eq!(rows[0].user_id, Some(user_id));
}

/// A token is not a password guess. It is resolved before the Argon2 path
/// precisely so that it is never filed as one — otherwise five `docker pull`s
/// with a valid token would lock its owner out of the whole forge.
#[tokio::test]
async fn a_token_is_never_counted_as_a_failed_password() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "reg_mfa", "reg_mfa@example.com").await;
    let pat = create_pat(&base, &jwt, "repo").await;

    let threshold = rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS;
    for _ in 0..=threshold {
        assert_eq!(
            token_subject(&base, &basic("reg_mfa", &pat)).await,
            "reg_mfa"
        );
    }

    let account = user(&db, user_id).await;
    assert_eq!(
        account.login_attempts, 0,
        "token-authenticated pulls advanced the brute-force counter"
    );
    assert_eq!(
        account.locked_until, None,
        "repeated `docker pull`s with a valid token locked the account"
    );
    assert!(
        failed_log_rows(&db, "reg_mfa").await.is_empty(),
        "a valid token was filed in the login log as a failed password"
    );
}

/// The password door of an MFA-less account keeps working, and a wrong password
/// on an MFA account is still a guess — the gate must not become a place where
/// the counter stops running.
#[tokio::test]
async fn the_gate_changes_nothing_else_about_the_password_door() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, user_id) = register_full(&base, "reg_mfa", "reg_mfa@example.com").await;

    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .expect("enable MFA");

    let refused = token_request(&base, &basic("reg_mfa", "not-the-password")).await;
    assert_eq!(
        refused.status(),
        401,
        "a wrong password on an MFA account must be rejected like any other"
    );
    assert_eq!(
        user(&db, user_id).await.login_attempts,
        1,
        "wrong passwords stopped advancing the brute-force counter once MFA was on"
    );
}

/// A PAT is still an explicit credential. If it lacks the registry scope,
/// downgrading it to public access makes that refusal look like a success.
#[tokio::test]
async fn pat_without_repo_scope_is_rejected_instead_of_becoming_anonymous() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (jwt, _user_id) = register_full(&base, "reg_mfa", "reg_mfa@example.com").await;
    let pat = create_pat(&base, &jwt, "user").await;

    let response = token_request(&base, &basic("reg_mfa", &pat)).await;
    assert_eq!(
        response.status(),
        401,
        "a PAT without repo scope must not receive an anonymous token"
    );
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body["errors"][0]["code"], "UNAUTHORIZED");
    assert_eq!(body["errors"][0]["message"], "invalid credentials");
}
