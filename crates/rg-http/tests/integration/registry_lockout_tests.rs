//! The brute-force lockout has to cover `docker login` too.
//!
//! `POST /users/login` stops guessing after five tries. The registry's token
//! endpoint took the same username and password over HTTP Basic, verified the
//! Argon2 hash, and answered — with no lock honoured, no strike recorded, and
//! nothing in `login_log`. A five-try threshold on the web form is worth
//! nothing while `/v2/auth/token` next to it counts to infinity in silence.
//!
//! The registry's answer to bad credentials is an *anonymous* token, not a 401
//! — that is the OCI flow, and it is why the guessing was invisible: from the
//! outside every attempt looks like a successful anonymous handshake.
//!
//! The SSH half of the same class lives in `rg-ssh/tests/ssh_lockout_tests.rs`.

use base64::Engine as _;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::common::{register_full, spawn_test_app_with_db};

const PASSWORD: &str = "Qz7$wRtm";

fn basic(username: &str, password: &str) -> String {
    format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"))
    )
}

/// One `docker login`-shaped token request; returns the subject the registry
/// minted the token for (`"anonymous"` when the credentials were refused).
async fn token_subject(base: &str, auth: &str) -> String {
    let resp = reqwest::Client::new()
        .get(format!("{}/v2/auth/token", base))
        .query(&[
            ("service", "forgekeep-registry"),
            ("scope", "repository:reg_lock/image:pull"),
        ])
        .header(reqwest::header::AUTHORIZATION, auth)
        .send()
        .await
        .unwrap();
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
        token_subject(&base, &basic("reg_lock", PASSWORD)).await,
        "reg_lock",
        "baseline: the real password authenticates before any strike"
    );

    let threshold = rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS;
    for attempt in 1..=threshold {
        assert_eq!(
            token_subject(&base, &basic("reg_lock", "not-the-password")).await,
            "anonymous",
            "a wrong registry password was accepted on attempt {attempt}"
        );
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
    assert_eq!(
        token_subject(&base, &basic("reg_lock", PASSWORD)).await,
        "anonymous",
        "a locked account still authenticates against the registry"
    );

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

/// An anonymous pull carries no credentials at all — it must not be counted,
/// logged, or otherwise mistaken for a guess.
#[tokio::test]
async fn anonymous_registry_requests_are_not_login_attempts() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_jwt, user_id) = register_full(&base, "reg_lock", "reg_lock@example.com").await;

    let resp = reqwest::Client::new()
        .get(format!("{}/v2/auth/token", base))
        .query(&[
            ("service", "forgekeep-registry"),
            ("scope", "repository:reg_lock/image:pull"),
        ])
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

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
