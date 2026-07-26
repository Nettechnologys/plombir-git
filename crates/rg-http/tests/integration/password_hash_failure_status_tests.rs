//! Regression coverage for card_7889d9a648f0: a password check that could not
//! be *performed* must not be reported as a password that was wrong.
//!
//! `verify_password` collapsed every outcome into a bool — a PHC string it
//! could not parse, or a hash written by an algorithm this build cannot verify
//! (a half-finished rehash, a restore from a forge that used bcrypt), read
//! exactly like a mistyped password. So the registry handed the caller an
//! anonymous token, the API answered `401 invalid credentials`, and the account
//! holder had a login that failed forever with nothing in the log to explain it.
//!
//! The hash is broken here with a plain `UPDATE` rather than by closing the
//! pool: an outage fails the *lookup* first, so the request would never reach
//! the verification these tests are about, and the assertions would pass
//! against the unfixed code.

use crate::common::{register_user, spawn_test_app_with_db};
use base64::Engine as _;
use sea_orm::ConnectionTrait;

/// Not PHC at all — the shape a truncated column or an aborted migration leaves.
const BROKEN_HASH: &str = "not-a-phc-string";

const PASSWORD: &str = "Qz7$wRtm";

fn basic_auth(user: &str, pass: &str) -> String {
    let encoded =
        base64::engine::general_purpose::STANDARD.encode(format!("{user}:{pass}").as_bytes());
    format!("Basic {encoded}")
}

async fn break_stored_hash(db: &rg_db::DatabaseConnection, username: &str) {
    db.execute_unprepared(&format!(
        "UPDATE users SET password_hash = '{BROKEN_HASH}' WHERE username = '{username}'"
    ))
    .await
    .expect("corrupt the stored hash");
}

/// `GET /v2/auth/token` — the endpoint named on the card.
///
/// A token minted for "anonymous" because the *verification* failed is the
/// worst possible answer: docker takes the token, gets a 401 off the first
/// pull, and comes straight back here to fetch another one.
#[tokio::test]
async fn registry_token_for_a_broken_hash_is_not_an_anonymous_token() {
    let (base, db) = spawn_test_app_with_db().await;
    register_user(&base, "ocihash", "ocihash@example.com", PASSWORD).await;
    let client = reqwest::Client::new();
    let url = format!(
        "{base}/v2/auth/token?service=forgekeep-registry&scope=repository:ocihash/app:pull,push"
    );

    // Baseline on a healthy hash. Without it the assertion below cannot tell
    // "the failure is now reported" from "this endpoint 500s on everything".
    let resp = client
        .get(&url)
        .header("Authorization", basic_auth("ocihash", PASSWORD))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "a healthy login must still mint a token"
    );

    // The other half of the split: a genuinely wrong password is the client's
    // problem and must stay a quiet anonymous token, not become an error.
    let resp = client
        .get(&url)
        .header("Authorization", basic_auth("ocihash", "not-the-password"))
        .send()
        .await
        .expect("request");
    assert_eq!(
        resp.status(),
        200,
        "a wrong password is a rejection, not a failure"
    );

    break_stored_hash(&db, "ocihash").await;

    let resp = client
        .get(&url)
        .header("Authorization", basic_auth("ocihash", PASSWORD))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "a hash the verifier cannot use must be reported, not answered with an \
         anonymous token — got {status}: {body}"
    );
    // docker/podman parse the OCI envelope, not the AppError JSON shape.
    assert!(
        body.get("errors").and_then(|e| e.as_array()).is_some(),
        "the OCI error-envelope must survive the new branch, got: {body}"
    );
    let message = body["errors"][0]["message"]
        .as_str()
        .expect("envelope carries a message");
    assert!(
        !message.contains("password hash"),
        "the cause belongs in the operator log, not in the client's envelope: {message}"
    );
}

/// `POST /api/v1/users/login` — the same verification, one transport over.
///
/// A 401 here also feeds the brute-force counter, so our broken column would
/// have locked the account out on top of rejecting it.
#[tokio::test]
async fn web_login_with_a_broken_hash_is_not_reported_as_bad_credentials() {
    let (base, db) = spawn_test_app_with_db().await;
    register_user(&base, "loginhash", "loginhash@example.com", PASSWORD).await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/users/login");

    // Baseline: a wrong password on a healthy hash is a 401 and stays one.
    let resp = client
        .post(&url)
        .json(&serde_json::json!({"login": "loginhash", "password": "not-the-password"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 401, "a wrong password is still a 401");

    break_stored_hash(&db, "loginhash").await;

    let resp = client
        .post(&url)
        .json(&serde_json::json!({"login": "loginhash", "password": PASSWORD}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "an unusable stored hash is our failure, not invalid credentials — got {status}: {body}"
    );
    assert_eq!(
        body["error"]["message"], "Internal server error",
        "the client gets the sanitized message (H-05), got: {body}"
    );
}

/// `POST /api/v1/users/mfa/disable` — peripheral of the same class, found while
/// fixing the card: it discarded the verification error entirely
/// (`map_err(|_| unauthorized(...))`) and told an already-authenticated caller
/// their own password was wrong.
#[tokio::test]
async fn mfa_disable_with_a_broken_hash_is_not_reported_as_a_wrong_password() {
    let (base, db) = spawn_test_app_with_db().await;
    let token = register_user(&base, "mfahash", "mfahash@example.com", PASSWORD).await;
    let client = reqwest::Client::new();
    let url = format!("{base}/api/v1/users/mfa/disable");

    // Baseline: a wrong password is a 401 here, and must stay one.
    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"password": "not-the-password"}))
        .send()
        .await
        .expect("request");
    assert_eq!(resp.status(), 401, "a wrong password is still a 401");

    break_stored_hash(&db, "mfahash").await;

    let resp = client
        .post(&url)
        .bearer_auth(&token)
        .json(&serde_json::json!({"password": PASSWORD}))
        .send()
        .await
        .expect("request");
    let status = resp.status();
    let body: serde_json::Value = resp.json().await.expect("json body");
    assert!(
        status.is_server_error(),
        "an unusable stored hash must not be answered with 'invalid password' — \
         got {status}: {body}"
    );
}
