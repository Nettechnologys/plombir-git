//! card_37f787c458bc: an account DELETE that wins after an MFA route's read is
//! an ordinary missing-user outcome, never a backend-shaped write error or a
//! success audit event.

use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, Statement};

use crate::common::{register_full, spawn_test_app_with_db};

const PASSWORD: &str = "Qz7$wRtm";

async fn setup(base: &str, token: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(token)
        .send()
        .await
        .expect("setup request")
}

async fn setup_secret(base: &str, token: &str) -> String {
    let response = setup(base, token).await;
    assert_eq!(response.status(), 200, "healthy MFA setup must succeed");
    let body: serde_json::Value = response.json().await.expect("setup response body");
    body["secret"]
        .as_str()
        .expect("setup returned no secret")
        .to_string()
}

fn current_code(secret: &str) -> String {
    let bytes = totp_rs::Secret::Encoded(secret.to_string())
        .to_bytes()
        .expect("the setup secret is not base32");
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

async fn enable(base: &str, token: &str, secret: &str) -> reqwest::Response {
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "code": current_code(secret), "password": PASSWORD }))
        .send()
        .await
        .expect("enable request")
}

async fn audit_count(db: &rg_db::DatabaseConnection) -> u64 {
    rg_db::ops::audit_log_ops::list_paginated(db, 0, 1, None, None, None, None, None)
        .await
        .expect("count audit rows")
        .1
}

async fn backup_code_count(db: &rg_db::DatabaseConnection, user_id: i64) -> u64 {
    rg_db::ops::mfa_backup_code_ops::Entity::find()
        .filter(rg_db::entities::mfa_backup_code::Column::UserId.eq(user_id))
        .count(db)
        .await
        .expect("count backup codes")
}

async fn install_delete_before_update(
    db: &rg_db::DatabaseConnection,
    user_id: i64,
    trigger_name: &str,
    column: &str,
) {
    db.execute(Statement::from_string(
        db.get_database_backend(),
        format!(
            "CREATE TRIGGER {trigger_name} BEFORE UPDATE OF {column} ON users \
             WHEN OLD.id = {user_id} \
             BEGIN DELETE FROM users WHERE id = OLD.id; END"
        ),
    ))
    .await
    .expect("install the competing account delete");
}

async fn assert_typed_not_found(response: reqwest::Response) {
    let status = response.status();
    let body = response.text().await.expect("read error response");
    assert_eq!(
        status, 404,
        "a DELETE after the scoped read must stay a typed missing user: {body}"
    );
    assert!(
        !body.contains("RecordNotUpdated") && !body.contains("db:"),
        "the response leaked a backend-shaped write failure: {body}"
    );
}

#[tokio::test]
async fn setup_deleted_inside_the_totp_write_is_404_and_returns_no_unstored_secret() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "mfa_setup_race", "mfa_setup_race@example.com").await;
    let audit_before = audit_count(&db).await;
    // The column the setup step writes, which since card_08400088bb40 is the
    // pending slot rather than the live secret. A trigger left on the old column
    // never fires, and this test passes by never reaching the race it exists to
    // drive.
    install_delete_before_update(
        &db,
        user_id,
        "delete_user_before_totp_setup",
        "pending_totp_secret",
    )
    .await;

    assert_typed_not_found(setup(&base, &token).await).await;
    assert!(
        rg_db::ops::user_ops::find_by_id(&db, user_id)
            .await
            .expect("look for the deleted setup account")
            .is_none(),
        "the losing setup resurrected the account"
    );
    assert_eq!(
        audit_count(&db).await,
        audit_before,
        "the losing setup published an audit event"
    );
}

#[tokio::test]
async fn enable_deleted_inside_the_flag_write_is_404_with_no_codes_or_audit() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "mfa_enable_race", "mfa_enable_race@example.com").await;
    let secret = setup_secret(&base, &token).await;
    let audit_before = audit_count(&db).await;
    install_delete_before_update(&db, user_id, "delete_user_before_mfa_enable", "mfa_enabled")
        .await;

    assert_typed_not_found(enable(&base, &token, &secret).await).await;
    assert!(
        rg_db::ops::user_ops::find_by_id(&db, user_id)
            .await
            .expect("look for the deleted enrolment account")
            .is_none(),
        "the losing enrolment resurrected the account"
    );
    assert_eq!(
        backup_code_count(&db, user_id).await,
        0,
        "the losing enrolment published recovery credentials"
    );
    assert_eq!(
        audit_count(&db).await,
        audit_before,
        "the losing enrolment published a success audit event"
    );
}

#[tokio::test]
async fn disable_deleted_inside_the_flag_write_is_404_with_no_codes_or_audit() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) =
        register_full(&base, "mfa_disable_race", "mfa_disable_race@example.com").await;
    let secret = setup_secret(&base, &token).await;
    let enabled = enable(&base, &token, &secret).await;
    assert_eq!(enabled.status(), 200, "fixture MFA enrolment must succeed");
    assert!(
        backup_code_count(&db, user_id).await > 0,
        "fixture enrolment published no recovery credentials"
    );
    let audit_before = audit_count(&db).await;
    install_delete_before_update(
        &db,
        user_id,
        "delete_user_before_mfa_disable",
        "mfa_enabled",
    )
    .await;

    let disabled = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/disable"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "password": PASSWORD }))
        .send()
        .await
        .expect("disable request");
    assert_typed_not_found(disabled).await;
    assert!(
        rg_db::ops::user_ops::find_by_id(&db, user_id)
            .await
            .expect("look for the deleted removal account")
            .is_none(),
        "the losing removal resurrected the account"
    );
    assert_eq!(
        backup_code_count(&db, user_id).await,
        0,
        "backup credentials outlived the deleted account"
    );
    assert_eq!(
        audit_count(&db).await,
        audit_before,
        "the losing removal published a success audit event"
    );
}
