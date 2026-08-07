//! card_7c7ada2d6a72: an intercepted passkey ceremony is answered once.
//!
//! `login_finish` reconstructed the ceremony from a signed cookie and nothing
//! else. Signature and `exp` say "we issued this, and it is still young" —
//! never "it has already been answered" — so one intercepted request, the
//! cookie plus the assertion body, was answered a session as many times as it
//! was presented, for the whole 300-second life of the cookie. The
//! signature-counter compare-and-swap does not cover it either: a platform
//! passkey reporting `signCount = 0` re-stores a byte-identical credential,
//! which is an honest `Stored`.
//!
//! # Why the halves, and what they cost
//!
//! The obvious test — send one `finish` request twice — cannot reach the code
//! in question. A *verified* assertion needs a real authenticator
//! (`webauthn-authenticator-rs`, not a dependency of this workspace), and
//! without one the second request dies at the same place the first one did,
//! several hundred lines before the spend. A green assertion there would be
//! green for a reason that has nothing to do with single use. So the property
//! is held in three parts, and each covers what the others cannot:
//!
//! * The **database seam** is driven for real, against every backend, in
//!   `rg-db`'s `webauthn_ceremony_single_use` — eight racing spends, an
//!   out-of-order replay, and the retention window.
//! * The **wire** is driven for real here: a ceremony really does leave the
//!   server carrying a per-ceremony nonce, and two ceremonies never carry the
//!   same one. Without that, the spend above would be arbitrating a constant.
//! * The **handlers' use of it** is read out of their own source, because the
//!   defect is a missing call: the swallowing form compiles, answers `200`, and
//!   differs from the correct one only in where the call sits.

use crate::common::{register_full, source_scan, spawn_test_app};
use base64::Engine;

/// The sealed ceremony cookie a `start` call sets, as the browser would store
/// it.
async fn ceremony_cookie(base: &str, token: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/passkeys/register/start"))
        .bearer_auth(token)
        // WebAuthn RP ids must be domains; the test host is a bare IP, so send
        // a domain `Host` header, as a reverse proxy would.
        .header(reqwest::header::HOST, "localhost")
        .send()
        .await
        .expect("begin a passkey registration ceremony");
    assert_eq!(response.status(), 200, "the ceremony did not start");
    response
        .headers()
        .get(reqwest::header::SET_COOKIE)
        .expect("a ceremony must set its state cookie")
        .to_str()
        .expect("the cookie header is text")
        .split(';')
        .next()
        .expect("a cookie has a value")
        .trim_start_matches("forgekeep_passkey_reg=")
        .to_string()
}

/// The ceremony id sealed into a state cookie.
///
/// Read out of the JWT payload without verifying it: the test is asking what
/// the server put on the wire, and the signature is a separate property with
/// its own tests (`rg_core::auth::webauthn`).
fn sealed_ceremony_id(cookie: &str) -> String {
    let payload = cookie
        .split('.')
        .nth(1)
        .expect("a sealed ceremony state is a JWT");
    let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(payload)
        .expect("the JWT payload is url-safe base64");
    let claims: serde_json::Value =
        serde_json::from_slice(&decoded).expect("the JWT payload is JSON");
    claims["data"]["ceremony_id"]
        .as_str()
        .expect("the sealed ceremony state must carry a ceremony id")
        .to_string()
}

/// What the spend arbitrates has to actually vary per ceremony. A constant — or
/// an absent field defaulted to one — would make the ledger refuse the second
/// honest login on the instance instead of the first replay.
#[tokio::test]
async fn every_ceremony_leaves_the_server_carrying_its_own_nonce() {
    let base = spawn_test_app().await;
    let (token, _uid) = register_full(&base, "pk_nonce", "pk_nonce@example.com").await;

    let first = sealed_ceremony_id(&ceremony_cookie(&base, &token).await);
    let second = sealed_ceremony_id(&ceremony_cookie(&base, &token).await);

    assert!(
        !first.is_empty(),
        "a ceremony left the server with an empty nonce: there is nothing to spend"
    );
    assert_ne!(
        first, second,
        "two ceremonies carried the same nonce; the second honest login would be refused as a replay"
    );
    assert!(
        first.len() >= 40,
        "the ceremony nonce is short enough to be guessed: {first}"
    );
}

/// `login_finish` must spend the challenge after the assertion verifies and
/// before anything is issued — and refuse a spent one exactly as it refuses a
/// bad signature.
#[test]
fn login_finish_spends_its_challenge_before_it_issues_anything() {
    let body = handler_body("login_finish");

    let verify_at = body
        .find("finish_authentication(")
        .expect("login_finish must still verify the assertion");
    let spend_at = body.find("spend_ceremony(").expect(
        "login_finish must spend the ceremony's challenge — without it, one \
                 intercepted request is a session for every 300 seconds it is replayed in",
    );
    let counter_at = body
        .find("touch_and_update(")
        .expect("login_finish must still store the advanced signature counter");
    let token_at = body
        .find("generate_token(")
        .expect("login_finish must still mint a session token");

    assert!(
        verify_at < spend_at,
        "the challenge must be spent only after the assertion verifies, or a request \
         proving nothing could burn a ceremony that is still in flight"
    );
    assert!(
        spend_at < counter_at && spend_at < token_at,
        "the challenge must be spent before the counter write and before the token is \
         minted: a replay that gets past the spend has already been answered once"
    );

    // The refusal has to be the one a bad signature gets. A distinct status
    // would tell whoever is replaying an intercepted request that the cookie
    // and assertion they hold were genuine.
    let refusal = &body[spend_at..counter_at];
    assert!(
        refusal.contains("AppError::unauthorized(")
            && refusal.contains(r#""passkey authentication failed""#),
        "a spent challenge must be refused as `401 passkey authentication failed`, the same \
         answer a bad signature gets"
    );
}

/// The same window exists on the registration ceremony, and it is closed the
/// same way.
#[test]
fn register_finish_spends_its_challenge_before_it_stores_a_credential() {
    let body = handler_body("register_finish");

    let verify_at = body
        .find("finish_registration(")
        .expect("register_finish must still verify the attestation");
    let spend_at = body.find("spend_ceremony(").expect(
        "register_finish must spend the ceremony's challenge: its cookie is \
                 replayable for the same 300 seconds the login one is",
    );
    let store_at = body
        .find("passkey_credential_ops::create(")
        .expect("register_finish must still store the new credential");

    assert!(
        verify_at < spend_at && spend_at < store_at,
        "the attestation must verify, then the challenge must be spent, and only then may a \
         credential be stored"
    );
    let refusal = &body[spend_at..store_at];
    assert!(
        refusal.contains("AppError::bad_request(")
            && refusal.contains(r#""passkey registration could not be verified""#),
        "a spent registration challenge must be refused exactly as an unverifiable \
         attestation is"
    );
}

/// Both ceremonies must mint a fresh nonce when they *start*, or the finish
/// side has nothing distinguishable to spend.
#[test]
fn both_ceremonies_mint_a_fresh_nonce_when_they_start() {
    for handler in ["register_start", "login_start"] {
        assert!(
            handler_body(handler).contains("new_ceremony_id()"),
            "{handler} must seal a fresh ceremony id into its state cookie"
        );
    }
}

fn handler_body(name: &str) -> String {
    let source = std::fs::read_to_string(source_scan::src_root().join("api/passkeys.rs"))
        .expect("read the passkey handlers");
    source_scan::functions(&source)
        .into_iter()
        .find(|f| f.name == name)
        .unwrap_or_else(|| panic!("{name} is declared in api/passkeys.rs"))
        .body
}
