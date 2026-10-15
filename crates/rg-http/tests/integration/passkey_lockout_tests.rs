//! card_05245ed87f97: a locked account answers a passkey login the way a bad
//! signature is answered.
//!
//! `login_finish` read `locked_until` before `finish_authentication` and said
//! `401 account is temporarily locked`. `login_start` hands out the challenge
//! cookie for a username alone, so the lock state of every account with a
//! passkey was readable without any secret. The password door already reads
//! the lock only after the hash verifies; this is the passkey half.
//!
//! Unlike the other passkey suites this one drives a *verified* assertion. A
//! software authenticator is a P-256 key: the stored credential is the
//! `Passkey` JSON `webauthn-rs` would have produced at registration, and the
//! assertion is authenticator data plus client data signed the way a browser's
//! authenticator signs them. Without a real signature the test could only show
//! that a bad assertion is refused — which says nothing about what a *good*
//! one on a locked account is told.

use base64::Engine as _;
use p256::ecdsa::signature::Signer as _;
use p256::ecdsa::{Signature, SigningKey};
use sea_orm::{ActiveModelTrait, EntityTrait, IntoActiveModel, Set};
use sha2::{Digest, Sha256};

use crate::common::{register_full, spawn_test_app_with_db};

const RP_ID: &str = "localhost";
const ORIGIN: &str = "http://localhost";
const CREDENTIAL_ID: &[u8] = b"lockout-test-credential";

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

fn authenticator() -> SigningKey {
    SigningKey::from_bytes(&[7u8; 32].into()).expect("a valid P-256 scalar")
}

/// The credential as `webauthn-rs` stores a user-verified, device-bound passkey
/// registered with `none` attestation and a zero counter.
fn stored_passkey(key: &SigningKey) -> String {
    let point = key.verifying_key().to_encoded_point(false);
    let passkey = serde_json::json!({
        "cred": {
            "cred_id": b64(CREDENTIAL_ID),
            "cred": {
                "type_": "ES256",
                "key": {"EC_EC2": {
                    "curve": "SECP256R1",
                    "x": b64(point.x().expect("uncompressed x")),
                    "y": b64(point.y().expect("uncompressed y")),
                }},
            },
            "counter": 0,
            "transports": null,
            "user_verified": true,
            "backup_eligible": false,
            "backup_state": false,
            "registration_policy": "required",
            "extensions": {},
            "attestation": {"data": "None", "metadata": "None"},
            "attestation_format": "none",
        }
    })
    .to_string();
    // Proved here rather than at the first login: a fixture the server cannot
    // parse is skipped as corrupt, and the login would fail for that reason.
    rg_core::auth::webauthn::passkey_from_json(&passkey).expect("the fixture is a Passkey");
    passkey
}

/// `POST /login/start` for `username`: the challenge and the cookie it is sealed in.
async fn start(base: &str, username: &str) -> (Vec<u8>, String) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/passkeys/login/start"))
        // WebAuthn RP ids must be domains; the test host is a bare IP.
        .header(reqwest::header::HOST, RP_ID)
        .json(&serde_json::json!({"username": username}))
        .send()
        .await
        .expect("login/start");
    assert_eq!(response.status(), 200, "the login ceremony did not start");
    let cookie = response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .expect("a ceremony cookie")
        .to_str()
        .expect("text")
        .split(';')
        .next()
        .expect("a cookie value")
        .to_string();
    let body: serde_json::Value = response.json().await.expect("json");
    let challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(
            body["publicKey"]["challenge"]
                .as_str()
                .expect("a challenge"),
        )
        .expect("the challenge is url-safe base64");
    (challenge, cookie)
}

/// An assertion over `challenge`, signed by `key` (or garbage when `genuine` is false).
fn assertion(key: &SigningKey, challenge: &[u8], counter: u32, genuine: bool) -> serde_json::Value {
    let mut authenticator_data = Sha256::digest(RP_ID.as_bytes()).to_vec();
    // User present + user verified; not backup-eligible, as stored.
    authenticator_data.push(0x01 | 0x04);
    authenticator_data.extend_from_slice(&counter.to_be_bytes());
    let client_data = serde_json::json!({
        "type": "webauthn.get",
        "challenge": b64(challenge),
        "origin": ORIGIN,
        "crossOrigin": false,
    })
    .to_string();
    let mut signed = authenticator_data.clone();
    signed.extend_from_slice(&Sha256::digest(client_data.as_bytes()));
    let signature: Signature = key.sign(&signed);
    let mut signature = signature.to_der().as_bytes().to_vec();
    if !genuine {
        let last = signature.len() - 1;
        signature[last] ^= 0xff;
    }
    serde_json::json!({
        "id": b64(CREDENTIAL_ID),
        "rawId": b64(CREDENTIAL_ID),
        "type": "public-key",
        "response": {
            "authenticatorData": b64(&authenticator_data),
            "clientDataJSON": b64(client_data.as_bytes()),
            "signature": b64(&signature),
            "userHandle": null,
        },
        "extensions": {},
    })
}

async fn finish(base: &str, cookie: &str, assertion: &serde_json::Value) -> (u16, String) {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/passkeys/login/finish"))
        .header(reqwest::header::HOST, RP_ID)
        .header(reqwest::header::COOKIE, cookie)
        .json(assertion)
        .send()
        .await
        .expect("login/finish");
    (
        response.status().as_u16(),
        response.text().await.expect("body"),
    )
}

/// The refusal as a caller could compare two of them: everything but the
/// per-request id every error envelope carries.
fn comparable(body: &str) -> serde_json::Value {
    let mut body: serde_json::Value = serde_json::from_str(body).expect("a JSON error body");
    body["error"]
        .as_object_mut()
        .expect("an error envelope")
        .remove("request_id");
    body
}

async fn set_lock(db: &rg_db::DatabaseConnection, user_id: i64, locked: bool) {
    let mut user = rg_db::entities::user::Entity::find_by_id(user_id)
        .one(db)
        .await
        .expect("query")
        .expect("the user")
        .into_active_model();
    user.locked_until = Set(locked.then(|| chrono::Utc::now() + chrono::Duration::hours(1)));
    user.update(db).await.expect("set the lock");
}

#[tokio::test]
async fn a_locked_account_answers_a_genuine_assertion_as_it_answers_a_forged_one() {
    let (base, db) = spawn_test_app_with_db().await;
    let (_, user_id) = register_full(&base, "pk-locked", "pk-locked@example.com").await;
    let key = authenticator();
    rg_db::ops::passkey_credential_ops::create(
        &db,
        user_id,
        &b64(CREDENTIAL_ID),
        &stored_passkey(&key),
        "software key",
        RP_ID,
    )
    .await
    .expect("register the passkey");
    set_lock(&db, user_id, true).await;

    let (challenge, cookie) = start(&base, "pk-locked").await;
    let forged = finish(&base, &cookie, &assertion(&key, &challenge, 1, false)).await;
    let (challenge, cookie) = start(&base, "pk-locked").await;
    let genuine = finish(&base, &cookie, &assertion(&key, &challenge, 1, true)).await;

    assert_eq!(forged.0, 401, "a forged assertion: {}", forged.1);
    assert_eq!(
        (genuine.0, comparable(&genuine.1)),
        (forged.0, comparable(&forged.1)),
        "a locked account told a genuine assertion apart from a forged one"
    );
    assert!(
        !genuine.1.contains("locked"),
        "the refusal names the lock: {}",
        genuine.1
    );

    // The same key and the same shape of assertion sign the account in once
    // the lock is gone — so the refusal above was the lock, read after the
    // signature verified, and not a fixture the server could not verify.
    set_lock(&db, user_id, false).await;
    let (challenge, cookie) = start(&base, "pk-locked").await;
    let (status, body) = finish(&base, &cookie, &assertion(&key, &challenge, 2, true)).await;
    assert_eq!(status, 200, "the unlocked login failed: {body}");
}
