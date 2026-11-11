//! Credential recovery must not leave a pre-existing or in-flight address link usable.
use crate::common::{register_full, spawn_test_app_with_db};
use rg_db::entities::{email_confirmation, notification_delivery, user};
use rg_db::ops::{email_confirmation_ops as links, password_reset_token_ops as resets, user_ops};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, PaginatorTrait, QueryFilter, Statement};

async fn account(db: &rg_db::DatabaseConnection, id: i64) -> user::Model {
    user_ops::find_by_id(db, id).await.unwrap().unwrap()
}

async fn pending(
    db: &rg_db::DatabaseConnection,
    user: &user::Model,
    token: &str,
) -> email_confirmation::Model {
    let hash = rg_db::token_digest(token);
    assert!(links::replace_pending_email_change(
        db,
        user,
        &format!("new-{}@example.test", user.username),
        &hash,
        chrono::Utc::now() + chrono::Duration::hours(1)
    )
    .await
    .unwrap());
    links::find_live(db, &hash).await.unwrap().unwrap()
}

async fn event_count(db: &rg_db::DatabaseConnection, id: i64, event: &str) -> u64 {
    notification_delivery::Entity::find()
        .filter(notification_delivery::Column::UserId.eq(id))
        .filter(notification_delivery::Column::EventKey.eq(event))
        .count(db)
        .await
        .unwrap()
}

#[tokio::test]
async fn forgot_password_keeps_its_padded_response_when_issuance_loses_to_an_address_change() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, healthy) = register_full(&base, "reset-healthy", "reset-healthy@example.test").await;
    let (_, raced) = register_full(&base, "reset-raced", "reset-raced@example.test").await;
    // Change the account after forgot-password's lookup, at the first write of
    // issuance. The stale snapshot must produce the ordinary padded no-op.
    db.execute(Statement::from_string(db.get_database_backend(), format!(
        "CREATE TRIGGER move_before_reset_issuance BEFORE UPDATE OF session_version ON users WHEN OLD.id = {raced} AND NEW.session_version = OLD.session_version BEGIN UPDATE users SET email = 'moved@example.test' WHERE id = OLD.id; END"
    ))).await.unwrap();
    let mut expected = None;
    for email in [
        "reset-healthy@example.test",
        "absent@example.test",
        "reset-raced@example.test",
    ] {
        let started = std::time::Instant::now();
        let response = reqwest::Client::new()
            .post(format!("{base}/api/v1/users/forgot-password"))
            .json(&serde_json::json!({"email": email}))
            .send()
            .await
            .unwrap();
        assert!(
            started.elapsed() >= std::time::Duration::from_millis(80),
            "missing timing pad for {email}"
        );
        let status = response.status();
        let body: serde_json::Value = response.json().await.unwrap();
        assert_eq!(status, 200);
        if let Some(expected) = &expected {
            assert_eq!(&body, expected);
        } else {
            expected = Some(body);
        }
    }
    for (id, count) in [(healthy, 1), (raced, 0)] {
        assert_eq!(
            rg_db::entities::password_reset_token::Entity::find()
                .filter(rg_db::entities::password_reset_token::Column::UserId.eq(id))
                .count(&db)
                .await
                .unwrap(),
            count
        );
    }
}

#[tokio::test]
async fn credential_recovery_revokes_even_an_already_read_address_link() {
    let (base, db) = spawn_test_app_with_db().await;
    for action in [
        "password",
        "admin-password",
        "password-reset",
        "factor-reset",
    ] {
        let (_, id) = register_full(&base, action, &format!("{action}@example.test")).await;
        if action == "factor-reset" {
            user_ops::enable_mfa(&db, id).await.unwrap();
        }
        let before = account(&db, id).await;
        let row = pending(&db, &before, action).await;
        match action {
            "password" => {
                user_ops::change_password(&db, id, &before.password_hash, "changed")
                    .await
                    .unwrap()
                    .unwrap();
            }
            "admin-password" => {
                user_ops::set_password_by_administrator(&db, id, "changed")
                    .await
                    .unwrap()
                    .unwrap();
            }
            "password-reset" => {
                let reset = resets::create(
                    &db,
                    id,
                    "reset-token",
                    chrono::Utc::now() + chrono::Duration::minutes(15),
                )
                .await
                .unwrap();
                resets::complete_password_reset(&db, reset.id, id, "changed")
                    .await
                    .unwrap()
                    .unwrap();
            }
            "factor-reset" => {
                rg_core::user::account::reset_mfa(&db, id).await.unwrap();
            }
            _ => unreachable!(),
        }
        assert!(
            user_ops::complete_email_change(&db, &row)
                .await
                .unwrap()
                .is_none(),
            "{action}"
        );
        let response = reqwest::Client::new()
            .post(format!("{base}/api/v1/users/verify-email"))
            .json(&serde_json::json!({"token": action}))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 400, "{action}");
        assert_eq!(account(&db, id).await.email, before.email, "{action}");
        assert_eq!(event_count(&db, id, "email_changed").await, 0, "{action}");
        assert!(
            !links::replace_pending_email_change(
                &db,
                &before,
                "stale@example.test",
                "stale-hash",
                chrono::Utc::now() + chrono::Duration::hours(1)
            )
            .await
            .unwrap(),
            "{action}"
        );
        assert!(links::find_live(&db, "stale-hash").await.unwrap().is_none());
    }
}

#[tokio::test]
async fn a_benign_failed_login_does_not_invalidate_address_change_proof() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, id) = register_full(&base, "email-strike", "email-strike@example.test").await;
    let before = account(&db, id).await;
    user_ops::record_failed_login(&db, id, 5).await.unwrap();
    assert!(links::replace_pending_email_change(
        &db,
        &before,
        "next@example.test",
        "good-proof",
        chrono::Utc::now() + chrono::Duration::hours(1)
    )
    .await
    .unwrap());
    for _ in 1..5 {
        user_ops::record_failed_login(&db, id, 5).await.unwrap();
    }
    assert!(!links::replace_pending_email_change(
        &db,
        &before,
        "next@example.test",
        "locked-proof",
        chrono::Utc::now() + chrono::Duration::hours(1)
    )
    .await
    .unwrap());
    assert!(links::find_live(&db, "good-proof").await.unwrap().is_some());
}

#[tokio::test]
async fn address_change_is_single_use_and_invalidates_old_address_reset_links() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, id) = register_full(&base, "double-link", "double-link@example.test").await;
    let before = account(&db, id).await;
    let row = pending(&db, &before, "double-token").await;
    let reset = resets::create(
        &db,
        id,
        "old-address-token",
        chrono::Utc::now() + chrono::Duration::minutes(15),
    )
    .await
    .unwrap();
    assert!(
        links::take_live(&db, &row.token_hash)
            .await
            .unwrap()
            .is_none(),
        "a generic take must not detach address proof from its write"
    );
    let (a, b) = tokio::join!(
        user_ops::complete_email_change(&db, &row),
        user_ops::complete_email_change(&db, &row)
    );
    assert_eq!(
        usize::from(a.unwrap().is_some()) + usize::from(b.unwrap().is_some()),
        1
    );
    assert_eq!(account(&db, id).await.email, row.email);
    assert_eq!(event_count(&db, id, "email_changed").await, 1);
    assert!(
        resets::complete_password_reset(&db, reset.id, id, "attacker-password")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(account(&db, id).await.password_hash, before.password_hash);
    assert!(!resets::replace_for_current_account(
        &db,
        &before,
        "late-old-address",
        chrono::Utc::now() + chrono::Duration::minutes(15)
    )
    .await
    .unwrap());
    assert!(resets::find_by_hash(&db, "late-old-address")
        .await
        .unwrap()
        .is_none());
    let current = account(&db, id).await;
    assert!(resets::replace_for_current_account(
        &db,
        &current,
        "current-address",
        chrono::Utc::now() + chrono::Duration::minutes(15)
    )
    .await
    .unwrap());
}

#[tokio::test]
async fn failed_address_notification_rolls_back_address_token_and_reset_revocation() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, id) = register_full(&base, "rollback-email", "rollback-email@example.test").await;
    let before = account(&db, id).await;
    let row = pending(&db, &before, "rollback-token").await;
    resets::create(
        &db,
        id,
        "still-valid-reset",
        chrono::Utc::now() + chrono::Duration::minutes(15),
    )
    .await
    .unwrap();
    db.execute(Statement::from_string(db.get_database_backend(),
        "CREATE TRIGGER refuse_address_notice BEFORE INSERT ON notification_deliveries WHEN NEW.event_key = 'email_changed' BEGIN SELECT RAISE(ABORT, 'fixture outbox unavailable'); END")).await.unwrap();
    assert!(user_ops::complete_email_change(&db, &row).await.is_err());
    assert_eq!(account(&db, id).await.email, before.email);
    assert!(links::find_live(&db, &row.token_hash)
        .await
        .unwrap()
        .is_some());
    assert!(
        !resets::find_by_hash(&db, "still-valid-reset")
            .await
            .unwrap()
            .unwrap()
            .used
    );
    assert_eq!(event_count(&db, id, "email_changed").await, 0);
    db.execute(Statement::from_string(
        db.get_database_backend(),
        "DROP TRIGGER refuse_address_notice",
    ))
    .await
    .unwrap();
    assert!(user_ops::complete_email_change(&db, &row)
        .await
        .unwrap()
        .is_some());
}

#[tokio::test]
async fn factor_reset_and_its_notice_are_one_decision() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, id) = register_full(&base, "reset-atomic", "reset-atomic@example.test").await;
    user_ops::enable_mfa(&db, id).await.unwrap();
    rg_db::ops::mfa_backup_code_ops::set_codes(&db, id, &["backup-fixture".into()])
        .await
        .unwrap();
    rg_db::ops::passkey_credential_ops::create(
        &db,
        id,
        "credential",
        "fixture-json",
        "test key",
        "localhost",
    )
    .await
    .unwrap();
    let before = account(&db, id).await;
    let row = pending(&db, &before, "reset-atomic-token").await;
    db.execute(Statement::from_string(db.get_database_backend(),
        "CREATE TRIGGER refuse_factor_reset_notice BEFORE INSERT ON notification_deliveries WHEN NEW.event_key = 'admin_factors_reset' BEGIN SELECT RAISE(ABORT, 'fixture outbox unavailable'); END")).await.unwrap();
    assert!(rg_core::user::account::reset_mfa(&db, id).await.is_err());
    let rolled_back = account(&db, id).await;
    assert_eq!(rolled_back.session_version, before.session_version);
    assert!(rolled_back.mfa_enabled);
    assert_eq!(
        rg_db::ops::mfa_backup_code_ops::list_codes(&db, id)
            .await
            .unwrap()
            .iter()
            .filter(|c| !c.used)
            .count(),
        1
    );
    assert_eq!(
        rg_db::ops::passkey_credential_ops::list_by_user(&db, id)
            .await
            .unwrap()
            .len(),
        1
    );
    assert!(links::find_live(&db, &row.token_hash)
        .await
        .unwrap()
        .is_some());
    assert_eq!(event_count(&db, id, "admin_factors_reset").await, 0);
    db.execute(Statement::from_string(
        db.get_database_backend(),
        "DROP TRIGGER refuse_factor_reset_notice",
    ))
    .await
    .unwrap();
    let result = rg_core::user::account::reset_mfa(&db, id).await.unwrap();
    assert!(result.totp_removed);
    assert_eq!(result.passkeys_removed, 1);
    assert_eq!(result.user.session_version, before.session_version + 1);
    assert!(!result.user.mfa_enabled);
    assert!(links::find_live(&db, &row.token_hash)
        .await
        .unwrap()
        .is_none());
    assert!(rg_db::ops::mfa_backup_code_ops::list_codes(&db, id)
        .await
        .unwrap()
        .iter()
        .all(|c| c.used));
    assert_eq!(event_count(&db, id, "admin_factors_reset").await, 1);
    assert!(
        user_ops::record_successful_login_for_proved_account(&db, &before)
            .await
            .unwrap()
            .is_none()
    );
}
