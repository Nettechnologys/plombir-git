//! card_9585caf5692d: an intercepted TOTP code passes the second factor once.
//!
//! `POST /users/mfa/verify` used to answer `rg_core::auth::totp::verify_code`,
//! which is a pure function of the shared secret and the clock. Nothing was
//! written, so nothing distinguished a first use from a replay: with `skew = 1`
//! over a 30-second step one code was accepted for the previous, current and
//! next step — around 90 seconds during which whoever intercepted it (a phishing
//! proxy, a log line, a glance at the screen) could mint as many sessions as
//! they could send requests for. RFC 6238 §5.2 requires the second attempt to be
//! refused.
//!
//! `crates/rg-db/tests/totp_step_single_use.rs` proves the statement that spends
//! the step, on all three backends and with the step passed in explicitly. This
//! file proves the part only the endpoint can answer: the replay comes back as
//! `401` with **no token**, it is indistinguishable from a wrong code, and
//! tolerating a drifting authenticator did not become the price of single use.

use crate::common::{register_full, spawn_test_app_with_db};

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

/// The step a code generated right now belongs to.
fn current_step() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("a clock after the epoch")
        .as_secs()
        / 30
}

/// Enrol TOTP over the real endpoints and hand back the stored secret.
async fn enrol(base: &str, token: &str) -> String {
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
            "password": "Qz7$wRtm",
        }))
        .send()
        .await
        .expect("enable request");
    assert_eq!(enabled.status(), 200, "enrolment must succeed");
    secret
}

/// Pass the primary factor and return the challenge cookie it issues.
///
/// The challenge lives five minutes, so one is enough for every attempt below —
/// which also keeps the attempts comparable: each one differs only in the code
/// it carries.
async fn primary_factor(base: &str, username: &str) -> String {
    let login = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({ "login": username, "password": "Qz7$wRtm" }))
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

/// One `POST /users/mfa/verify`, as `(status, token)`.
async fn verify(base: &str, challenge: &str, username: &str, code: &str) -> (u16, String) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/verify"))
        .header(reqwest::header::COOKIE, challenge)
        .json(&serde_json::json!({ "username": username, "code": code }))
        .send()
        .await
        .expect("verify request");
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().await.expect("verify response body");
    let token = body["token"].as_str().unwrap_or_default().to_string();
    (status, token)
}

#[tokio::test]
async fn one_totp_code_is_answered_a_session_once() {
    let (base, _db) = spawn_test_app_with_db().await;
    let username = "totp_replay";
    let (token, _user_id) = register_full(&base, username, "totp_replay@example.com").await;
    let secret = enrol(&base, &token).await;
    let challenge = primary_factor(&base, username).await;

    // The step is captured once and every code below is derived from it, so the
    // three attempts are about the same code and not about three instants.
    let step = current_step();
    let code = code_for_step(&secret, step);

    let (status, session) = verify(&base, &challenge, username, &code).await;
    assert_eq!(status, 200, "the first use of a live code must succeed");
    assert!(
        !session.is_empty(),
        "a successful verify must issue a token"
    );

    let (replay_status, replay_session) = verify(&base, &challenge, username, &code).await;
    assert_eq!(
        replay_status, 401,
        "the same code passed the second factor a second time"
    );
    assert!(
        replay_session.is_empty(),
        "a refused verify handed out a session anyway: {replay_session}"
    );

    // Indistinguishable from a wrong code — otherwise the reply confirms to
    // whoever replayed it that the code was genuine and merely late.
    let (wrong_status, _) = verify(&base, &challenge, username, "000000").await;
    assert_eq!(
        wrong_status, replay_status,
        "a replay must not be answerable apart from a wrong code"
    );

    // A code from the *next* step is still inside the skew window and nobody has
    // spent it, so an authenticator running slightly fast must still get in:
    // single use is not allowed to cost clock tolerance.
    let (skew_status, skew_session) = verify(
        &base,
        &challenge,
        username,
        &code_for_step(&secret, step + 1),
    )
    .await;
    assert_eq!(
        skew_status, 200,
        "a newer, unspent step must still be accepted"
    );
    assert!(!skew_session.is_empty());

    // And the step just below the spent one — the code a replay of a slightly
    // stale interception carries — is refused rather than waved through as "a
    // different step".
    let (stale_status, stale_session) = verify(
        &base,
        &challenge,
        username,
        &code_for_step(&secret, step - 1),
    )
    .await;
    assert_eq!(
        stale_status, 401,
        "a step older than the one already spent must be refused"
    );
    assert!(stale_session.is_empty());
}
