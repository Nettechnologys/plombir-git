//! Verifies that a Personal Access Token authenticates REST API calls, not
//! just git-over-HTTP. The API handlers validate JWTs; a middleware translates
//! a PAT into an equivalent Bearer JWT so PAT-based API access works.

use crate::common::{register_full, register_user, spawn_test_app, spawn_test_app_with_db};

use base64::Engine as _;
use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter};

async fn create_pat(base: &str, jwt: &str) -> String {
    create_pat_with_scopes(base, jwt, None).await
}

async fn create_pat_with_scopes(base: &str, jwt: &str, scopes: Option<&str>) -> String {
    let client = reqwest::Client::new();
    let mut body = serde_json::json!({ "name": "api-cli" });
    if let Some(scopes) = scopes {
        body["scopes"] = serde_json::Value::String(scopes.to_string());
    }
    let resp = client
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "create token failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    body["token"].as_str().unwrap().to_string()
}

async fn listed_pat(base: &str, jwt: &str) -> serde_json::Value {
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(jwt)
        .send()
        .await
        .expect("list PATs");
    assert_eq!(response.status(), 200, "list tokens failed");
    response
        .json::<Vec<serde_json::Value>>()
        .await
        .expect("token list is JSON")
        .into_iter()
        .find(|token| token["name"] == "api-cli")
        .expect("created PAT appears in the token list")
}

fn assert_same_instant(actual: &serde_json::Value, expected: &str) {
    let actual = chrono::DateTime::parse_from_rfc3339(
        actual.as_str().expect("expiration is an RFC 3339 string"),
    )
    .expect("response expiration parses");
    let expected = chrono::DateTime::parse_from_rfc3339(expected).expect("fixture parses");
    assert_eq!(actual, expected);
}

/// A supplied expiration is a capability boundary: malformed input must not
/// silently grant the stronger, non-expiring credential. Exercise the routed
/// handler and inspect the database so a superficially correct 400 cannot hide
/// a row or a raw PAT created before the refusal.
#[tokio::test]
async fn pat_expiration_distinguishes_malformed_valid_and_absent_values() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "patexpiry", "patexpiry@example.com").await;
    let client = reqwest::Client::new();
    let endpoint = format!("{base}/api/v1/users/tokens");

    let rejected = client
        .post(&endpoint)
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "name": "must-not-exist",
            "expires_at": "2030-99-99"
        }))
        .send()
        .await
        .expect("reject malformed expiration");
    assert_eq!(rejected.status(), 400);
    let rejection_body = rejected.text().await.expect("rejection body");
    assert!(rejection_body.contains("expires_at"), "{rejection_body}");
    assert!(
        !rejection_body.contains("ifp_"),
        "a rejected request exposed a raw PAT: {rejection_body}"
    );
    assert_eq!(
        rg_db::entities::access_token::Entity::find()
            .filter(rg_db::entities::access_token::Column::UserId.eq(user_id))
            .count(&db)
            .await
            .expect("count PATs after rejected create"),
        0,
        "a rejected expiration must not leave an access-token row"
    );

    let offset_expiration = "2030-01-02T03:04:05+05:30";
    let valid = client
        .post(&endpoint)
        .bearer_auth(&jwt)
        .json(&serde_json::json!({
            "name": "bounded",
            "expires_at": offset_expiration
        }))
        .send()
        .await
        .expect("create bounded PAT");
    assert_eq!(valid.status(), 201);
    let valid = valid
        .json::<serde_json::Value>()
        .await
        .expect("bounded PAT body");
    assert_same_instant(&valid["expires_at"], offset_expiration);

    let unbounded = client
        .post(&endpoint)
        .bearer_auth(&jwt)
        .json(&serde_json::json!({ "name": "unbounded" }))
        .send()
        .await
        .expect("create explicitly unbounded PAT");
    assert_eq!(unbounded.status(), 201);
    let unbounded = unbounded
        .json::<serde_json::Value>()
        .await
        .expect("unbounded PAT body");
    assert_eq!(unbounded["expires_at"], serde_json::Value::Null);

    assert_eq!(
        rg_db::entities::access_token::Entity::find()
            .filter(rg_db::entities::access_token::Column::UserId.eq(user_id))
            .count(&db)
            .await
            .expect("count accepted PATs"),
        2,
        "only the valid bounded and explicitly unbounded PATs should exist"
    );
}

#[tokio::test]
async fn pat_scopes_are_enforced_by_api_family() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patscope", "patscope@example.com", "Qz7$wRtm").await;
    let user_pat = create_pat_with_scopes(&base, &jwt, Some("user")).await;
    let repo_pat = create_pat_with_scopes(&base, &jwt, Some("repo")).await;
    let combined_pat = create_pat_with_scopes(&base, &jwt, Some("user, repo")).await;
    let client = reqwest::Client::new();

    let denied_repo = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(&user_pat)
        .json(&serde_json::json!({ "name": "scope-denied" }))
        .send()
        .await
        .unwrap();
    assert_eq!(denied_repo.status(), 403);

    let allowed_user = client
        .get(format!("{}/api/v1/users/me", base))
        .bearer_auth(&user_pat)
        .send()
        .await
        .unwrap();
    assert_eq!(allowed_user.status(), 200);

    let denied_user = client
        .get(format!("{}/api/v1/users/me", base))
        .bearer_auth(&repo_pat)
        .send()
        .await
        .unwrap();
    assert_eq!(denied_user.status(), 403);

    let allowed_repo = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(&combined_pat)
        .json(&serde_json::json!({ "name": "scope-allowed" }))
        .send()
        .await
        .unwrap();
    assert_eq!(allowed_repo.status(), 201);
}

const TEST_SSH_KEY: &str =
    "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIAICAgICAgICAgICAgICAgICAgICAgICAgIC pat-scope-test";

#[tokio::test]
async fn pat_cannot_issue_a_broader_pat_or_ssh_key_but_a_browser_session_can() {
    let (base, db) = spawn_test_app_with_db().await;
    let (jwt, user_id) = register_full(&base, "patissuer", "patissuer@example.com").await;
    let pat = create_pat_with_scopes(&base, &jwt, Some("user")).await;
    let client = reqwest::Client::new();
    let token_url = format!("{base}/api/v1/users/tokens");
    let ssh_url = format!("{base}/api/v1/users/ssh-keys");
    let token_body = serde_json::json!({ "name": "expanded", "scopes": "repo,admin" });
    let ssh_body = serde_json::json!({ "title": "test", "public_key": TEST_SSH_KEY });

    for response in [
        client
            .post(&token_url)
            .bearer_auth(&pat)
            .json(&token_body)
            .send()
            .await
            .unwrap(),
        client
            .post(&ssh_url)
            .bearer_auth(&pat)
            .json(&ssh_body)
            .send()
            .await
            .unwrap(),
    ] {
        assert_eq!(response.status(), 403);
        assert!(response
            .text()
            .await
            .unwrap()
            .contains("credential creation requires a login session"));
    }
    assert_eq!(
        rg_db::entities::access_token::Entity::find()
            .filter(rg_db::entities::access_token::Column::UserId.eq(user_id))
            .count(&db)
            .await
            .unwrap(),
        1,
        "the refused request must not insert a PAT"
    );
    assert_eq!(
        rg_db::entities::ssh_key::Entity::find()
            .filter(rg_db::entities::ssh_key::Column::UserId.eq(user_id))
            .count(&db)
            .await
            .unwrap(),
        0,
        "the refused request must not insert an SSH key"
    );

    let cookie = format!("plombir_git_token={jwt}");
    let created_pat = client
        .post(&token_url)
        .header("Cookie", &cookie)
        .json(&token_body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        created_pat.status(),
        201,
        "browser session can create a PAT"
    );
    let created_ssh = client
        .post(&ssh_url)
        .header("Cookie", &cookie)
        .json(&ssh_body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        created_ssh.status(),
        201,
        "browser session can create an SSH key"
    );

    // When both credentials are sent, the PAT still owns the request's grant.
    let mixed = client
        .post(&token_url)
        .header("Cookie", &cookie)
        .bearer_auth(&pat)
        .json(&token_body)
        .send()
        .await
        .unwrap();
    assert_eq!(mixed.status(), 403);
    assert!(mixed
        .text()
        .await
        .unwrap()
        .contains("credential creation requires a login session"));
}

/// A `user`-scoped PAT reaches every `/users/mfa/*` door it used to, and is
/// refused at each one that changes the factor (security audit finding #6).
///
/// The second factor is what a stolen credential is stopped by, so it must not
/// be something a stolen credential can set: a token that enrols an
/// authenticator its owner never held locks the owner out for good, because a
/// password reset of an MFA account ends at the second factor. Reading the
/// backup-code *status* reveals nothing and stays open to the token.
#[tokio::test]
async fn pat_cannot_change_the_second_factor() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patmfa", "patmfa@example.com", "Qz7$wRtm").await;
    let cookie = format!("plombir_git_token={jwt}");
    let user_pat = create_pat_with_scopes(&base, &jwt, Some("user")).await;
    let client = reqwest::Client::new();

    for (route, body) in [
        ("/users/mfa/setup", serde_json::json!({})),
        (
            "/users/mfa/enable",
            serde_json::json!({ "code": "000000", "password": "Qz7$wRtm" }),
        ),
        (
            "/users/mfa/backup/regenerate",
            serde_json::json!({ "password": "Qz7$wRtm" }),
        ),
        (
            "/users/mfa/disable",
            serde_json::json!({ "password": "Qz7$wRtm" }),
        ),
    ] {
        let denied = client
            .post(format!("{base}/api/v1{route}"))
            .bearer_auth(&user_pat)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(denied.status(), 403, "a PAT reached POST {route}");
        assert!(
            denied
                .text()
                .await
                .unwrap()
                .contains("credential creation requires a login session"),
            "POST {route} must say why the token is refused"
        );
    }

    let status_with_pat = client
        .get(format!("{base}/api/v1/users/mfa/backup"))
        .bearer_auth(&user_pat)
        .send()
        .await
        .unwrap();
    assert_eq!(
        status_with_pat.status(),
        200,
        "the backup-code status is a read and stays open to a user-scoped token"
    );

    let started = client
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        started.status(),
        200,
        "browser session starts MFA enrolment"
    );
}

#[tokio::test]
async fn pat_cannot_issue_bot_or_deploy_credentials_or_start_passkey_registration() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patlateral", "patlateral@example.com", "Qz7$wRtm").await;
    let cookie = format!("plombir_git_token={jwt}");
    let user_pat = create_pat_with_scopes(&base, &jwt, Some("user")).await;
    let repo_pat = create_pat_with_scopes(&base, &jwt, Some("repo")).await;
    let client = reqwest::Client::new();

    let bot = client
        .post(format!("{base}/api/v1/users/bots"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({ "username": "patlateralbot" }))
        .send()
        .await
        .unwrap();
    assert_eq!(bot.status(), 201, "browser session creates the bot fixture");
    let bot_url = format!("{base}/api/v1/users/bots/patlateralbot/tokens");
    let bot_body = serde_json::json!({ "name": "bot-grant" });
    let denied_bot = client
        .post(&bot_url)
        .bearer_auth(&user_pat)
        .json(&bot_body)
        .send()
        .await
        .unwrap();
    assert_eq!(denied_bot.status(), 403);
    assert!(denied_bot
        .text()
        .await
        .unwrap()
        .contains("credential creation requires a login session"));
    let created_bot = client
        .post(&bot_url)
        .header("Cookie", &cookie)
        .json(&bot_body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        created_bot.status(),
        201,
        "browser session creates a bot PAT"
    );

    let repo = client
        .post(format!("{base}/api/v1/repos"))
        .header("Cookie", &cookie)
        .json(&serde_json::json!({ "name": "pat-grant" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        repo.status(),
        201,
        "browser session creates the repository fixture"
    );
    let deploy_url = format!("{base}/api/v1/repos/patlateral/pat-grant/keys");
    let deploy_body =
        serde_json::json!({ "title": "deploy", "public_key": TEST_SSH_KEY, "read_only": false });
    let denied_deploy = client
        .post(&deploy_url)
        .bearer_auth(&repo_pat)
        .json(&deploy_body)
        .send()
        .await
        .unwrap();
    assert_eq!(denied_deploy.status(), 403);
    assert!(denied_deploy
        .text()
        .await
        .unwrap()
        .contains("credential creation requires a login session"));
    let created_deploy = client
        .post(&deploy_url)
        .header("Cookie", &cookie)
        .json(&deploy_body)
        .send()
        .await
        .unwrap();
    assert_eq!(
        created_deploy.status(),
        201,
        "browser session creates a deploy key"
    );

    let passkey_url = format!("{base}/api/v1/users/passkeys/register/start");
    let denied_passkey = client
        .post(&passkey_url)
        .bearer_auth(&user_pat)
        .header(reqwest::header::HOST, "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(denied_passkey.status(), 403);
    assert!(denied_passkey
        .text()
        .await
        .unwrap()
        .contains("credential creation requires a login session"));
    let started_passkey = client
        .post(&passkey_url)
        .header("Cookie", &cookie)
        .header(reqwest::header::HOST, "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(
        started_passkey.status(),
        200,
        "browser session starts passkey registration"
    );
    let finish_url = format!("{base}/api/v1/users/passkeys/register/finish");
    let finish_body = serde_json::json!({
        "name": "bogus",
        "credential": {
            "id": "AAAA",
            "rawId": "AAAA",
            "type": "public-key",
            "response": {
                "attestationObject": "AAAA",
                "clientDataJSON": "AAAA"
            }
        }
    });
    let denied_finish = client
        .post(&finish_url)
        .bearer_auth(&user_pat)
        .json(&finish_body)
        .send()
        .await
        .unwrap();
    assert_eq!(denied_finish.status(), 403);
    let session_finish = client
        .post(&finish_url)
        .header("Cookie", &cookie)
        .json(&finish_body)
        .send()
        .await
        .unwrap();
    assert_eq!(session_finish.status(), 400);
    assert!(session_finish
        .text()
        .await
        .unwrap()
        .contains("passkey registration challenge missing or expired"));
}

#[tokio::test]
async fn unknown_pat_scope_is_rejected_at_creation() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patscope2", "patscope2@example.com", "Qz7$wRtm").await;
    let response = reqwest::Client::new()
        .post(format!("{}/api/v1/users/tokens", base))
        .bearer_auth(jwt)
        .json(&serde_json::json!({
            "name": "bad-scope",
            "scopes": "repo,delete_everything"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(response.status(), 400);
}

/// Creating a repo requires auth; do it with a PAT as a Bearer token.
#[tokio::test]
async fn pat_authenticates_api_via_bearer() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patapi1", "patapi1@example.com", "Qz7$wRtm").await;
    let pat = create_pat(&base, &jwt).await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth(&pat) // PAT, not JWT
        .json(&serde_json::json!({ "name": "via-pat" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 201, "PAT should authenticate repo creation");
}

/// The account-token API promises a live `last_used_at`, so drive the shared
/// production resolver through REST and then read the same field back through
/// its public API. `resolve_pat` is also the sole PAT resolver for git HTTP and
/// OCI Basic auth.
#[tokio::test]
async fn successful_pat_auth_updates_the_api_visible_last_used_at() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "pattouch", "pattouch@example.com", "Qz7$wRtm").await;
    let pat = create_pat_with_scopes(&base, &jwt, Some("user")).await;
    assert_eq!(
        listed_pat(&base, &jwt).await["last_used_at"],
        serde_json::Value::Null
    );

    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(&pat)
        .send()
        .await
        .expect("authenticate with PAT");
    assert_eq!(response.status(), 200, "PAT should authenticate the API");

    assert!(
        listed_pat(&base, &jwt).await["last_used_at"].is_string(),
        "a successful PAT authentication must update the public usage timestamp"
    );
}

/// Same, but the PAT presented via HTTP Basic auth (`user:token`).
#[tokio::test]
async fn pat_authenticates_api_via_basic() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "patapi2", "patapi2@example.com", "Qz7$wRtm").await;
    let pat = create_pat(&base, &jwt).await;

    let client = reqwest::Client::new();
    let basic = base64::engine::general_purpose::STANDARD.encode(format!("patapi2:{}", pat));
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .header("Authorization", format!("Basic {}", basic))
        .json(&serde_json::json!({ "name": "via-basic" }))
        .send()
        .await
        .unwrap();
    assert_eq!(
        resp.status(),
        201,
        "PAT via Basic auth should authenticate the API"
    );
}

/// An invalid/garbage token must NOT authenticate.
#[tokio::test]
async fn invalid_token_is_rejected_by_api() {
    let base = spawn_test_app().await;
    let _ = register_user(&base, "patapi3", "patapi3@example.com", "Qz7$wRtm").await;

    let client = reqwest::Client::new();
    let resp = client
        .post(format!("{}/api/v1/repos", base))
        .bearer_auth("not-a-real-token")
        .json(&serde_json::json!({ "name": "nope" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401, "garbage token must be rejected");
}
