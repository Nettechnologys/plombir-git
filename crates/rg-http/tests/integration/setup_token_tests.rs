//! The one-time setup token that guards the bootstrap registration.
//!
//! Security audit finding #13: the first account on an empty instance becomes
//! its administrator whatever `[auth].registration` says, so before this token
//! the first start was a race — whoever reached `POST /users/register` first
//! owned the instance. These tests pin the contract over HTTP: no token, no
//! bootstrap; the right token (body field or header) creates the administrator
//! and retires the file; afterwards the configured mode rules and the token
//! buys nothing. `GET /instance` says when the field has to be shown.

use rg_core::user::registration::{RegistrationMode, SetupToken, SETUP_TOKEN_FILE_NAME};

use crate::common::{spawn_test_app_with_overrides, StateOverrides};

struct Fresh {
    base: String,
    db: rg_db::DatabaseConnection,
    secret: String,
    path: std::path::PathBuf,
    _dir: tempfile::TempDir,
}

async fn fresh_instance(mode: RegistrationMode) -> Fresh {
    let dir = tempfile::tempdir().expect("temp dir");
    let secret = SetupToken::generate_secret();
    let path = dir.path().join(SETUP_TOKEN_FILE_NAME);
    std::fs::write(&path, format!("{secret}\n")).expect("write the token file");
    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        registration: Some(mode),
        setup_token: Some(SetupToken::required(secret.clone(), Some(path.clone()))),
        ..Default::default()
    })
    .await;
    Fresh {
        base,
        db,
        secret,
        path,
        _dir: dir,
    }
}

async fn register(
    base: &str,
    username: &str,
    body_token: Option<&str>,
    header_token: Option<&str>,
) -> reqwest::Response {
    let mut body = serde_json::json!({
        "username": username,
        "email": format!("{username}@example.com"),
        "password": "Qz7$wRtm",
    });
    if let Some(token) = body_token {
        body["setup_token"] = serde_json::Value::String(token.to_string());
    }
    let mut request = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/register"))
        .json(&body);
    if let Some(token) = header_token {
        request = request.header("X-Setup-Token", token);
    }
    request.send().await.expect("request")
}

async fn setup_required(base: &str) -> bool {
    let info: serde_json::Value = reqwest::Client::new()
        .get(format!("{base}/api/v1/instance"))
        .send()
        .await
        .expect("GET /instance")
        .json()
        .await
        .expect("instance json");
    info["setup_required"]
        .as_bool()
        .expect("setup_required is a bool")
}

#[tokio::test]
async fn an_empty_instance_refuses_the_first_registration_without_the_token() {
    let fresh = fresh_instance(RegistrationMode::Closed).await;
    assert!(
        setup_required(&fresh.base).await,
        "the page must ask for the token"
    );

    let refused = register(&fresh.base, "stranger", None, None).await;
    assert_eq!(refused.status(), 403, "no token, no bootstrap");
    let body: serde_json::Value = refused.json().await.expect("error envelope");
    assert_eq!(body["error"]["code"], "FORBIDDEN", "envelope: {body}");
    let message = body["error"]["message"].as_str().unwrap_or_default();
    assert!(
        message.contains("setup token") && message.contains("create-admin"),
        "the refusal must point at the token and the CLI: {message}"
    );

    let wrong = register(&fresh.base, "stranger", Some("not-the-token"), None).await;
    assert_eq!(wrong.status(), 403, "a wrong token is no token");

    assert!(
        rg_db::ops::user_ops::find_by_username(&fresh.db, "stranger")
            .await
            .expect("query")
            .is_none(),
        "a refused bootstrap must write no row"
    );
    assert!(
        fresh.path.exists(),
        "a refused attempt must not spend the token"
    );
    assert!(setup_required(&fresh.base).await);
}

#[tokio::test]
async fn the_token_in_the_body_creates_the_administrator_and_is_retired() {
    let fresh = fresh_instance(RegistrationMode::Closed).await;

    let created = register(&fresh.base, "founder", Some(&fresh.secret), None).await;
    assert_eq!(
        created.status(),
        201,
        "the right token admits the first account"
    );
    let created: serde_json::Value = created.json().await.expect("auth response");
    let token = created["token"].as_str().expect("session token");

    let admin_listing = reqwest::Client::new()
        .get(format!("{}/api/v1/admin/users", fresh.base))
        .bearer_auth(token)
        .send()
        .await
        .expect("admin listing")
        .status();
    assert_eq!(
        admin_listing, 200,
        "the bootstrap account is the instance admin"
    );

    assert!(
        !fresh.path.exists(),
        "the token file is deleted once it was used"
    );
    assert!(
        !setup_required(&fresh.base).await,
        "no setup is pending any more"
    );

    // Closed mode after the bootstrap: refused, and the spent token is no key.
    let later = register(&fresh.base, "member", Some(&fresh.secret), None).await;
    assert_eq!(
        later.status(),
        403,
        "closed: the window shut behind the founder"
    );
    let later = register(&fresh.base, "member", None, None).await;
    assert_eq!(later.status(), 403);
}

#[tokio::test]
async fn the_token_in_the_header_works_too_and_open_mode_then_needs_none() {
    let fresh = fresh_instance(RegistrationMode::Open).await;

    let created = register(&fresh.base, "founder", None, Some(&fresh.secret)).await;
    assert_eq!(
        created.status(),
        201,
        "X-Setup-Token is the header spelling"
    );

    let member = register(&fresh.base, "member", None, None).await;
    assert_eq!(
        member.status(),
        201,
        "open: later registrations need no token"
    );
    let member_row = rg_db::ops::user_ops::find_by_username(&fresh.db, "member")
        .await
        .expect("query")
        .expect("member exists");
    assert!(
        !member_row.is_admin,
        "the bootstrap capability was spent on the founder"
    );
}

/// Six strangers race for the window with the token, as six copies of the
/// operator's own form submission would: one wins, the rest see a shut window.
#[tokio::test]
async fn concurrent_bootstrap_attempts_with_the_token_yield_exactly_one_admin() {
    let fresh = fresh_instance(RegistrationMode::Closed).await;
    let attempts = (0..6).map(|n| {
        let base = fresh.base.clone();
        let secret = fresh.secret.clone();
        async move { register(&base, &format!("racer{n}"), Some(&secret), None).await }
    });
    let statuses: Vec<u16> = futures::future::join_all(attempts)
        .await
        .into_iter()
        .map(|response| response.status().as_u16())
        .collect();
    assert_eq!(
        statuses.iter().filter(|status| **status == 201).count(),
        1,
        "exactly one bootstrap, got {statuses:?}"
    );
}
