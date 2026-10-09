//! card_910bae727b74: backup codes had no way to be re-issued.
//!
//! `set_codes` has always been able to replace the live set — it deletes the
//! unused rows and inserts the new ones in one transaction — but the only caller
//! was enrolment. So an owner who had spent all ten codes, or whose printout had
//! leaked, could only get new ones by disabling MFA and enrolling again: dropping
//! the second factor in order to renew the material that exists for when the
//! second factor is unavailable.
//!
//! This file proves the endpoint that closes that gap: it is a password door
//! (and therefore carries the account lockout, like `POST /users/mfa/disable`),
//! and the re-issue really rotates — the old unused code stops passing
//! `POST /users/mfa/verify` at the same moment the new one starts.

use crate::common::{register_full, spawn_test_app_with_db};

const PASSWORD: &str = "Qz7$wRtm";

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

/// Enrol TOTP over the real endpoints; hand back the backup codes it issued.
async fn enrol(base: &str, token: &str) -> Vec<String> {
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
        .json(&serde_json::json!({
            "code": code_for_step(&secret, current_step()),
            "password": PASSWORD,
        }))
        .send()
        .await
        .expect("enable request");
    assert_eq!(enabled.status(), 200, "enrolment must succeed");
    let body: serde_json::Value = enabled.json().await.expect("enable response body");
    body["backup_codes"]
        .as_array()
        .expect("enrolment returned no backup codes")
        .iter()
        .map(|c| c.as_str().expect("a backup code is not text").to_string())
        .collect()
}

/// One `POST /users/mfa/backup/regenerate`, as `(status, codes)`.
async fn regenerate(base: &str, token: Option<&str>, password: &str) -> (u16, Vec<String>) {
    let mut request = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/backup/regenerate"))
        .json(&serde_json::json!({ "password": password }));
    if let Some(token) = token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await.expect("regenerate request");
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().await.unwrap_or(serde_json::Value::Null);
    let codes = body["backup_codes"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|c| c.as_str().unwrap_or_default().to_string())
                .collect()
        })
        .unwrap_or_default();
    (status, codes)
}

/// `GET /users/mfa/backup` — the summary the settings page reads.
///
/// The route had no test of any kind: the only thing that ever named it was the
/// `POST .../backup/regenerate` below, which the coverage inventory used to read
/// as its parent (card_d482cf7e098e). It is the one endpoint that can say
/// whether a re-issue reached the live set, so the rotation test asks it.
async fn backup_status(base: &str, token: &str) -> serde_json::Value {
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/users/mfa/backup"))
        .bearer_auth(token)
        .send()
        .await
        .expect("backup status request");
    assert_eq!(
        response.status(),
        200,
        "the owner must be able to read their own backup-code status"
    );
    response.json().await.expect("backup status body")
}

/// Pass the primary factor and return the challenge cookie it issues.
async fn primary_factor(base: &str, username: &str) -> String {
    let login = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({ "login": username, "password": PASSWORD }))
        .send()
        .await
        .expect("login request");
    assert_eq!(login.status(), 200);
    let cookie = login
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .expect("login issued no challenge cookie")
        .to_str()
        .expect("challenge cookie is not text")
        .split(';')
        .next()
        .expect("empty challenge cookie")
        .to_string();
    assert!(cookie.starts_with("plombir_git_mfa_challenge="));
    cookie
}

/// Redeem a backup code through the login endpoint; returns the status.
async fn redeem(base: &str, username: &str, code: &str) -> u16 {
    let challenge = primary_factor(base, username).await;
    reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/verify"))
        .header(reqwest::header::COOKIE, challenge)
        .json(&serde_json::json!({ "username": username, "code": code, "backup": true }))
        .send()
        .await
        .expect("verify request")
        .status()
        .as_u16()
}

#[tokio::test]
async fn regenerating_backup_codes_rotates_the_live_set() {
    let (base, _db) = spawn_test_app_with_db().await;
    let username = "backup_reissue";
    let (token, _user_id) = register_full(&base, username, "backup_reissue@example.com").await;
    let old_codes = enrol(&base, &token).await;
    assert_eq!(old_codes.len(), 10, "enrolment issues ten codes");
    let issued = backup_status(&base, &token).await;
    assert_eq!(issued["total"], 10, "the status miscounts the issued set");
    assert_eq!(
        issued["unused"], 10,
        "a set nobody has spent reads as spent"
    );

    // No session at all: the re-issue is not a public door.
    let (anonymous, _) = regenerate(&base, None, PASSWORD).await;
    assert_eq!(anonymous, 401, "the re-issue must require a session");

    // A session is not enough. This is the point of the card's acceptance
    // check: a stolen cookie must not be able to mint itself a permanent
    // bypass of the second factor.
    let (no_password, codes) = regenerate(&base, Some(&token), "not-the-password").await;
    assert_eq!(no_password, 401, "a wrong password must not re-issue codes");
    assert!(
        codes.is_empty(),
        "a refused re-issue handed out codes anyway: {codes:?}"
    );

    let (status, new_codes) = regenerate(&base, Some(&token), PASSWORD).await;
    assert_eq!(status, 200, "the owner's password must re-issue the set");
    assert_eq!(new_codes.len(), 10, "a re-issue hands out a full set");
    assert!(
        new_codes.iter().all(|c| !old_codes.contains(c)),
        "the re-issued set repeats a code from the old one"
    );

    // The rotation is what makes this worth having: the old printout is dead.
    assert_eq!(
        redeem(&base, username, &old_codes[0]).await,
        401,
        "a code from the replaced set still passed the second factor"
    );
    assert_eq!(
        redeem(&base, username, &new_codes[0]).await,
        200,
        "a freshly issued code did not pass the second factor"
    );

    // The summary the settings page reads has to describe the set that is
    // actually live — and never the codes in it.
    let after = backup_status(&base, &token).await;
    assert_eq!(
        after["total"], 10,
        "the status still counts the replaced set"
    );
    assert_eq!(
        after["unused"], 9,
        "redeeming a code left the status reporting a full set"
    );
    let rendered = after.to_string();
    assert!(
        new_codes.iter().all(|code| !rendered.contains(code)),
        "the status handed the live backup codes back: {rendered}"
    );
}

#[tokio::test]
async fn regenerating_without_mfa_enabled_is_a_bad_request() {
    let (base, _db) = spawn_test_app_with_db().await;
    let (token, _user_id) = register_full(&base, "backup_nomfa", "backup_nomfa@example.com").await;

    // Codes that guard nothing are not a credential, they are a trap: the
    // account has no second factor, so a set minted here would only sit in the
    // database until MFA was turned on and silently widen it.
    let (status, codes) = regenerate(&base, Some(&token), PASSWORD).await;
    assert_eq!(status, 400, "a re-issue without MFA must be refused");
    assert!(codes.is_empty());
}
