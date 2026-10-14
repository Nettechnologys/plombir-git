//! Sudo mode (decision #10, security audit finding #7): a session that has not
//! proved itself within ten minutes cannot mint a credential that outlives it.
//!
//! A login that proved a secret starts in sudo mode, which is what the
//! registration tokens these tests start from carry. The stolen session these
//! tests stand in for is an ordinary one, minted the way an older login's
//! token reads: valid, current, and outside any window.

use crate::common::{create_repo, register_full, spawn_test_app_with_db};

const SECRET: &str = "test-secret-key";
const PASSWORD: &str = "Qz7$wRtm";
const SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBBB sudo@plombir-git";

/// The session a thief holds: the account's own, valid, outside the window.
async fn ordinary_session(db: &rg_db::DatabaseConnection, user_id: i64) -> String {
    let user = rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .unwrap()
        .unwrap();
    rg_core::auth::jwt::generate_token(user.id, &user.username, user.session_version, SECRET, 7)
        .unwrap()
}

async fn post(
    client: &reqwest::Client,
    url: String,
    token: &str,
    body: serde_json::Value,
) -> (u16, serde_json::Value) {
    let response = client
        .post(url)
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .unwrap();
    let status = response.status().as_u16();
    (status, response.json().await.unwrap_or_default())
}

fn sudo_refusal(answer: &(u16, serde_json::Value)) -> bool {
    answer.0 == 403 && answer.1["error"]["reason"] == "sudo_required"
}

async fn login_attempts(db: &rg_db::DatabaseConnection, user_id: i64) -> i32 {
    rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .unwrap()
        .unwrap()
        .login_attempts
}

/// Every route that mints something outliving the session refuses a session
/// outside its window with the reason the client acts on — and the same
/// account, stepped up, gets past that refusal.
#[tokio::test]
async fn credential_routes_refuse_a_session_outside_its_sudo_window() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (fresh, user_id) = register_full(&base, "sudo_owner", "sudo_owner@example.com").await;
    create_repo(&base, &fresh, "deploys").await;
    rg_db::ops::user_ops::update_by_id(&db, user_id, None, None, Some(true), None)
        .await
        .unwrap()
        .unwrap();
    let (status, _) = post(
        &client,
        format!("{base}/api/v1/users/bots"),
        &fresh,
        serde_json::json!({ "username": "sudo_owner-bot" }),
    )
    .await;
    assert!(status == 201 || status == 200, "create a bot: {status}");
    let stale = ordinary_session(&db, user_id).await;

    let routes: Vec<(&str, String, serde_json::Value)> = vec![
        (
            "SSH key",
            format!("{base}/api/v1/users/ssh-keys"),
            serde_json::json!({ "title": "laptop", "key": SSH_KEY }),
        ),
        (
            "personal access token",
            format!("{base}/api/v1/users/tokens"),
            serde_json::json!({ "name": "ci" }),
        ),
        (
            "bot token",
            format!("{base}/api/v1/users/bots/sudo_owner-bot/tokens"),
            serde_json::json!({ "name": "deploy" }),
        ),
        (
            "deploy key",
            format!("{base}/api/v1/repos/sudo_owner/deploys/keys"),
            serde_json::json!({ "title": "deploy", "public_key": SSH_KEY, "read_only": false }),
        ),
        (
            "passkey registration",
            format!("{base}/api/v1/users/passkeys/register/start"),
            serde_json::json!({}),
        ),
        (
            "commit signing key",
            format!("{base}/api/v1/users/signing-keys"),
            serde_json::json!({ "title": "signing", "public_key": SSH_KEY }),
        ),
        (
            "SSO link",
            format!("{base}/api/v1/auth/sso/github/link"),
            serde_json::json!({}),
        ),
        (
            "runner registration",
            format!("{base}/api/v1/runners/register"),
            serde_json::json!({ "name": "r", "repository": "sudo_owner/deploys" }),
        ),
    ];
    for (what, url, body) in &routes {
        let answer = post(&client, url.clone(), &stale, body.clone()).await;
        assert!(
            sudo_refusal(&answer),
            "a session outside its window minted a {what}: {answer:?}"
        );
    }

    // The same account, stepped up: past the sudo refusal on every route
    // (some still refuse for their own reasons — a disabled provider, a
    // repeated key — which is not what this test is about).
    let (status, stepped) = post(
        &client,
        format!("{base}/api/v1/users/me/sudo"),
        &stale,
        serde_json::json!({ "password": PASSWORD }),
    )
    .await;
    assert_eq!(status, 200, "{stepped}");
    let elevated = stepped["token"].as_str().unwrap().to_string();
    for (what, url, body) in &routes {
        let answer = post(&client, url.clone(), &elevated, body.clone()).await;
        assert!(
            !sudo_refusal(&answer),
            "a stepped-up session was still refused a {what}: {answer:?}"
        );
    }
}

/// The step-up re-issues the same session, counts a wrong password as a
/// strike but not a missing one, and dies with the session it came from.
#[tokio::test]
async fn stepping_up_reissues_the_session_and_counts_only_real_guesses() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (_, user_id) = register_full(&base, "sudo_guess", "sudo_guess@example.com").await;
    let stale = ordinary_session(&db, user_id).await;
    let sudo = format!("{base}/api/v1/users/me/sudo");

    let (status, _) = post(
        &client,
        sudo.clone(),
        &stale,
        serde_json::json!({ "password": "wrong" }),
    )
    .await;
    assert_eq!(status, 401);
    assert_eq!(
        login_attempts(&db, user_id).await,
        1,
        "a wrong password is a strike"
    );
    let (status, _) = post(
        &client,
        sudo.clone(),
        &stale,
        serde_json::json!({ "password": "" }),
    )
    .await;
    assert_eq!(status, 400);
    assert_eq!(
        login_attempts(&db, user_id).await,
        1,
        "an empty password is a malformed request, not a guess"
    );

    let (status, body) = post(
        &client,
        sudo.clone(),
        &stale,
        serde_json::json!({ "password": PASSWORD }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let before = rg_core::auth::jwt::validate_token(&stale, SECRET).unwrap();
    let after =
        rg_core::auth::jwt::validate_token(body["token"].as_str().unwrap(), SECRET).unwrap();
    assert_eq!(
        (after.sub.as_str(), after.session_version, after.exp),
        (before.sub.as_str(), before.session_version, before.exp),
        "stepping up is not a new login"
    );
    assert!(after.sudo_active_at(chrono::Utc::now().timestamp()));

    // Logging out ends the session, and the window with it.
    let elevated = body["token"].as_str().unwrap().to_string();
    let logout = client
        .post(format!("{base}/api/v1/users/logout"))
        .bearer_auth(&elevated)
        .send()
        .await
        .unwrap();
    assert!(logout.status().is_success());
    let answer = post(
        &client,
        format!("{base}/api/v1/users/ssh-keys"),
        &elevated,
        serde_json::json!({ "title": "after logout", "key": SSH_KEY }),
    )
    .await;
    assert_eq!(
        answer.0, 401,
        "a logged-out session minted a key: {answer:?}"
    );
}

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

/// An account with a second factor steps up with it as well as the password,
/// and a PAT cannot step up at all.
#[tokio::test]
async fn the_second_factor_is_asked_for_and_a_pat_cannot_step_up() {
    let (base, db) = spawn_test_app_with_db().await;
    let client = reqwest::Client::new();
    let (fresh, user_id) = register_full(&base, "sudo_mfa", "sudo_mfa@example.com").await;

    let (status, setup) = post(
        &client,
        format!("{base}/api/v1/users/mfa/setup"),
        &fresh,
        serde_json::json!({}),
    )
    .await;
    assert_eq!(status, 200, "{setup}");
    let secret = setup["secret"].as_str().unwrap().to_string();
    let (status, enabled) = post(
        &client,
        format!("{base}/api/v1/users/mfa/enable"),
        &fresh,
        serde_json::json!({ "code": current_code(&secret), "password": PASSWORD }),
    )
    .await;
    assert_eq!(status, 200, "{enabled}");
    let backup = enabled["backup_codes"][0].as_str().unwrap().to_string();

    let stale = ordinary_session(&db, user_id).await;
    let sudo = format!("{base}/api/v1/users/me/sudo");
    let (status, _) = post(
        &client,
        sudo.clone(),
        &stale,
        serde_json::json!({ "password": PASSWORD }),
    )
    .await;
    assert_eq!(
        status, 400,
        "the password alone is not enough with MFA enrolled"
    );
    let (status, _) = post(
        &client,
        sudo.clone(),
        &stale,
        serde_json::json!({ "password": PASSWORD, "totp_code": "000000" }),
    )
    .await;
    assert_eq!(status, 401);
    let (status, body) = post(
        &client,
        sudo.clone(),
        &stale,
        serde_json::json!({ "password": PASSWORD, "backup_code": backup }),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let elevated = body["token"].as_str().unwrap().to_string();
    let (status, created) = post(
        &client,
        format!("{base}/api/v1/users/tokens"),
        &elevated,
        serde_json::json!({ "name": "automation" }),
    )
    .await;
    assert_eq!(status, 201, "{created}");
    let pat = created["token"].as_str().unwrap().to_string();
    let (status, _) = post(
        &client,
        sudo,
        &pat,
        serde_json::json!({ "password": PASSWORD }),
    )
    .await;
    assert_eq!(status, 403, "a personal access token stepped up");
}
