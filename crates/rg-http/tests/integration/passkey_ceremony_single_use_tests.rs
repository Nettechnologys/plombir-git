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
        .trim_start_matches("plombir_git_passkey_reg=")
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

/// A handler body together with its byte-aligned code-only view.
///
/// Every structural position this file asserts on — a call, an ordering, the
/// span between two calls — is located in `code`, where a comment or a
/// call-shaped Rust literal contributes nothing. The original text is kept
/// beside it because the *refusal* is a string literal, and only the original
/// still carries what a literal says.
struct HandlerSource {
    body: String,
    code: String,
}

impl HandlerSource {
    fn new(body: String) -> Self {
        let code = source_scan::rust_code_only(&body);
        Self { body, code }
    }

    /// Where the live call to `call` starts, or `None` when the only mentions
    /// of it are prose or data.
    fn call_at(&self, call: &str) -> Option<usize> {
        self.code.find(call)
    }

    fn call_at_or_panic(&self, call: &str, expectation: &str) -> usize {
        self.call_at(call)
            .unwrap_or_else(|| panic!("{expectation}"))
    }

    /// Whether a live `constructor(...)` between `from` and `to` is built with
    /// `message` as its first argument.
    ///
    /// The call is found in the code view and its arguments are then sliced
    /// from the original source at that offset, so the refusal a replay gets is
    /// compared against the literal the compiler sees. Neither a comment
    /// quoting the message nor a live refusal carrying a *different* one counts.
    fn refuses_between(&self, from: usize, to: usize, constructor: &str, message: &str) -> bool {
        let needle = format!("{constructor}(");
        let literal = format!("{message:?}");
        self.code[from..to]
            .match_indices(&needle)
            .any(|(relative, _)| {
                let open = from + relative + needle.len() - 1;
                source_scan::call_args_from_code(&self.body, &self.code, open)
                    .is_some_and(|args| args.first().is_some_and(|argument| *argument == literal))
            })
    }
}

/// `login_finish` must spend the challenge after the assertion verifies and
/// before anything is issued — and refuse a spent one exactly as it refuses a
/// bad signature.
#[test]
fn login_finish_spends_its_challenge_before_it_issues_anything() {
    let handler = handler_source("login_finish");

    let verify_at = handler.call_at_or_panic(
        "finish_authentication(",
        "login_finish must still verify the assertion",
    );
    let spend_at = handler.call_at_or_panic(
        "spend_ceremony(",
        "login_finish must spend the ceremony's challenge — without it, one \
         intercepted request is a session for every 300 seconds it is replayed in",
    );
    let counter_at = handler.call_at_or_panic(
        "touch_and_update(",
        "login_finish must still store the advanced signature counter",
    );
    let token_at = handler.call_at_or_panic(
        "generate_token(",
        "login_finish must still mint a session token",
    );

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
    assert!(
        handler.refuses_between(
            spend_at,
            counter_at,
            "AppError::unauthorized",
            "passkey authentication failed",
        ),
        "a spent challenge must be refused as `401 passkey authentication failed`, the same \
         answer a bad signature gets"
    );
}

/// The same window exists on the registration ceremony, and it is closed the
/// same way.
#[test]
fn register_finish_spends_its_challenge_before_it_stores_a_credential() {
    let handler = handler_source("register_finish");

    let verify_at = handler.call_at_or_panic(
        "finish_registration(",
        "register_finish must still verify the attestation",
    );
    let spend_at = handler.call_at_or_panic(
        "spend_ceremony(",
        "register_finish must spend the ceremony's challenge: its cookie is \
         replayable for the same 300 seconds the login one is",
    );
    let store_at = handler.call_at_or_panic(
        "passkey_credential_ops::create(",
        "register_finish must still store the new credential",
    );

    assert!(
        verify_at < spend_at && spend_at < store_at,
        "the attestation must verify, then the challenge must be spent, and only then may a \
         credential be stored"
    );
    assert!(
        handler.refuses_between(
            spend_at,
            store_at,
            "AppError::bad_request",
            "passkey registration could not be verified",
        ),
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
            source_scan::calls(&handler_source(handler).body, "new_ceremony_id"),
            "{handler} must seal a fresh ceremony id into its state cookie"
        );
    }
}

/// The guards above read source, so what they read has to be code.
///
/// Every spelling they look for is call-shaped, which makes it equally
/// spellable in a comment or a Rust literal — and the ordering ones would then
/// be comparing the position of prose. The decoys below carry every spelling in
/// each non-code form at once; a live call after them is still seen.
#[test]
fn ceremony_guards_ignore_non_code_decoys_and_keep_live_calls() {
    const DECOYS: &str = r###"
let normal = "finish_authentication( spend_ceremony( touch_and_update( generate_token(";
let raw = r#"finish_registration( spend_ceremony( passkey_credential_ops::create("#;
let bytes = b"spend_ceremony( new_ceremony_id()";
// finish_authentication( spend_ceremony( touch_and_update( generate_token(
/* finish_registration( spend_ceremony( passkey_credential_ops::create( */
"###;

    let decoys = HandlerSource::new(DECOYS.to_string());
    for call in [
        "finish_authentication(",
        "finish_registration(",
        "spend_ceremony(",
        "touch_and_update(",
        "generate_token(",
        "passkey_credential_ops::create(",
    ] {
        assert_eq!(
            decoys.call_at(call),
            None,
            "`{call}` was read out of Rust data or prose"
        );
    }
    assert!(
        !source_scan::calls(DECOYS, "new_ceremony_id"),
        "a ceremony nonce cannot be minted by a comment or a literal"
    );

    let live = HandlerSource::new(format!(
        "{DECOYS}\nlet fresh = wa::new_ceremony_id();\nspend_ceremony(&state, &fresh).await?;\n"
    ));
    let spend_at = live
        .call_at("spend_ceremony(")
        .expect("a live spend after the decoys is still a call");
    assert!(
        live.call_at("touch_and_update(").is_none(),
        "only the live call may be found"
    );
    assert!(
        live.call_at("new_ceremony_id()")
            .is_some_and(|at| at < spend_at),
        "the live nonce is minted before the live spend"
    );
    assert!(source_scan::calls(&live.body, "new_ceremony_id"));
}

/// The refusal is a *value*, and the value has to come from the call that
/// actually runs.
#[test]
fn a_refusal_is_read_from_a_live_call_and_not_from_prose() {
    const DECOYS: &str = r###"
// AppError::unauthorized("passkey authentication failed")
let normal = "AppError::unauthorized(\"passkey authentication failed\")";
let raw = r#"AppError::unauthorized("passkey authentication failed")"#;
/* AppError::unauthorized("passkey authentication failed") */
return Err(AppError::unauthorized("invalid credentials"));
"###;

    let decoys = HandlerSource::new(DECOYS.to_string());
    assert!(
        !decoys.refuses_between(
            0,
            decoys.code.len(),
            "AppError::unauthorized",
            "passkey authentication failed",
        ),
        "quoted prose and a differently-worded live refusal are not the refusal a replay gets"
    );

    let live = HandlerSource::new(format!(
        "{DECOYS}return Err(AppError::unauthorized(\"passkey authentication failed\"));\n"
    ));
    assert!(live.refuses_between(
        0,
        live.code.len(),
        "AppError::unauthorized",
        "passkey authentication failed",
    ));
    assert!(
        !live.refuses_between(
            0,
            live.code.len(),
            "AppError::bad_request",
            "passkey authentication failed",
        ),
        "the constructor is part of the refusal: a 400 is not the answer a bad signature gets"
    );
}

fn handler_source(name: &str) -> HandlerSource {
    let source = std::fs::read_to_string(source_scan::src_root().join("api/passkeys.rs"))
        .expect("read the passkey handlers");
    HandlerSource::new(
        source_scan::functions(&source)
            .into_iter()
            .find(|f| f.name == name)
            .unwrap_or_else(|| panic!("{name} is declared in api/passkeys.rs"))
            .body,
    )
}
