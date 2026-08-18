//! Acceptance for card_f341d219589f: a passkey login may not issue a JWT for a
//! signature counter it failed to store.
//!
//! `login_finish` used to log the failure of
//! `passkey_credential_ops::touch_and_update` and carry on to
//! `generate_token`. The write it dropped is not an audit trail: it holds the
//! counter and backup state the authenticator reported for *this* assertion,
//! and those are what the next assertion is compared against to spot a cloned
//! or replayed credential. A login confirmed against state the server did not
//! keep is the whole defect.
//!
//! # Why the guard, and what it costs
//!
//! The obvious test — drive `POST /users/passkeys/login/finish` with a closed
//! pool — cannot reach the code in question. Producing a *verified* assertion
//! needs a real authenticator (`webauthn-authenticator-rs`, not a dependency of
//! this workspace), and without one the request dies at the challenge cookie,
//! several hundred lines before the counter write. A closed pool would answer
//! `503` from `user_ops::find_by_id` and the test would be green for a reason
//! that has nothing to do with the counter.
//!
//! So the property is held in two halves, and both are needed:
//!
//! * The **database seam** is driven for real — [`touch_and_update`] against a
//!   live sqlite database, a deleted row, and a closed pool — because the three
//!   answers the handler now branches on are exactly what it used to conflate.
//! * The **handler's use of it** is read out of the source, because "the error
//!   is propagated rather than logged" is a line of code that was missing, and
//!   no request this suite can build exercises a missing `?`.
//!
//! What the second half cannot see: that the propagated error carries the right
//! status (the first half proves that for the DB error itself), or that the
//! ordering holds through some future refactor that moves the write behind a
//! helper. It reads `login_finish`'s own body — a counter write relocated into
//! a callee would need this guard taught about it.

use axum::http::StatusCode;
use rg_db::ops::passkey_credential_ops::{self, CounterWrite};
use rg_http::error::AppError;

use crate::common::{setup_test_db, source_scan};

/// A user plus one registered passkey row, which is all the counter write
/// touches. The stored blob is opaque to `touch_and_update` — it re-serializes
/// whatever the handler hands it — so a marker string is enough to prove *which*
/// value landed in the column.
async fn seed_passkey(db: &rg_db::DatabaseConnection) -> (i64, i64) {
    let user = rg_db::ops::user_ops::create_user(
        db,
        "passkey-counter-user",
        "passkey-counter@example.com",
        "hash",
        "Passkey Counter",
    )
    .await
    .expect("create passkey fixture user");

    let passkey = passkey_credential_ops::create(
        db,
        user.id,
        "credential-id",
        "{\"counter\":1}",
        "yubikey",
        "passkeys.example.test",
    )
    .await
    .expect("register fixture passkey");
    (user.id, passkey.id)
}

/// The happy half of the contract the handler now depends on: the write reports
/// that it happened, and the advanced blob is the one left in the column.
#[tokio::test]
async fn an_advanced_counter_is_actually_persisted() {
    let (db, _dir) = setup_test_db().await;
    let (user_id, passkey_id) = seed_passkey(&db).await;

    let stored = passkey_credential_ops::touch_and_update(
        &db,
        passkey_id,
        "{\"counter\":1}",
        "{\"counter\":2}",
    )
    .await
    .expect("store the advanced counter");
    assert_eq!(
        stored,
        CounterWrite::Stored,
        "an existing passkey row still holding the verified snapshot must report a stored write"
    );

    let row = passkey_credential_ops::list_by_user(&db, user_id)
        .await
        .expect("read passkeys back")
        .into_iter()
        .find(|m| m.id == passkey_id)
        .expect("the fixture passkey is still registered");
    assert_eq!(
        row.passkey, "{\"counter\":2}",
        "the column must hold the advanced credential, not the pre-assertion one"
    );
    assert_eq!(
        row.rp_id.as_deref(),
        Some("passkeys.example.test"),
        "advancing the counter must not lose the credential's relying-party id"
    );
    assert!(
        row.last_used_at.is_some(),
        "a successful assertion must stamp last_used_at"
    );
}

/// The distinction the old signature could not express. A revoked credential is
/// not a failed write: it ends the ceremony, while a failed write is a retryable
/// outage. Folding the first into `DbErr` is what left one `if let Err` arm
/// standing for both — and that arm swallowed them.
#[tokio::test]
async fn a_missing_passkey_row_is_reported_as_absence_not_as_a_write_failure() {
    let (db, _dir) = setup_test_db().await;
    let (user_id, passkey_id) = seed_passkey(&db).await;

    let removed = passkey_credential_ops::delete(&db, user_id, passkey_id)
        .await
        .expect("revoke the fixture passkey");
    assert!(removed, "the fixture passkey must have been registered");

    let stored = passkey_credential_ops::touch_and_update(
        &db,
        passkey_id,
        "{\"counter\":1}",
        "{\"counter\":2}",
    )
    .await
    .expect("a vanished row is an answer, not an error");
    assert_eq!(
        stored,
        CounterWrite::Missing,
        "a passkey that is no longer registered must report absence, not a lost race"
    );
}

/// The outage half: the write fails, and it fails as the retryable 503 the rest
/// of the API classifies a dead pool as — never as a 4xx blaming the client for
/// an assertion that verified.
#[tokio::test]
async fn a_closed_pool_makes_the_counter_write_a_retryable_503() {
    let (db, _dir) = setup_test_db().await;
    let (_user_id, passkey_id) = seed_passkey(&db).await;

    db.clone()
        .close()
        .await
        .expect("close passkey fixture pool");

    let error = passkey_credential_ops::touch_and_update(
        &db,
        passkey_id,
        "{\"counter\":1}",
        "{\"counter\":2}",
    )
    .await
    .expect_err("a closed pool cannot store the advanced counter");
    let error = AppError::from(error);
    assert_eq!(
        error.status(),
        StatusCode::SERVICE_UNAVAILABLE,
        "a database outage during the counter write is ours and retryable"
    );
}

/// The byte-aligned code-only view every position in this file is read from.
///
/// Named once so the guard and its fixture cannot drift apart: the fixture
/// below is what proves this view is doing the work, and it can only prove it
/// if it is the same view.
fn code_view(text: &str) -> String {
    source_scan::rust_code_only(text)
}

/// Whether `region` hands a failure to the caller before it swallows one.
///
/// Read from the byte-aligned code-only view of the region: `login_finish`
/// explains each of these steps in the paragraph above it, and a guard that
/// counted the prose would be deciding the order of a sentence. A
/// call-shaped Rust literal is data for the same reason.
fn propagates_before_it_swallows(region: &str) -> bool {
    let code = code_view(region);
    let propagated = code.find("map_err(AppError::from)?").unwrap_or(usize::MAX);
    let swallowed = code
        .find("if let Err(")
        .unwrap_or(usize::MAX)
        .min(code.find("unwrap_or(").unwrap_or(usize::MAX));
    propagated < swallowed
}

/// The half no request can reach: `login_finish` must *propagate* the counter
/// write's failure, and it must do so before it mints the token.
///
/// Read out of the handler's own body rather than asserted over a response,
/// because the defect is a missing `?` — the swallowing form compiles, answers
/// `200`, and differs from the correct one only in the source.
#[test]
fn login_finish_cannot_issue_a_token_without_the_counter_write() {
    let source = std::fs::read_to_string(source_scan::src_root().join("api/passkeys.rs"))
        .expect("read the passkey handlers");
    let body = source_scan::functions(&source)
        .into_iter()
        .find(|f| f.name == "login_finish")
        .expect("login_finish is declared in api/passkeys.rs")
        .body;
    // Structural positions come from the code-only view; the byte alignment is
    // what lets the regions below still be sliced out of the original body.
    let code = code_view(&body);

    let serialize_at = code
        .find("passkey_to_json(")
        .expect("login_finish must still serialize the advanced credential");
    let write_at = code
        .find("touch_and_update(")
        .expect("login_finish must still store the advanced signature counter");
    let token_at = code
        .find("generate_token(")
        .expect("login_finish must still mint a session token");
    assert!(
        serialize_at < write_at && write_at < token_at,
        "the advanced credential must be serialized, then stored, and only then may a \
         token be minted — this ordering is the property the card is about"
    );

    // Each of the two steps has to hand its failure to the caller *before* the
    // next step is reached. `record_successful_login` and the login-log write
    // that follow are deliberately best-effort and keep their `if let Err`
    // arms, so the check is per-step rather than "no swallow anywhere below".
    for (label, region, reason) in [
        (
            "serializing the advanced credential",
            &body[serialize_at..write_at],
            "falling back to the stored blob re-stores the pre-assertion counter and calls it success",
        ),
        (
            "storing the advanced signature counter",
            &body[write_at..token_at],
            "a login confirmed against a counter we did not keep is the defect itself",
        ),
    ] {
        assert!(
            propagates_before_it_swallows(region),
            "{label} must propagate its failure with `map_err(AppError::from)?` \
             before anything else happens: {reason}"
        );
    }
}

/// What the guard above reads has to be code.
///
/// Both halves are spellable in prose — the handler's own paragraphs name the
/// calls and the fallback they refuse — and `map_err(AppError::from)?` inside a
/// comment above a swallowing arm would hold the swallow green.
#[test]
fn the_counter_write_guard_ignores_non_code_decoys_and_keeps_live_code() {
    const DECOYS: &str = r###"
// passkey_to_json( touch_and_update( generate_token( map_err(AppError::from)?
/* passkey_to_json( touch_and_update( map_err(AppError::from)? */
let normal = "passkey_to_json( touch_and_update( generate_token( map_err(AppError::from)?";
let raw = r#"touch_and_update( generate_token( map_err(AppError::from)?"#;
let bytes = b"generate_token( map_err(AppError::from)?";
"###;

    let decoys = code_view(DECOYS);
    for call in ["passkey_to_json(", "touch_and_update(", "generate_token("] {
        assert_eq!(
            decoys.find(call),
            None,
            "`{call}` was read out of Rust data or prose"
        );
    }

    assert!(
        !propagates_before_it_swallows(&format!(
            "{DECOYS}\nif let Err(error) = store(&json) {{ tracing::warn!(?error); }}\n"
        )),
        "a commented-out or quoted `?` does not propagate the swallowed failure below it"
    );
    assert!(
        propagates_before_it_swallows(&format!(
            "{DECOYS}\nlet stored = store(&json).map_err(AppError::from)?;\n\
             if let Err(error) = log(&stored) {{ tracing::warn!(?error); }}\n"
        )),
        "a live propagation before a deliberate best-effort arm is the shape the handler has"
    );
    assert!(
        !propagates_before_it_swallows(&format!(
            "{DECOYS}\nlet stored = store(&json).unwrap_or(previous);\n\
             let _ = stored.map_err(AppError::from)?;\n"
        )),
        "propagating after the fallback already happened is the defect, not the fix"
    );
}
