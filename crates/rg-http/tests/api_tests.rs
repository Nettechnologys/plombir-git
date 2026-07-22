mod common;

use common::{register_full, register_user, spawn_test_app, spawn_test_app_with_db};

// ── Health endpoint ──────────────────────────────────────────────

#[tokio::test]
async fn test_health_endpoint() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{}/health", base)).send().await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    assert!(body["checks"]["database"] == "ok");
    assert!(body["checks"]["filesystem"] == "ok");
}

// ── User registration ────────────────────────────────────────────

#[tokio::test]
async fn test_register_success() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/users/register", base))
        .json(&serde_json::json!({
            "username": "alice",
            "email": "alice@example.com",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 201);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("token").is_some());
    assert_eq!(body["username"], "alice");
    assert!(body["user_id"].is_number());
}

#[tokio::test]
async fn test_register_duplicate_username() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    // First registration
    let resp1 = client
        .post(format!("{}/api/v1/users/register", base))
        .json(&serde_json::json!({
            "username": "bob",
            "email": "bob@example.com",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp1.status(), 201);

    // Duplicate
    let resp2 = client
        .post(format!("{}/api/v1/users/register", base))
        .json(&serde_json::json!({
            "username": "bob",
            "email": "bob2@example.com",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();
    assert!(resp2.status() == 409 || resp2.status() == 400);
}

// ── Login ────────────────────────────────────────────────────────

#[tokio::test]
async fn test_login_success() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    register_user(&base, "charlie", "charlie@example.com", "Qz7$wRtm").await;

    let resp = client
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({
            "login": "charlie",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(body.get("token").is_some());
    assert_eq!(body["username"], "charlie");
}

#[tokio::test]
async fn star_and_watch_endpoints_accept_browser_auth_cookie() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    let token = register_user(
        &base,
        "cookie_repo_user",
        "cookie_repo_user@example.com",
        "Qz7$wRtm",
    )
    .await;

    let login = client
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({
            "login": "cookie_repo_user",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let cookie = login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let repo = client
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&token)
        .json(&serde_json::json!({"name": "cookie-auth-repo"}))
        .send()
        .await
        .unwrap();
    assert_eq!(repo.status(), 201);
    let repo_url = format!("{base}/api/v1/repos/cookie_repo_user/cookie-auth-repo");

    for endpoint in ["starred", "watch"] {
        let response = client
            .get(format!("{repo_url}/{endpoint}"))
            .header(reqwest::header::COOKIE, &cookie)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200, "GET {endpoint}");
    }
    let starred = client
        .put(format!("{repo_url}/star"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(starred.status(), 200);
    let watched = client
        .put(format!("{repo_url}/watch"))
        .header(reqwest::header::COOKIE, &cookie)
        .json(&serde_json::json!({"state": "watching"}))
        .send()
        .await
        .unwrap();
    assert_eq!(watched.status(), 200);
    let unwatched = client
        .delete(format!("{repo_url}/watch"))
        .header(reqwest::header::COOKIE, &cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(unwatched.status(), 200);
}

#[tokio::test]
async fn test_login_invalid_credentials() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    // Register first so user exists
    register_user(&base, "nonexistent", "nonexistent@example.com", "Qz7$wRtm").await;

    // Try wrong password
    let resp = client
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({
            "login": "nonexistent",
            "password": "wrong"
        }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}

// ── GET /users/me ────────────────────────────────────────────────

#[tokio::test]
async fn test_me_authenticated() {
    let base = spawn_test_app().await;
    let token = register_user(&base, "dana_test", "dana@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/users/me", base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["username"], "dana_test");
    assert_eq!(body["email"], "dana@example.com");
}

#[tokio::test]
async fn test_me_accepts_httponly_cookie_without_bearer() {
    let base = spawn_test_app().await;
    register_user(&base, "cookie_user", "cookie_user@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    let login_resp = client
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({
            "login": "cookie_user",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(login_resp.status(), 200);
    let auth_cookie = login_resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .expect("login should set auth cookie")
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .to_string();

    let resp = client
        .get(format!("{}/api/v1/users/me", base))
        .header(reqwest::header::COOKIE, auth_cookie)
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["username"], "cookie_user");
}

#[tokio::test]
async fn test_disable_mfa_rejects_wrong_password() {
    let (base, db) = spawn_test_app_with_db().await;
    let (token, user_id) = register_full(&base, "mfa_user", "mfa_user@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id, "totp")
        .await
        .unwrap();
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/users/mfa/disable", base))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "password": "wrong-password" }))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
    let user = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .unwrap()
        .unwrap();
    assert!(user.mfa_enabled);
}

#[tokio::test]
async fn test_mfa_verify_requires_a_primary_factor_challenge() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_token, user_id) =
        register_full(&base, "mfa_challenge", "mfa_challenge@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id, "totp")
        .await
        .unwrap();
    rg_db::ops::mfa_backup_code_ops::set_codes(&db, user_id, &["123456".to_string()])
        .await
        .unwrap();
    let client = reqwest::Client::new();

    let direct = client
        .post(format!("{}/api/v1/users/mfa/verify", base))
        .json(&serde_json::json!({
            "username": "mfa_challenge",
            "code": "000000"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(direct.status(), 401);

    let login = client
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({
            "login": "mfa_challenge",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 200);
    let challenge_cookie_header = login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(challenge_cookie_header.starts_with("ironforge_mfa_challenge="));
    assert!(challenge_cookie_header.contains("HttpOnly"));
    let challenge_cookie = challenge_cookie_header
        .split(';')
        .next()
        .unwrap()
        .to_string();
    let body: serde_json::Value = login.json().await.unwrap();
    assert_eq!(body["mfa_required"], true);
    assert_eq!(body["token"], "");

    let verified = client
        .post(format!("{}/api/v1/users/mfa/verify", base))
        .header(reqwest::header::COOKIE, &challenge_cookie)
        .json(&serde_json::json!({
            "username": "mfa_challenge",
            "code": "123456",
            "backup": true
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(verified.status(), 200);
    let set_cookies: Vec<_> = verified
        .headers()
        .get_all(reqwest::header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_string())
        .collect();
    assert!(set_cookies
        .iter()
        .any(|cookie| cookie.starts_with("ironforge_token=")));
    assert!(set_cookies.iter().any(|cookie| {
        cookie.starts_with("ironforge_mfa_challenge=") && cookie.contains("Max-Age=0")
    }));
}

#[tokio::test]
async fn test_password_failures_are_logged_and_lock_known_accounts() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_token, user_id) = register_full(&base, "lock_user", "lock_user@example.com").await;
    let client = reqwest::Client::new();

    for _ in 0..5 {
        let response = client
            .post(format!("{}/api/v1/users/login", base))
            .json(&serde_json::json!({
                "login": "lock_user",
                "password": "wrong-password"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 401);
    }

    let locked = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(locked.login_attempts, 5);
    assert!(locked.locked_until.is_some());
    assert!(
        rg_db::ops::login_log_ops::count_failed_since(
            &db,
            "lock_user",
            chrono::Utc::now() - chrono::Duration::minutes(1),
        )
        .await
        .unwrap()
            >= 5
    );

    let blocked = client
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({
            "login": "lock_user",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(blocked.status(), 401);

    rg_db::ops::user_ops::reset_login_failures(&db, user_id)
        .await
        .unwrap();
    let recovered = client
        .post(format!("{}/api/v1/users/login", base))
        .json(&serde_json::json!({
            "login": "lock_user",
            "password": "Qz7$wRtm"
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(recovered.status(), 200);
}

#[tokio::test]
async fn test_new_primary_factor_challenge_does_not_reset_mfa_failures() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_token, user_id) =
        register_full(&base, "mfa_lock_user", "mfa_lock_user@example.com").await;
    rg_db::ops::user_ops::enable_mfa(&db, user_id, "totp")
        .await
        .unwrap();
    let client = reqwest::Client::new();

    async fn login_challenge(client: &reqwest::Client, base: &str) -> String {
        let response = client
            .post(format!("{}/api/v1/users/login", base))
            .json(&serde_json::json!({
                "login": "mfa_lock_user",
                "password": "Qz7$wRtm"
            }))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), 200);
        response
            .headers()
            .get_all(reqwest::header::SET_COOKIE)
            .iter()
            .find_map(|value| {
                let value = value.to_str().ok()?;
                value
                    .starts_with("ironforge_mfa_challenge=")
                    .then(|| value.split(';').next().unwrap().to_string())
            })
            .unwrap()
    }

    async fn fail_mfa(client: &reqwest::Client, base: &str, cookie: &str) -> reqwest::StatusCode {
        client
            .post(format!("{}/api/v1/users/mfa/verify", base))
            .header(reqwest::header::COOKIE, cookie)
            .json(&serde_json::json!({
                "username": "mfa_lock_user",
                "code": "not-a-code",
                "backup": true
            }))
            .send()
            .await
            .unwrap()
            .status()
    }

    let first_challenge = login_challenge(&client, &base).await;
    for _ in 0..4 {
        assert_eq!(fail_mfa(&client, &base, &first_challenge).await, 401);
    }
    let refreshed_challenge = login_challenge(&client, &base).await;
    assert_eq!(fail_mfa(&client, &base, &refreshed_challenge).await, 401);

    let user = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.login_attempts, 5);
    assert!(user.locked_until.is_some());
}

// ── Repo CRUD ────────────────────────────────────────────────────

#[tokio::test]
async fn test_get_repo() {
    let base = spawn_test_app().await;
    let token = register_user(&base, "repogetter", "repogetter@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    // Create repo
    let create_resp = client
        .post(format!("{}/api/v1/repos", base.clone()))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "name": "get-test" }))
        .send()
        .await
        .unwrap();
    assert_eq!(create_resp.status(), 201);

    // Get by owner/name
    let resp = client
        .get(format!("{}/api/v1/repos/repogetter/get-test", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200, "GET /repos/repogetter/get-test failed");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["name"], "get-test");
}

#[tokio::test]
async fn test_list_repos() {
    let base = spawn_test_app().await;
    let token = register_user(&base, "listuser", "list@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    // Create two repos
    for name in &["alpha", "beta"] {
        let resp = client
            .post(format!("{}/api/v1/repos", base))
            .bearer_auth(&token)
            .json(&serde_json::json!({ "name": name }))
            .send()
            .await
            .unwrap();
        assert_eq!(resp.status(), 201, "failed to create {}", name);
    }

    // List repos
    let resp = client
        .get(format!("{}/api/v1/repos/listuser", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let repos = body
        .get("data")
        .and_then(|d| d.as_array())
        .or_else(|| body.as_array())
        .expect("expected array of repos");
    assert_eq!(repos.len(), 2);
}

#[tokio::test]
async fn test_star_repo() {
    let base = spawn_test_app().await;
    let token = register_user(&base, "staruser", "star@example.com", "Qz7$wRtm").await;
    let client = reqwest::Client::new();

    // Create repo
    let _ = client
        .post(format!("{}/api/v1/repos", base.clone()))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "name": "star-me" }))
        .send()
        .await
        .unwrap();

    // Star
    let resp = client
        .put(format!(
            "{}/api/v1/repos/staruser/star-me/star",
            base.clone()
        ))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert!(resp.status().is_success(), "star failed: {}", resp.status());

    // Get stargazers
    let resp = client
        .get(format!("{}/api/v1/repos/staruser/star-me/stargazers", base))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    let stargazers = body.get("data").and_then(|d| d.as_array()).unwrap();
    assert_eq!(stargazers.len(), 1);
    assert_eq!(stargazers[0]["user_id"], 1);
}

#[tokio::test]
async fn test_me_unauthenticated() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/users/me", base))
        .send()
        .await
        .unwrap();

    assert_eq!(resp.status(), 401);
}
