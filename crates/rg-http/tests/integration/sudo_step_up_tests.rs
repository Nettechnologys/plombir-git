//! Sudo mode: minting a credential that outlives the session needs the
//! password proved again (security audit finding #7).
//!
//! A bearer session lasts seven days. Before this, the routes that mint SSH
//! keys, personal access tokens, passkeys and SSO links asked nothing more of
//! it than that it was valid — so one stolen session was permanent access the
//! moment it added a key. `POST /users/me/sudo` re-proves the password (and
//! the second factor, when enrolled) and re-issues the same session with a
//! ten-minute window in it; the credential routes take `SudoUser` and refuse
//! a session outside that window with `reason: sudo_required`.

use sea_orm::{ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder};

use crate::common::{
    register_full, register_user, register_user_plain, spawn_test_app, spawn_test_app_with_db,
    spawn_test_app_with_state, sudo_session,
};

const PASSWORD: &str = "Qz7$wRtm";

const TEST_SSH_KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAIMLnUZTOEZ6vRQedOGoTgsxZ5HGkmNVaERGBsVPZkTDe sudo-step-up";

/// Every route behind `SudoUser`, with the body and headers that reach it.
///
/// The SSO link names a provider that does not exist: the extractor runs
/// before the handler, so a session without sudo is refused before the
/// provider is looked up, and one with it is answered by the lookup.
fn protected_requests(base: &str) -> Vec<(&'static str, reqwest::RequestBuilder)> {
    let client = reqwest::Client::new();
    vec![
        (
            "POST /users/tokens",
            client
                .post(format!("{base}/api/v1/users/tokens"))
                .json(&serde_json::json!({ "name": "sudo-cli" })),
        ),
        (
            "POST /users/ssh-keys",
            client
                .post(format!("{base}/api/v1/users/ssh-keys"))
                .json(&serde_json::json!({ "title": "laptop", "public_key": TEST_SSH_KEY })),
        ),
        (
            "POST /users/passkeys/register/start",
            client
                .post(format!("{base}/api/v1/users/passkeys/register/start"))
                .header(reqwest::header::HOST, "localhost"),
        ),
        (
            "POST /auth/sso/{slug}/link",
            client.post(format!("{base}/api/v1/auth/sso/no-such-provider/link")),
        ),
    ]
}

async fn sudo_attempt(
    base: &str,
    token: &str,
    body: serde_json::Value,
) -> (reqwest::StatusCode, serde_json::Value) {
    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/me/sudo"))
        .bearer_auth(token)
        .json(&body)
        .send()
        .await
        .expect("sudo request");
    let status = resp.status();
    let body = resp.json().await.unwrap_or(serde_json::Value::Null);
    (status, body)
}

fn assert_sudo_required(label: &str, status: reqwest::StatusCode, body: &serde_json::Value) {
    assert_eq!(status, 403, "{label}: {body}");
    assert_eq!(
        body["error"]["reason"], "sudo_required",
        "{label} must name the step the client can take: {body}"
    );
    assert_eq!(body["error"]["code"], "FORBIDDEN", "{label}: {body}");
}

async fn user(db: &rg_db::DatabaseConnection, user_id: i64) -> rg_db::entities::user::Model {
    rg_db::ops::user_ops::find_by_id(db, user_id)
        .await
        .expect("load user")
        .expect("user exists")
}

#[tokio::test]
async fn credential_routes_refuse_a_plain_session_and_admit_a_stepped_up_one() {
    let (base, db) = spawn_test_app_with_db().await;
    let plain = register_user_plain(&base, "sudoplain", "sudoplain@example.com", PASSWORD).await;
    let user_id = rg_db::ops::user_ops::find_by_username(&db, "sudoplain")
        .await
        .unwrap()
        .unwrap()
        .id;

    for (label, request) in protected_requests(&base) {
        let resp = request.bearer_auth(&plain).send().await.expect(label);
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        assert_sudo_required(label, status, &body);
    }
    // The same answer through the cookie, which is how the browser presents
    // the session.
    let via_cookie = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .header("Cookie", format!("plombir_git_token={plain}"))
        .json(&serde_json::json!({ "name": "sudo-cli" }))
        .send()
        .await
        .unwrap();
    let status = via_cookie.status();
    let body: serde_json::Value = via_cookie.json().await.unwrap_or(serde_json::Value::Null);
    assert_sudo_required("POST /users/tokens (cookie)", status, &body);

    assert_eq!(
        rg_db::entities::access_token::Entity::find()
            .filter(rg_db::entities::access_token::Column::UserId.eq(user_id))
            .count(&db)
            .await
            .unwrap(),
        0,
        "a refused request must not mint a PAT"
    );
    assert_eq!(
        rg_db::entities::ssh_key::Entity::find()
            .filter(rg_db::entities::ssh_key::Column::UserId.eq(user_id))
            .count(&db)
            .await
            .unwrap(),
        0,
        "a refused request must not store an SSH key"
    );

    // A plain session is still a session everywhere else.
    let me = reqwest::Client::new()
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(&plain)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200);

    let elevated = sudo_session(&base, &plain, PASSWORD, None).await;
    let expected = [201, 201, 200, 404];
    for ((label, request), want) in protected_requests(&base).into_iter().zip(expected) {
        let resp = request.bearer_auth(&elevated).send().await.expect(label);
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        assert_eq!(status, want, "{label} after stepping up: {body}");
        assert!(
            !body.contains("sudo_required"),
            "{label} still asks for sudo after stepping up: {body}"
        );
    }
}

#[tokio::test]
async fn the_step_up_answers_like_the_login_and_keeps_the_session() {
    let (base, db, state) = spawn_test_app_with_state().await;
    let plain = register_user_plain(&base, "sudokeep", "sudokeep@example.com", PASSWORD).await;
    let before = rg_core::auth::jwt::validate_token(&plain, &state.jwt_secret).unwrap();
    assert_eq!(before.sudo_exp, None, "a login session is not in sudo mode");

    let resp = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/me/sudo"))
        .bearer_auth(&plain)
        .json(&serde_json::json!({ "password": PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 200);
    let cookie = resp
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .expect("the step-up sets the session cookie like the login does")
        .to_str()
        .unwrap()
        .to_string();
    assert!(cookie.starts_with("plombir_git_token="), "{cookie}");
    assert!(cookie.contains("HttpOnly"), "{cookie}");
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body["username"], "sudokeep");
    assert_eq!(body["mfa_required"], false);
    let token = body["token"].as_str().expect("the token is in the body");
    assert!(
        cookie.starts_with(&format!("plombir_git_token={token};")),
        "the cookie and the body carry the same session"
    );

    let after = rg_core::auth::jwt::validate_token(token, &state.jwt_secret).unwrap();
    assert_eq!(after.sub, before.sub);
    assert_eq!(after.session_version, before.session_version);
    assert_eq!(after.exp, before.exp, "stepping up is not a new login");
    let now = chrono::Utc::now().timestamp();
    let until = after.sudo_exp.expect("the re-issued session is in sudo mode");
    assert!(until > now, "{until} <= {now}");
    assert!(
        until <= now + rg_core::auth::jwt::SUDO_TTL.num_seconds(),
        "the window is bounded by SUDO_TTL"
    );

    let journal = rg_db::entities::audit_log::Entity::find()
        .filter(rg_db::entities::audit_log::Column::Action.eq("user.sudo"))
        .order_by_asc(rg_db::entities::audit_log::Column::Id)
        .all(&db)
        .await
        .unwrap();
    assert_eq!(journal.len(), 1, "the step-up is journalled once");
    let details: serde_json::Value =
        serde_json::from_str(journal[0].details.as_deref().unwrap_or("{}")).unwrap();
    assert_eq!(details["method"], "password");
    assert_eq!(details["second_factor"], serde_json::Value::Null);

    // The window travels with the session: a logout revokes the elevated
    // token exactly as it revokes the plain one.
    let logout = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/logout"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap();
    assert_eq!(logout.status(), 200);
    let after_logout = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": "after-logout" }))
        .send()
        .await
        .unwrap();
    assert_eq!(after_logout.status(), 401);
}

#[tokio::test]
async fn an_expired_sudo_window_is_an_ordinary_session_again() {
    let (base, _db, state) = spawn_test_app_with_state().await;
    let plain = register_user_plain(&base, "sudostale", "sudostale@example.com", PASSWORD).await;
    let claims = rg_core::auth::jwt::validate_token(&plain, &state.jwt_secret).unwrap();
    let now = chrono::Utc::now().timestamp();
    let stale = rg_core::auth::jwt::encode_claims_as_is(
        &rg_core::auth::jwt::Claims {
            sudo_exp: Some(now - 1),
            ..claims
        },
        &state.jwt_secret,
    )
    .unwrap();

    let me = reqwest::Client::new()
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(&stale)
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200, "the session itself is still good");

    for (label, request) in protected_requests(&base) {
        let resp = request.bearer_auth(&stale).send().await.expect(label);
        let status = resp.status();
        let body: serde_json::Value = resp.json().await.unwrap_or(serde_json::Value::Null);
        assert_sudo_required(&format!("{label} with an expired window"), status, &body);
    }
}

#[tokio::test]
async fn a_wrong_password_is_refused_and_strikes_the_shared_lockout() {
    let (base, db) = spawn_test_app_with_db().await;
    let plain = register_user_plain(&base, "sudoguess", "sudoguess@example.com", PASSWORD).await;
    let user_id = rg_db::ops::user_ops::find_by_username(&db, "sudoguess")
        .await
        .unwrap()
        .unwrap()
        .id;

    // A malformed request is not a guess.
    let (status, _) = sudo_attempt(&base, &plain, serde_json::json!({})).await;
    assert_eq!(status, 422, "a body without a password is malformed, not wrong");
    assert_eq!(user(&db, user_id).await.login_attempts, 0);

    for attempt in 1..=rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS {
        let (status, body) =
            sudo_attempt(&base, &plain, serde_json::json!({ "password": "not-it" })).await;
        assert_eq!(status, 401, "attempt {attempt}: {body}");
        assert_eq!(
            body["error"]["reason"],
            serde_json::Value::Null,
            "a wrong password is not an invitation to step up"
        );
    }
    let locked = user(&db, user_id).await;
    assert!(
        locked
            .locked_until
            .is_some_and(|until| until > chrono::Utc::now()),
        "the shared lockout engaged after {} wrong guesses",
        rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS
    );

    // The lock holds against the right password, here and at the login.
    let (status, body) = sudo_attempt(&base, &plain, serde_json::json!({ "password": PASSWORD })).await;
    assert_eq!(status, 401, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("locked"),
        "{body}"
    );
    let login = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({ "login": "sudoguess", "password": PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(login.status(), 401, "the lock is the account's, not the door's");

    // The attempts are filed under the door they arrived at.
    let filed = rg_db::ops::login_log_ops::Entity::find()
        .filter(rg_db::entities::login_log::Column::Username.eq("sudoguess"))
        .filter(rg_db::entities::login_log::Column::AuthProvider.eq("sudo"))
        .filter(rg_db::entities::login_log::Column::Success.eq(false))
        .count(&db)
        .await
        .unwrap();
    assert!(
        filed >= rg_core::auth::lockout::MAX_FAILED_PASSWORD_ATTEMPTS as u64,
        "{filed} sudo attempts filed"
    );
}

#[tokio::test]
async fn a_personal_access_token_cannot_step_up() {
    let base = spawn_test_app().await;
    let jwt = register_user(&base, "sudopat", "sudopat@example.com", PASSWORD).await;
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&jwt)
        .json(&serde_json::json!({ "name": "cli", "scopes": "user,repo,admin" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);
    let pat = created.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, body) = sudo_attempt(&base, &pat, serde_json::json!({ "password": PASSWORD })).await;
    assert_eq!(status, 403, "{body}");
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("login session"),
        "{body}"
    );
    assert_eq!(body["error"]["reason"], serde_json::Value::Null);

    // A PAT beside a session cookie still owns the request.
    let mixed = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/me/sudo"))
        .header("Cookie", format!("plombir_git_token={jwt}"))
        .bearer_auth(&pat)
        .json(&serde_json::json!({ "password": PASSWORD }))
        .send()
        .await
        .unwrap();
    assert_eq!(mixed.status(), 403);

    // And the PAT is refused at the credential routes without being told to
    // step up — there is no step it could take.
    let denied = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/ssh-keys"))
        .bearer_auth(&pat)
        .json(&serde_json::json!({ "title": "laptop", "public_key": TEST_SSH_KEY }))
        .send()
        .await
        .unwrap();
    assert_eq!(denied.status(), 403);
    let body: serde_json::Value = denied.json().await.unwrap();
    assert_eq!(body["error"]["reason"], serde_json::Value::Null, "{body}");
}

// ── MFA ────────────────────────────────────────────────────────────────────

/// The authenticator's side of the handshake, for one explicit time step.
fn code_for_step(secret: &str, step: u64) -> String {
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
    .generate(step * 30)
}

fn current_step() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("a clock after the epoch")
        .as_secs()
        / 30
}

/// Enrol TOTP over the real endpoints; hand back the secret and the backup
/// codes. Enrolment is an ordinary session route — it does not need sudo.
async fn enrol(base: &str, token: &str) -> (String, Vec<String>) {
    let client = reqwest::Client::new();
    let setup: serde_json::Value = client
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(token)
        .send()
        .await
        .expect("setup request")
        .json()
        .await
        .expect("setup response body");
    let secret = setup["secret"]
        .as_str()
        .expect("setup returned no secret")
        .to_string();
    let enabled = client
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(token)
        .json(&serde_json::json!({ "code": code_for_step(&secret, current_step()) }))
        .send()
        .await
        .expect("enable request");
    assert_eq!(enabled.status(), 200, "enrolment must succeed");
    let body: serde_json::Value = enabled.json().await.expect("enable response body");
    let codes = body["backup_codes"]
        .as_array()
        .expect("enrolment returned no backup codes")
        .iter()
        .map(|c| c.as_str().expect("a backup code is not text").to_string())
        .collect();
    (secret, codes)
}

#[tokio::test]
async fn an_mfa_enrolled_account_needs_the_second_factor_to_step_up() {
    let (base, db) = spawn_test_app_with_db().await;
    let plain = register_user_plain(&base, "sudomfa", "sudomfa@example.com", PASSWORD).await;
    let user_id = rg_db::ops::user_ops::find_by_username(&db, "sudomfa")
        .await
        .unwrap()
        .unwrap()
        .id;
    let (secret, backup_codes) = enrol(&base, &plain).await;

    // The password alone is not enough, and saying so is not a strike.
    let (status, body) = sudo_attempt(&base, &plain, serde_json::json!({ "password": PASSWORD })).await;
    assert_eq!(status, 400, "{body}");
    assert_eq!(user(&db, user_id).await.login_attempts, 0);

    // A wrong code is a strike, like the login's second step.
    let (status, body) = sudo_attempt(
        &base,
        &plain,
        serde_json::json!({ "password": PASSWORD, "totp_code": "000000" }),
    )
    .await;
    assert_eq!(status, 401, "{body}");
    assert_eq!(user(&db, user_id).await.login_attempts, 1);

    // A wrong password with the right code is the password's refusal.
    let (status, body) = sudo_attempt(
        &base,
        &plain,
        serde_json::json!({ "password": "not-it", "totp_code": code_for_step(&secret, current_step()) }),
    )
    .await;
    assert_eq!(status, 401, "{body}");

    // Password and code: in.
    let step = current_step();
    let (status, body) = sudo_attempt(
        &base,
        &plain,
        serde_json::json!({ "password": PASSWORD, "totp_code": code_for_step(&secret, step) }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let elevated = body["token"].as_str().unwrap().to_string();
    let created = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&elevated)
        .json(&serde_json::json!({ "name": "after-mfa" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), 201);

    // The code was spent: replaying it is refused (RFC 6238 §5.2).
    let (status, body) = sudo_attempt(
        &base,
        &plain,
        serde_json::json!({ "password": PASSWORD, "totp_code": code_for_step(&secret, step) }),
    )
    .await;
    assert_eq!(status, 401, "a replayed TOTP code must not step up: {body}");

    // A backup code works once.
    let (status, body) = sudo_attempt(
        &base,
        &plain,
        serde_json::json!({ "password": PASSWORD, "backup_code": backup_codes[0] }),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = sudo_attempt(
        &base,
        &plain,
        serde_json::json!({ "password": PASSWORD, "backup_code": backup_codes[0] }),
    )
    .await;
    assert_eq!(status, 401, "a spent backup code must not step up: {body}");

    let journal = rg_db::entities::audit_log::Entity::find()
        .filter(rg_db::entities::audit_log::Column::Action.eq("user.sudo"))
        .order_by_asc(rg_db::entities::audit_log::Column::Id)
        .all(&db)
        .await
        .unwrap();
    let factors: Vec<serde_json::Value> = journal
        .iter()
        .map(|row| {
            serde_json::from_str::<serde_json::Value>(row.details.as_deref().unwrap_or("{}"))
                .unwrap()["second_factor"]
                .clone()
        })
        .collect();
    assert_eq!(factors, vec![serde_json::json!("totp"), serde_json::json!("backup_code")]);
}

#[tokio::test]
async fn the_profile_says_whether_a_second_factor_will_be_asked() {
    let base = spawn_test_app().await;
    let (token, _) = register_full(&base, "sudoprofile", "sudoprofile@example.com").await;
    async fn me(base: &str, token: &str) -> serde_json::Value {
        reqwest::Client::new()
            .get(format!("{base}/api/v1/users/me"))
            .bearer_auth(token)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
    assert_eq!(me(&base, &token).await["mfa_enabled"], false);
    enrol(&base, &token).await;
    assert_eq!(me(&base, &token).await["mfa_enabled"], true);
}
