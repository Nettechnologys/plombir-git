use crate::common::{register_full, register_user, spawn_test_app, spawn_test_app_with_db};

// ── Health endpoint ──────────────────────────────────────────────

#[tokio::test]
async fn test_health_endpoint() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let resp = client.get(format!("{}/health", base)).send().await.unwrap();

    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["status"], "ok");
    // `/health` is public: an anonymous caller gets the verdict, not the report.
    assert!(
        body.get("checks").is_none(),
        "anonymous /health must not carry dependency detail: {body}"
    );
    assert!(
        body.get("version").is_none() && body.get("commit").is_none(),
        "anonymous /health must not fingerprint the build: {body}"
    );
}

/// The detection side of the same rule: an instance admin gets the full report,
/// a signed-in non-admin gets exactly what an anonymous caller gets — `?verbose=1`
/// is not a bypass.
#[tokio::test]
async fn health_details_are_for_the_instance_admin_only() {
    use crate::common::register_full;

    let base = spawn_test_app().await;
    let client = reqwest::Client::new();
    // The first registration on a fresh instance is the founder (admin).
    let (admin_token, _) = register_full(&base, "health-admin", "health-admin@example.com").await;
    let (member_token, _) = register_full(&base, "health-member", "health-member@example.com").await;

    let anonymous: serde_json::Value = client
        .get(format!("{base}/health"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(anonymous.get("checks").is_none(), "{anonymous}");

    let admin: serde_json::Value = client
        .get(format!("{base}/health?verbose=1"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(admin["status"], "ok");
    assert_eq!(admin["checks"]["database"], "ok");
    assert_eq!(admin["checks"]["filesystem"], "ok");
    assert!(
        admin.get("version").is_some() && admin.get("commit").is_some(),
        "the admin report names the build: {admin}"
    );

    let member: serde_json::Value = client
        .get(format!("{base}/health?verbose=1"))
        .bearer_auth(&member_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(
        member.get("checks").is_none() && member.get("version").is_none(),
        "verbose=1 must not substitute for the admin gate: {member}"
    );
}

/// card_0d7755e0dfe0: an unreachable SMTP relay stops outgoing mail and
/// nothing else, so it must not fail the probes an orchestrator acts on. It
/// used to turn `/health` into a 503 — an unhealthy container to Docker, a
/// backend that is not up to the web UI's readiness check.
#[tokio::test]
async fn an_unreachable_smtp_relay_fails_no_probe() {
    let (base, _db) = crate::common::spawn_test_app_with_overrides(crate::common::StateOverrides {
        // Port 9 (discard) on loopback: nothing listens, the connect fails.
        smtp_config: Some(rg_core::email::SmtpConfig::new(
            "127.0.0.1",
            9,
            "mailer",
            "secret",
            "noreply@example.com",
        )),
        ..Default::default()
    })
    .await;
    let client = reqwest::Client::new();

    for probe in ["/livez", "/readyz", "/health"] {
        let resp = client.get(format!("{base}{probe}")).send().await.unwrap();
        assert_eq!(resp.status(), 200, "{probe}");
        let body: serde_json::Value = resp.json().await.unwrap();
        assert_eq!(body["status"], "ok", "{probe}: {body}");
    }

    // The report still names the relay as down: reported, not decisive — to an
    // instance admin, the only caller the SMTP check is for now.
    let (admin_token, _) =
        crate::common::register_full(&base, "smtp-admin", "smtp-admin@example.com").await;
    let body: serde_json::Value = client
        .get(format!("{base}/health"))
        .bearer_auth(&admin_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_ne!(body["checks"]["smtp"], "ok", "{body}");
    assert!(
        body.get("phase").is_none(),
        "an internal phase number is not health: {body}"
    );
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
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
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
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
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
    assert!(challenge_cookie_header.starts_with("plombir_git_mfa_challenge="));
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
        .any(|cookie| cookie.starts_with("plombir_git_token=")));
    assert!(set_cookies.iter().any(|cookie| {
        cookie.starts_with("plombir_git_mfa_challenge=") && cookie.contains("Max-Age=0")
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
        rg_db::ops::login_log_ops::list_paginated(
            &db,
            1,
            50,
            Some("lock_user"),
            None,
            Some(false),
            Some(chrono::Utc::now() - chrono::Duration::minutes(1)),
            None,
        )
        .await
        .unwrap()
        .1 >= 5
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

    rg_db::ops::user_ops::reset_login_failures_if_open(&db, user_id)
        .await
        .unwrap()
        .expect("locked user remains open");
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
    rg_db::ops::user_ops::enable_mfa(&db, user_id)
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
                    .starts_with("plombir_git_mfa_challenge=")
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
    assert_eq!(stargazers[0]["username"], "staruser");
    assert!(
        stargazers[0]["starred_at"].as_str().is_some(),
        "the web list needs the time attached to the relationship: {body}"
    );
    assert!(
        stargazers[0].get("password_hash").is_none(),
        "a public stargazer row must not serialize the joined users model: {body}"
    );
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

// ── Passkeys (WebAuthn) ──────────────────────────────────────────

#[tokio::test]
async fn test_passkey_endpoints_require_auth() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let list = client
        .get(format!("{}/api/v1/users/passkeys", base))
        .send()
        .await
        .unwrap();
    assert_eq!(list.status(), 401);

    let start = client
        .post(format!("{}/api/v1/users/passkeys/register/start", base))
        .send()
        .await
        .unwrap();
    assert_eq!(start.status(), 401);
}

#[tokio::test]
async fn test_passkey_list_starts_empty() {
    let base = spawn_test_app().await;
    let (token, _uid) = register_full(&base, "pk_empty", "pk_empty@example.com").await;
    let client = reqwest::Client::new();

    let resp = client
        .get(format!("{}/api/v1/users/passkeys", base))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body.as_array().unwrap().len(), 0);
}

#[tokio::test]
async fn test_passkey_register_start_issues_challenge_and_cookie() {
    let base = spawn_test_app().await;
    let (token, _uid) = register_full(&base, "pk_reg", "pk_reg@example.com").await;
    let client = reqwest::Client::new();

    // WebAuthn RP ids must be domains; the test host is a bare IP, so send a
    // domain Host header (as a reverse proxy would).
    let resp = client
        .post(format!("{}/api/v1/users/passkeys/register/start", base))
        .bearer_auth(&token)
        .header(reqwest::header::HOST, "localhost")
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);

    // A sealed ceremony-state cookie must be set, HttpOnly.
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("plombir_git_passkey_reg="));
    assert!(cookie.contains("HttpOnly"));

    let body: serde_json::Value = resp.json().await.unwrap();
    let challenge = body["publicKey"]["challenge"].as_str().unwrap();
    assert!(!challenge.is_empty());
    // Fresh user: nothing to exclude.
    let exclude = body["publicKey"]["excludeCredentials"]
        .as_array()
        .map(|a| a.len())
        .unwrap_or(0);
    assert_eq!(exclude, 0);
}

#[tokio::test]
async fn test_passkey_register_finish_requires_challenge_cookie() {
    let base = spawn_test_app().await;
    let (token, _uid) = register_full(&base, "pk_finish", "pk_finish@example.com").await;
    let client = reqwest::Client::new();

    // A well-formed (but bogus) credential with no ceremony cookie must be
    // rejected as a missing/expired challenge, not accepted.
    let resp = client
        .post(format!("{}/api/v1/users/passkeys/register/finish", base))
        .bearer_auth(&token)
        .json(&serde_json::json!({
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
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);
}

#[tokio::test]
async fn test_passkey_login_start_without_passkeys_is_rejected() {
    let base = spawn_test_app().await;
    register_full(&base, "pk_login", "pk_login@example.com").await;
    let client = reqwest::Client::new();

    // Registered user but no passkeys → uniform rejection.
    let resp = client
        .post(format!("{}/api/v1/users/passkeys/login/start", base))
        .json(&serde_json::json!({"username": "pk_login"}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 400);

    // Unknown user → same rejection (no enumeration signal in the status).
    let ghost = client
        .post(format!("{}/api/v1/users/passkeys/login/start", base))
        .json(&serde_json::json!({"username": "does_not_exist"}))
        .send()
        .await
        .unwrap();
    assert_eq!(ghost.status(), 400);
}

#[tokio::test]
async fn test_passkey_login_finish_requires_challenge_cookie() {
    let base = spawn_test_app().await;
    let client = reqwest::Client::new();

    let resp = client
        .post(format!("{}/api/v1/users/passkeys/login/finish", base))
        .json(&serde_json::json!({
            "id": "AAAA",
            "rawId": "AAAA",
            "type": "public-key",
            "response": {
                "authenticatorData": "AAAA",
                "clientDataJSON": "AAAA",
                "signature": "AAAA"
            }
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
}
