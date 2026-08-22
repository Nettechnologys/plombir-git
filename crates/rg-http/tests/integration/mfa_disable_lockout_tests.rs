//! The brute-force lockout has to cover `POST /users/mfa/disable` too.
//!
//! The other three password doors (`POST /users/login`, SSH, `docker login`)
//! count guesses against a shared threshold. This one verified the Argon2 hash
//! and answered 401 — no lock honoured, no strike recorded, nothing in
//! `login_log`.
//!
//! It is not a way *in*: the door sits behind a valid session. It is the way a
//! stolen session is made permanent. An attacker holding someone's JWT can
//! guess the owner's password at Argon2 speed for as long as they like, purely
//! to take the second factor off — and before this test's fix, neither the
//! audit log nor the admin's view of the account recorded a single attempt.
//!
//! The registry and SSH halves of the same class live in
//! `registry_lockout_tests.rs` and `rg-ssh/tests/ssh_lockout_tests.rs`.

use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder};

use crate::common::{register_full, spawn_test_app_with_db};

const PASSWORD: &str = "Qz7$wRtm";

/// One `POST /users/mfa/disable` with the given password; returns the status.
async fn disable_attempt(base: &str, token: &str, password: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!("{}/api/v1/users/mfa/disable", base))
        .bearer_auth(token)
        .json(&serde_json::json!({ "password": password }))
        .send()
        .await
        .expect("request")
        .status()
}

/// One `POST /users/login`; returns the status.
async fn login_attempt(base: &str, login: &str, password: &str) -> reqwest::StatusCode {
    reqwest::Client::new()
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({ "login": login, "password": password }))
        .send()
        .await
        .expect("request")
        .status()
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
async fn failed_mfa_disable_passwords_lock_the_account() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "mfa_lock", "mfa_lock@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .expect("enable mfa");

    let threshold = rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS;
    for attempt in 1..=threshold {
        assert_eq!(
            disable_attempt(&base, &token, "not-the-password").await,
            401,
            "a wrong password was accepted on attempt {attempt}"
        );
    }

    let account = user(&db, user_id).await;
    assert_eq!(
        account.login_attempts, threshold,
        "mfa-disable password failures did not advance the brute-force counter"
    );
    assert!(
        account
            .locked_until
            .is_some_and(|until| until > chrono::Utc::now()),
        "{threshold} failed mfa-disable passwords did not lock the account"
    );
    assert!(
        account.mfa_enabled,
        "the second factor came off despite every password being wrong"
    );

    // The lock has to bite on the password that is actually correct, or it is
    // bookkeeping the attacker can sit out on the same session.
    assert_eq!(
        disable_attempt(&base, &token, PASSWORD).await,
        401,
        "a locked account still disabled its own MFA with the right password"
    );
    assert!(
        user(&db, user_id).await.mfa_enabled,
        "the lock did not stop the second factor coming off"
    );
}

/// The decision this door had to make: the counter is the *account's*, not the
/// door's. Strikes gathered here lock the login form, and vice versa — five
/// tries total, not five per entrance.
#[tokio::test]
async fn mfa_disable_shares_one_counter_with_the_login_form() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "mfa_lock", "mfa_lock@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .expect("enable mfa");

    let threshold = rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS;
    // Spend all but one strike on the login form...
    for _ in 1..threshold {
        assert_eq!(
            login_attempt(&base, "mfa_lock", "not-the-password").await,
            401
        );
    }
    assert_eq!(
        user(&db, user_id).await.login_attempts,
        threshold - 1,
        "login-form failures did not land on the shared counter"
    );

    // ...and the last one on the MFA-disable door. If the two doors counted
    // separately this would be strike 1 of 5 and the account would stay open.
    assert_eq!(
        disable_attempt(&base, &token, "not-the-password").await,
        401
    );

    let account = user(&db, user_id).await;
    assert_eq!(account.login_attempts, threshold);
    assert!(
        account
            .locked_until
            .is_some_and(|until| until > chrono::Utc::now()),
        "strikes from the two doors did not add up to one lock"
    );
    assert_eq!(
        login_attempt(&base, "mfa_lock", PASSWORD).await,
        401,
        "the lock an mfa-disable strike created does not hold the login form shut"
    );
}

#[tokio::test]
async fn rejected_mfa_disable_passwords_reach_the_login_log() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "mfa_lock", "mfa_lock@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .expect("enable mfa");

    let threshold = rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS;
    for _ in 1..=threshold {
        assert_eq!(
            disable_attempt(&base, &token, "not-the-password").await,
            401
        );
    }
    // The right password, refused by the lock the run above created.
    assert_eq!(disable_attempt(&base, &token, PASSWORD).await, 401);

    let rows = failed_log_rows(&db, "mfa_lock").await;
    assert_eq!(
        rows.len(),
        threshold as usize + 1,
        "mfa-disable rejections missing from login_log"
    );
    for row in &rows {
        assert_eq!(
            row.auth_provider, "mfa-disable",
            "the door is not named in the log — a run of these is \
             indistinguishable from web logins"
        );
        assert_eq!(row.user_id, Some(user_id));
    }
    // Same shape as the registry door: the strike that trips the threshold is
    // already filed as `account_locked`, reporting the lock the moment the
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
        "the door did not file the lock it created"
    );
}

/// The counterweight to locking the whole account from here: nothing decays
/// `login_attempts`, so without this reset two of the owner's own typos in the
/// disable form would be carried for months and three more would lock them out.
#[tokio::test]
async fn a_successful_mfa_disable_clears_the_strikes() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "mfa_lock", "mfa_lock@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
        .await
        .expect("enable mfa");

    for _ in 0..2 {
        assert_eq!(
            disable_attempt(&base, &token, "not-the-password").await,
            401
        );
    }
    assert_eq!(user(&db, user_id).await.login_attempts, 2);

    assert_eq!(
        disable_attempt(&base, &token, PASSWORD).await,
        200,
        "the right password did not disable MFA"
    );

    let account = user(&db, user_id).await;
    assert!(!account.mfa_enabled, "MFA was not disabled");
    assert_eq!(
        account.login_attempts, 0,
        "two typos in the disable form are still counted against the owner"
    );
    assert!(account.locked_until.is_none());
}
