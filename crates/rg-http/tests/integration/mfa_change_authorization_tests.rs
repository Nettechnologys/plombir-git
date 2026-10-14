//! Decision #5 (PR #5): who may change an account's second factor.
//!
//! An authenticator the owner never held locks them out at the next sign-in,
//! so arming, rotating or removing one must need more than a stolen session:
//! the password for an account that has one, sudo mode for one whose password
//! lives with its SSO provider or directory, and never a PAT. When the owner
//! is locked out anyway, an administrator (in sudo mode) takes every factor
//! off — passkeys included — and signs every session out.

use crate::common::{register_full, spawn_test_app_with_db};

const SECRET: &str = "test-secret-key";
const PASSWORD: &str = "Qz7$wRtm";

fn current_code(secret: &str) -> String {
    let bytes = totp_rs::Secret::Encoded(secret.to_string())
        .to_bytes()
        .unwrap();
    totp_rs::TOTP::new(
        totp_rs::Algorithm::SHA1,
        6,
        1,
        30,
        bytes,
        None,
        String::new(),
    )
    .unwrap()
    .generate_current()
    .unwrap()
}

async fn post(
    base: &str,
    path: &str,
    token: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1{path}"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or_default())
}

async fn session(db: &rg_db::DatabaseConnection, user_id: i64, sudo: bool) -> String {
    let user = rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .unwrap()
        .unwrap();
    if sudo {
        rg_core::auth::jwt::generate_authenticated_token(
            user.id,
            &user.username,
            user.session_version,
            SECRET,
            7,
        )
    } else {
        rg_core::auth::jwt::generate_token(user.id, &user.username, user.session_version, SECRET, 7)
    }
    .unwrap()
}

async fn setup_secret(base: &str, token: &str) -> String {
    let (status, body) = post(base, "/users/mfa/setup", token, serde_json::json!({})).await;
    assert_eq!(status, 200, "{body}");
    body["secret"].as_str().unwrap().to_string()
}

fn sudo_refusal(answer: &(u16, serde_json::Value)) -> bool {
    answer.0 == 403 && answer.1["error"]["reason"] == "sudo_required"
}

/// A local account confirms every enrolment with its password — the first
/// one too — and a PAT may not touch the factor at all.
#[tokio::test]
async fn a_local_account_confirms_every_change_with_its_password_and_a_pat_never_can() {
    let (base, db) = spawn_test_app_with_db().await;
    let (fresh, user_id) = register_full(&base, "mfa_local", "mfa_local@example.com").await;
    let plain = session(&db, user_id, false).await;

    let secret = setup_secret(&base, &plain).await;
    let (status, _) = post(
        &base,
        "/users/mfa/enable",
        &plain,
        serde_json::json!({ "code": current_code(&secret) }),
    )
    .await;
    assert_eq!(
        status, 400,
        "a first enrolment without the password must be refused"
    );
    let (status, _) = post(
        &base,
        "/users/mfa/enable",
        &plain,
        serde_json::json!({ "code": current_code(&secret), "password": "wrong" }),
    )
    .await;
    assert_eq!(status, 401);
    let (status, body) = post(
        &base,
        "/users/mfa/enable",
        &plain,
        serde_json::json!({ "code": current_code(&secret), "password": PASSWORD }),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, created) = post(
        &base,
        "/users/tokens",
        &fresh,
        serde_json::json!({ "name": "automation" }),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let pat = created["token"].as_str().unwrap().to_string();
    for path in [
        "/users/mfa/setup",
        "/users/mfa/enable",
        "/users/mfa/disable",
        "/users/mfa/backup/regenerate",
    ] {
        let (status, _) = post(
            &base,
            path,
            &pat,
            serde_json::json!({ "password": PASSWORD, "code": "000000" }),
        )
        .await;
        assert_eq!(status, 403, "a PAT reached {path}");
    }

    let (status, _) = post(&base, "/users/mfa/disable", &plain, serde_json::json!({})).await;
    assert_eq!(status, 400, "disabling without the password");
    let (status, _) = post(
        &base,
        "/users/mfa/disable",
        &plain,
        serde_json::json!({ "password": PASSWORD }),
    )
    .await;
    assert_eq!(status, 200);
}

/// An account with no password here — SSO or directory — changes its factor
/// only from sudo mode; a session without a fresh sign-in gets the reason the
/// client acts on, which is the acceptance case of decision #5.
#[tokio::test]
async fn a_passwordless_account_changes_its_factor_only_in_sudo_mode() {
    let (base, db) = spawn_test_app_with_db().await;
    let user =
        rg_db::ops::user_ops::create_user(&db, "mfa_sso", "mfa_sso@example.com", "", "SSO User")
            .await
            .unwrap();
    let stolen = session(&db, user.id, false).await;
    let fresh = session(&db, user.id, true).await;

    let answer = post(&base, "/users/mfa/setup", &stolen, serde_json::json!({})).await;
    assert!(
        sudo_refusal(&answer),
        "a stolen session started an enrolment: {answer:?}"
    );

    let secret = setup_secret(&base, &fresh).await;
    let answer = post(
        &base,
        "/users/mfa/enable",
        &stolen,
        serde_json::json!({ "code": current_code(&secret) }),
    )
    .await;
    assert!(
        sudo_refusal(&answer),
        "a stolen session armed a factor: {answer:?}"
    );
    let (status, body) = post(
        &base,
        "/users/mfa/enable",
        &fresh,
        serde_json::json!({ "code": current_code(&secret) }),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    for path in ["/users/mfa/backup/regenerate", "/users/mfa/disable"] {
        let answer = post(&base, path, &stolen, serde_json::json!({})).await;
        assert!(
            sudo_refusal(&answer),
            "a stolen session reached {path}: {answer:?}"
        );
    }
    let (status, _) = post(
        &base,
        "/users/mfa/backup/regenerate",
        &fresh,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200);
    let (status, _) = post(&base, "/users/mfa/disable", &fresh, serde_json::json!({})).await;
    assert_eq!(status, 200);
    assert!(
        !rg_db::ops::user_ops::find_by_id(&db, user.id)
            .await
            .unwrap()
            .unwrap()
            .mfa_enabled
    );
}

/// The administrator's reset takes every factor off — passkeys too — and
/// signs every session out; it needs sudo mode, refuses the admin's own
/// account and an account with nothing to reset, and is journalled.
#[tokio::test]
async fn an_administrator_resets_every_factor_from_sudo_mode() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, admin_id) = register_full(&base, "mfa_admin", "mfa_admin@example.com").await;
    rg_db::ops::user_ops::update_by_id(&db, admin_id, None, None, Some(true), None)
        .await
        .unwrap()
        .unwrap();
    let (target_token, target_id) =
        register_full(&base, "mfa_target", "mfa_target@example.com").await;
    let secret = setup_secret(&base, &target_token).await;
    let (status, _) = post(
        &base,
        "/users/mfa/enable",
        &target_token,
        serde_json::json!({ "code": current_code(&secret), "password": PASSWORD }),
    )
    .await;
    assert_eq!(status, 200);
    rg_db::ops::passkey_credential_ops::create(
        &db,
        target_id,
        "cred",
        "{}",
        "thief's key",
        "localhost",
    )
    .await
    .unwrap();
    let target_session = session(&db, target_id, false).await;

    let reset = format!("/admin/users/{target_id}/mfa/reset");
    let answer = post(
        &base,
        &reset,
        &session(&db, admin_id, false).await,
        serde_json::json!({}),
    )
    .await;
    assert!(
        sudo_refusal(&answer),
        "a stale admin session reset a factor: {answer:?}"
    );
    let admin = session(&db, admin_id, true).await;
    let (status, _) = post(
        &base,
        &format!("/admin/users/{admin_id}/mfa/reset"),
        &admin,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(
        status, 400,
        "an admin's own factors come off from their settings"
    );

    let (status, body) = post(&base, &reset, &admin, serde_json::json!({})).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(body["mfa_enabled"], false);
    assert!(
        rg_db::ops::passkey_credential_ops::list_by_user(&db, target_id)
            .await
            .unwrap()
            .is_empty()
    );
    let me = reqwest::Client::new()
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(&target_session)
        .send()
        .await
        .unwrap();
    assert_eq!(
        me.status().as_u16(),
        401,
        "the target's sessions survived the reset"
    );

    let (status, _) = post(&base, &reset, &admin, serde_json::json!({})).await;
    assert_eq!(status, 409, "nothing left to reset");

    let entry = rg_db::ops::audit_log_ops::list_paginated(
        &db,
        0,
        50,
        None,
        Some("admin.reset_mfa"),
        None,
        None,
        None,
    )
    .await
    .unwrap()
    .0;
    assert_eq!(entry.len(), 1);
    assert_eq!(entry[0].user_id, Some(admin_id));
    assert!(entry[0]
        .details
        .as_deref()
        .unwrap_or_default()
        .contains("\"passkeys_removed\":1"));
}

/// A PAT cannot delete the owner's passkeys.
#[tokio::test]
async fn a_pat_cannot_delete_a_passkey() {
    let (base, db) = spawn_test_app_with_db().await;
    let (fresh, user_id) = register_full(&base, "passkey_pat", "passkey_pat@example.com").await;
    let passkey =
        rg_db::ops::passkey_credential_ops::create(&db, user_id, "cred", "{}", "mine", "localhost")
            .await
            .unwrap();
    let (status, created) = post(
        &base,
        "/users/tokens",
        &fresh,
        serde_json::json!({ "name": "automation" }),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let response = reqwest::Client::new()
        .delete(format!("{base}/api/v1/users/passkeys/{}", passkey.id))
        .bearer_auth(created["token"].as_str().unwrap())
        .send()
        .await
        .unwrap();
    assert_eq!(response.status().as_u16(), 403);
    assert_eq!(
        rg_db::ops::passkey_credential_ops::list_by_user(&db, user_id)
            .await
            .unwrap()
            .len(),
        1
    );
}
