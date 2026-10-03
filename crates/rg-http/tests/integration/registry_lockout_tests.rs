//! The brute-force lockout has to cover `docker login` too.
//!
//! `POST /users/login` stops guessing after five tries. The registry's token
//! endpoint took the same username and password over HTTP Basic, verified the
//! Argon2 hash, and answered — with no lock honoured, no strike recorded, and
//! nothing in `login_log`. A five-try threshold on the web form is worth
//! nothing while `/v2/auth/token` next to it counts to infinity in silence.
//!
//! A missing credential still gets an anonymous token for public pulls, but a
//! supplied password that is wrong gets an OCI 401. Otherwise a `docker login`
//! failure looks exactly like a successful anonymous handshake.
//!
//! The SSH half of the same class lives in `rg-ssh/tests/ssh_lockout_tests.rs`.

use base64::Engine as _;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Statement};

use crate::common::{register_full, spawn_test_app_with_db};

const PASSWORD: &str = "Qz7$wRtm";

fn basic(username: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
    )
}

/// One `docker login`-shaped token request.
async fn token_request(base: &str, auth: Option<&str>) -> reqwest::Response {
    let request = reqwest::Client::new()
        .get(format!("{}/v2/auth/token", base))
        .query(&[
            ("service", "plombir-git-registry"),
            ("scope", "repository:reg_lock/image:pull"),
        ]);
    let request = match auth {
        Some(auth) => request.header(reqwest::header::AUTHORIZATION, auth),
        None => request,
    };
    request.send().await.unwrap()
}

/// The subject from a successful token response.
async fn token_subject(base: &str, auth: Option<&str>) -> String {
    let resp = token_request(base, auth).await;
    assert_eq!(resp.status(), 200, "token request failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    rg_core::auth::oci_token::validate_oci_token(body["token"].as_str().unwrap(), "test-secret-key")
        .expect("valid OCI token")
        .sub
}

/// The byte-for-byte OCI response for a rejected credential. Keeping the raw
/// bytes guards against adding a response-body account-enumeration oracle.
async fn rejected_body(base: &str, auth: &str) -> Vec<u8> {
    let resp = token_request(base, Some(auth)).await;
    assert_eq!(
        resp.status(),
        reqwest::StatusCode::UNAUTHORIZED,
        "credential rejection must not mint an anonymous token"
    );
    resp.bytes().await.unwrap().to_vec()
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

#[tokio::test]
async fn failed_registry_logins_lock_the_account() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, user_id) = register_full(&base, "reg_lock", "reg_lock@example.com").await;

    assert_eq!(
        token_subject(&base, Some(&basic("reg_lock", PASSWORD))).await,
        "reg_lock",
        "baseline: the real password authenticates before any strike"
    );

    let threshold = rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS;
    for _attempt in 1..=threshold {
        rejected_body(&base, &basic("reg_lock", "not-the-password")).await;
    }

    let account = user(&db, user_id).await;
    assert_eq!(
        account.login_attempts, threshold,
        "registry password failures did not advance the brute-force counter"
    );
    assert!(
        account
            .locked_until
            .is_some_and(|until| until > chrono::Utc::now()),
        "{threshold} failed docker logins did not lock the account"
    );

    // The lock has to bite on the credentials that are actually correct,
    // otherwise it is bookkeeping the attacker can ignore.
    rejected_body(&base, &basic("reg_lock", PASSWORD)).await;

    let rows = failed_log_rows(&db, "reg_lock").await;
    assert_eq!(
        rows.len(),
        threshold as usize + 1,
        "registry rejections missing from login_log"
    );
    for row in &rows {
        assert_eq!(
            row.auth_provider, "registry",
            "the door is not named in the log"
        );
        assert_eq!(row.user_id, Some(user_id));
    }
    // The strike that trips the threshold is already filed as `account_locked`
    // — same as on `POST /users/login`, which reports the lock the moment the
    // write creates it rather than one attempt later.
    let reasons: Vec<Option<&str>> = rows
        .iter()
        .map(|row| row.failure_reason.as_deref())
        .collect();
    let mut expected = vec![Some("invalid_credentials"); threshold as usize - 1];
    expected.push(Some("account_locked")); // the strike that trips the threshold
    expected.push(Some("account_locked")); // the right password, refused by it
    assert_eq!(
        reasons, expected,
        "the registry did not file the lock it created"
    );
}

/// A wrong password and an unknown user both pay the dummy-hash cost and must
/// return exactly the same OCI envelope. The 401 distinguishes a rejected
/// login from a missing Authorization header; it must not distinguish accounts.
#[tokio::test]
async fn registry_rejects_unknown_and_known_accounts_with_the_same_answer() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (_jwt, _user_id) = register_full(&base, "reg_known", "reg_known@example.com").await;

    let known = rejected_body(&base, &basic("reg_known", "not-the-password")).await;
    let unknown = rejected_body(&base, &basic("reg_unknown", "not-the-password")).await;

    assert_eq!(
        known, unknown,
        "the registry rejection body distinguishes a real account from a missing one"
    );
}

/// An anonymous pull carries no credentials at all — it must not be counted,
/// logged, or otherwise mistaken for a guess.
#[tokio::test]
async fn anonymous_registry_requests_are_not_login_attempts() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, user_id) = register_full(&base, "reg_lock", "reg_lock@example.com").await;

    assert_eq!(
        token_subject(&base, None).await,
        "anonymous",
        "a request with no Authorization header must retain public-pull access"
    );

    assert_eq!(user(&db, user_id).await.login_attempts, 0);
    assert!(
        rg_db::ops::login_log_ops::Entity::find()
            .all(&db)
            .await
            .unwrap()
            .iter()
            .all(|row| row.auth_provider != "registry"),
        "an anonymous registry request was filed as a failed login"
    );
}

/// The password hash may verify against a row which account deletion retires
/// before the registry publishes its identity. The conditional counter reset is
/// the lifecycle finalizer for this non-interactive door; both ordinary lifecycle
/// losses are credential rejections, never authenticated or anonymous tokens.
#[tokio::test]
async fn registry_password_losing_to_retirement_or_delete_is_rejected() {
    let (base, db) = spawn_test_app_with_db().await;

    for (index, delete) in [false, true].into_iter().enumerate() {
        let username = format!("registry_lifecycle_{index}");
        let (_, user_id) =
            register_full(&base, &username, &format!("{username}@example.invalid")).await;
        let mutation = if delete {
            "DELETE FROM users WHERE id = OLD.id;"
        } else {
            "UPDATE users SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
             WHERE id = OLD.id;"
        };
        db.execute(Statement::from_string(
            db.get_database_backend(),
            format!(
                "CREATE TRIGGER lose_registry_login_{index} \
                 BEFORE UPDATE OF login_attempts ON users WHEN OLD.id = {user_id} \
                 BEGIN {mutation} SELECT RAISE(IGNORE); END"
            ),
        ))
        .await
        .expect("install the competing account lifecycle mutation");

        let response = token_request(&base, Some(&basic(&username, PASSWORD))).await;
        assert_eq!(
            response.status(),
            reqwest::StatusCode::UNAUTHORIZED,
            "the registry accepted a password whose account lifecycle had ended"
        );
        let body: serde_json::Value = response.json().await.expect("OCI rejection body");
        assert!(
            body.get("token").is_none(),
            "the rejected registry login returned a bearer token: {body}"
        );
    }
}
