//! card_3c33caaf7402: enrolling a second factor is one commit, not two.
//!
//! `POST /users/mfa/enable` used to flip `users.mfa_enabled` and then write the
//! backup-code set in a separate commit. The codes exist nowhere else — the
//! response that carries them is the only time their owner ever sees them — so a
//! failure between the two commits produced the worst available state: an
//! account with a second factor, nobody holding its recovery codes, and a `500`
//! telling the owner that enrolment had not happened.
//!
//! Driven over the real HTTP stack with the backup-code INSERT broken, which is
//! the only seam that reaches the branch. Presenting the second factor is the
//! reason this endpoint is on `failure_semantics_sweep_tests::NOT_DRIVEN`; here
//! the code is derived from the secret `POST /users/mfa/setup` hands back.

use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

use crate::common::fault::{fail_db_writes, DbWrite};
use crate::common::{register_full, spawn_test_app_with_db};

/// Enrol a TOTP secret and return the code an authenticator app would show.
///
/// The secret comes back from `setup` in the same base32 encoding `verify_code`
/// parses, so the app's side of the handshake is a `TOTP::generate_current`.
async fn setup_and_current_code(base: &str, token: &str) -> String {
    let setup: serde_json::Value = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(token)
        .send()
        .await
        .expect("setup request")
        .json()
        .await
        .expect("setup response body");
    let secret = setup["secret"].as_str().expect("setup returned no secret");
    current_code(secret)
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

async fn enable(base: &str, token: &str, code: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "code": code, "password": "Qz7$wRtm" }))
        .send()
        .await
        .expect("enable request")
}

/// The same call on the *replacement* path, which asks for the account password
/// before it retires the authenticator the account is standing on
/// (card_08400088bb40).
async fn enable_replacement(base: &str, token: &str, code: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "code": code, "password": "Qz7$wRtm" }))
        .send()
        .await
        .expect("enable request")
}

async fn stored_codes(db: &rg_db::DatabaseConnection, user_id: i64) -> u64 {
    rg_db::ops::mfa_backup_code_ops::Entity::find()
        .filter(rg_db::entities::mfa_backup_code::Column::UserId.eq(user_id))
        .count(db)
        .await
        .expect("count stored backup codes")
}

async fn mfa_enabled(db: &rg_db::DatabaseConnection, user_id: i64) -> bool {
    rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .expect("load user")
        .expect("user exists")
        .mfa_enabled
}

/// A failing code write must not leave the second factor switched on.
#[tokio::test]
async fn a_failed_backup_code_write_leaves_the_account_without_a_second_factor() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "mfa_atomic", "mfa_atomic@example.com").await;
    let code = setup_and_current_code(&base, &token).await;

    let fault = fail_db_writes(&db, "mfa_backup_codes", DbWrite::Insert).await;
    let failed = enable(&base, &token, &code).await;
    assert_eq!(
        failed.status(),
        500,
        "a real backup-code INSERT failure must reach the client"
    );
    fault.clear().await;

    assert!(
        !mfa_enabled(&db, user_id).await,
        "the second factor is on while its owner holds no recovery codes — \
         and the response said the enrolment failed"
    );
    assert_eq!(
        stored_codes(&db, user_id).await,
        0,
        "the failed enrolment stored part of a code set nobody was shown"
    );

    // The account is still enrollable: the rolled-back attempt left the TOTP
    // secret from `setup` in place, which is what the retry presents.
    let healthy = enable(&base, &token, &current_code_of(&db, user_id).await).await;
    assert_eq!(
        healthy.status(),
        200,
        "the retry after the fault must succeed"
    );
    let body: serde_json::Value = healthy.json().await.expect("enable response body");
    let handed_out = body["backup_codes"]
        .as_array()
        .expect("enable returned no backup codes")
        .len();
    assert_eq!(
        handed_out as u64,
        stored_codes(&db, user_id).await,
        "the number of codes shown to the owner is not the number that was stored"
    );
    assert!(mfa_enabled(&db, user_id).await);
}

/// Re-run `setup`'s half of the handshake from the stored secret.
///
/// The retry above needs a code for the *same* secret the first attempt used, and
/// calling `setup` again would stage a different one — which would prove nothing
/// about the state the failure left behind.
///
/// The enrolment in flight is the pending slot, and the promotion into
/// `totp_secret` rides in the very transaction these tests roll back
/// (card_08400088bb40) — so an attempt that failed leaves its secret staged, and
/// only a completed one leaves it live. Both are the same handshake; which
/// column holds it says how far the enrolment got.
async fn current_code_of(db: &rg_db::DatabaseConnection, user_id: i64) -> String {
    let user = rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .expect("load user")
        .expect("user exists");
    let encrypted = user
        .pending_totp_secret
        .or(user.totp_secret)
        .expect("the rolled-back attempt lost the TOTP secret");
    let key = rg_core::auth::encryption::derive_key(crate::common::TEST_ENCRYPTION_KEY);
    let secret = rg_core::auth::encryption::decrypt(&encrypted, &key).expect("decrypt TOTP secret");
    current_code(&secret)
}

/// A failing re-enrolment must not spend the codes the owner is still holding.
///
/// The replacement path is a second enrolment run end to end against an account
/// that already has a factor: `setup` stages a new secret, `enable` presents a
/// code for it together with the account password, and the commit that arms it
/// deletes the live code set before writing the next one. A failure anywhere in
/// there must leave the owner holding exactly what they were holding before.
#[tokio::test]
async fn a_failed_re_enrolment_keeps_the_codes_the_owner_already_has() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "mfa_reissue", "mfa_reissue@example.com").await;
    let code = setup_and_current_code(&base, &token).await;

    let first = enable(&base, &token, &code).await;
    assert_eq!(first.status(), 200, "the first enrolment must succeed");
    let body: serde_json::Value = first.json().await.expect("enable response body");
    let issued: Vec<String> = body["backup_codes"]
        .as_array()
        .expect("enable returned no backup codes")
        .iter()
        .map(|value| value.as_str().expect("a code is not a string").to_string())
        .collect();
    assert_eq!(stored_codes(&db, user_id).await, issued.len() as u64);

    // Step 1 of the replacement, which by itself must change nothing the
    // account is using.
    let replacement = setup_and_current_code(&base, &token).await;

    let fault = fail_db_writes(&db, "mfa_backup_codes", DbWrite::Insert).await;
    let failed = enable_replacement(&base, &token, &replacement).await;
    assert_eq!(
        failed.status(),
        500,
        "a real backup-code INSERT failure must reach the client"
    );
    fault.clear().await;

    assert_eq!(
        stored_codes(&db, user_id).await,
        issued.len() as u64,
        "the failed re-enrolment revoked the set its owner is still holding"
    );
    for code in &issued {
        assert!(
            rg_db::ops::mfa_backup_code_ops::verify_and_consume(&db, user_id, code)
                .await
                .expect("verify an issued code"),
            "a code this account was actually handed stopped working"
        );
    }
}
