//! Sensitive account operations require proof of the currently enrolled factor.
use crate::common::{register_full, spawn_test_app_with_db};
use serde_json::{json, Value};
const PASSWORD: &str = "Qz7$wRtm";
const SECRET: &str = "test-secret-key";

async fn send(base: &str, method: &str, path: &str, token: &str, body: Value) -> (u16, Value) {
    let response = reqwest::Client::new()
        .request(method.parse().unwrap(), format!("{base}/api/v1{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    (
        response.status().as_u16(),
        response.json().await.unwrap_or_default(),
    )
}
fn code(secret: &str) -> String {
    totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        totp_rs::Secret::Encoded(secret.to_owned())
            .to_bytes()
            .unwrap(),
        None,
        String::new(),
    )
    .unwrap()
    .generate_current()
    .unwrap()
}
async fn enrol(base: &str, token: &str) -> (String, Value) {
    let (status, setup) = send(base, "POST", "/users/mfa/setup", token, json!({})).await;
    assert_eq!(status, 200, "{setup}");
    let secret = setup["secret"].as_str().unwrap().to_owned();
    let (status, enabled) = send(
        base,
        "POST",
        "/users/mfa/enable",
        token,
        json!({"password": PASSWORD, "code": code(&secret)}),
    )
    .await;
    assert_eq!(status, 200, "{enabled}");
    (secret, enabled["backup_codes"].clone())
}
async fn elevate(base: &str, token: &str, backup: &Value) -> String {
    let (status, body) = send(
        base,
        "POST",
        "/users/me/sudo",
        token,
        json!({"password": PASSWORD, "backup_code": backup}),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    body["token"].as_str().unwrap().to_owned()
}
fn refused(answer: &(u16, Value)) {
    assert_eq!(answer.0, 403, "{answer:?}");
    assert_eq!(answer.1["error"]["reason"], "sudo_required", "{answer:?}");
}

#[tokio::test]
async fn password_email_deletion_and_admin_grants_require_current_factor_proof() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, id) = register_full(&base, "sensitive", "sensitive@example.com").await;
    let (_, target) = register_full(&base, "grant_target", "target@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, id, None, None, Some(true), None)
        .await
        .unwrap();
    let (_, codes) = enrol(&base, &token).await;
    for (method, path, body) in [
        (
            "PUT",
            "/users/me/password".to_owned(),
            json!({"current_password": PASSWORD, "new_password":"Changed-password-27!"}),
        ),
        (
            "POST",
            "/users/me/email".to_owned(),
            json!({"password":PASSWORD,"email":"changed@example.com"}),
        ),
        (
            "DELETE",
            "/users/me".to_owned(),
            json!({"password": PASSWORD}),
        ),
        (
            "PATCH",
            format!("/admin/users/{target}"),
            json!({"is_admin":true}),
        ),
    ] {
        refused(&send(&base, method, &path, &token, body).await);
    }
    let user = rg_db::ops::user_ops::find_by_id(&db, id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.email, "sensitive@example.com");
    assert!(user.is_usable() && user.mfa_enabled);
    assert!(
        !rg_db::ops::user_ops::find_by_id(&db, target)
            .await
            .unwrap()
            .unwrap()
            .is_admin
    );
    let elevated = elevate(&base, &token, &codes[0]).await;
    let answer = send(
        &base,
        "PUT",
        "/users/me/password",
        &elevated,
        json!({"current_password": PASSWORD, "new_password":"Changed-password-27!"}),
    )
    .await;
    assert_eq!(answer.0, 200, "{answer:?}");
    let claims =
        rg_core::auth::jwt::validate_token(answer.1["token"].as_str().unwrap(), SECRET).unwrap();
    assert!(
        claims.sudo_exp.is_none() && claims.sudo_mfa.is_none(),
        "password replacement grants no new sudo proof"
    );
}

#[tokio::test]
async fn a_passwordless_account_cannot_be_deleted_with_just_a_stolen_session() {
    let (base, db) = spawn_test_app_with_db().await;
    let user = rg_db::ops::user_ops::create_user(&db, "no_password", "sso@example.com", "", "SSO")
        .await
        .unwrap();
    // Passwordless provider accounts use the username confirmation in addition to sudo.
    use sea_orm::{ActiveModelTrait, Set};
    let mut update: rg_db::entities::user::ActiveModel = user.clone().into();
    update.auth_provider = Set("oidc".into());
    update.update(&db).await.unwrap();
    let plain = rg_core::auth::jwt::generate_token(
        user.id,
        &user.username,
        user.session_version,
        SECRET,
        7,
    )
    .unwrap();
    refused(
        &send(
            &base,
            "DELETE",
            "/users/me",
            &plain,
            json!({"confirm_username": user.username}),
        )
        .await,
    );
    let fresh = rg_core::auth::jwt::generate_authenticated_token(
        user.id,
        &user.username,
        user.session_version,
        SECRET,
        7,
    )
    .unwrap();
    let answer = send(
        &base,
        "DELETE",
        "/users/me",
        &fresh,
        json!({"confirm_username": user.username}),
    )
    .await;
    assert_eq!(answer.0, 200, "{answer:?}");
}

#[tokio::test]
async fn rotation_and_reenrolment_retire_old_sudo_proof_but_not_the_login_session() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, id) = register_full(&base, "rotate_proof", "rotate@example.com").await;
    let (_, codes) = enrol(&base, &token).await;
    let first = elevate(&base, &token, &codes[0]).await;
    let (_, rotated_codes) = enrol(&base, &first).await;
    refused(
        &send(
            &base,
            "POST",
            "/users/tokens",
            &first,
            json!({"name":"old-proof"}),
        )
        .await,
    );
    let second = elevate(&base, &first, &rotated_codes[0]).await;
    let user = rg_db::ops::user_ops::find_by_id(&db, id)
        .await
        .unwrap()
        .unwrap();
    let claims = rg_core::auth::jwt::validate_token(&second, SECRET).unwrap();
    assert!(claims.sudo_covers_mfa(user.totp_secret.as_deref(), SECRET));
    assert_eq!(
        send(&base, "POST", "/users/mfa/disable", &second, json!({}))
            .await
            .0,
        200
    );
    let (_, new_codes) = enrol(&base, &second).await;
    refused(
        &send(
            &base,
            "POST",
            "/users/mfa/backup/regenerate",
            &second,
            json!({}),
        )
        .await,
    );
    let third = elevate(&base, &second, &new_codes[0]).await;
    assert_eq!(
        send(
            &base,
            "POST",
            "/users/tokens",
            &third,
            json!({"name":"new-proof"})
        )
        .await
        .0,
        201
    );
    assert_eq!(
        send(&base, "GET", "/users/me", &token, json!({})).await.0,
        200,
        "existing sessions still permit ordinary work"
    );
}

#[tokio::test]
async fn enrolment_commits_only_the_verified_snapshot_and_only_once() {
    let (_base, db) = spawn_test_app_with_db().await;
    let user =
        rg_db::ops::user_ops::create_user(&db, "cas_factor", "cas@example.com", "hash", "CAS")
            .await
            .unwrap();
    rg_db::ops::user_ops::stage_pending_totp_secret(&db, user.id, "encrypted-first")
        .await
        .unwrap();
    let first = rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .unwrap()
        .unwrap();
    rg_db::ops::user_ops::stage_pending_totp_secret(&db, user.id, "encrypted-second")
        .await
        .unwrap();
    let codes = rg_db::ops::mfa_backup_code_ops::generate_codes(10);
    assert!(
        !rg_db::ops::user_ops::complete_mfa_enrolment(&db, &first, &codes)
            .await
            .unwrap()
    );
    let second = rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!second.mfa_enabled && second.totp_secret.is_none());
    assert_eq!(
        second.pending_totp_secret.as_deref(),
        Some("encrypted-second")
    );
    assert!(
        rg_db::ops::user_ops::complete_mfa_enrolment(&db, &second, &codes)
            .await
            .unwrap()
    );
    assert!(
        !rg_db::ops::user_ops::complete_mfa_enrolment(&db, &second, &codes)
            .await
            .unwrap()
    );
    let stored = rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stored.totp_secret.as_deref(), Some("encrypted-second"));
    rg_db::ops::user_ops::stage_pending_totp_secret(&db, user.id, "encrypted-third")
        .await
        .unwrap();
    let stale = rg_db::ops::user_ops::find_by_id(&db, user.id)
        .await
        .unwrap()
        .unwrap();
    rg_db::ops::user_ops::invalidate_sessions(&db, user.id)
        .await
        .unwrap();
    assert!(
        !rg_db::ops::user_ops::complete_mfa_enrolment(&db, &stale, &codes)
            .await
            .unwrap()
    );
}
