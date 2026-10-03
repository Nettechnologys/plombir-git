//! card_08400088bb40: re-opening the enrolment wizard must not destroy the
//! factor that is holding the account.
//!
//! `POST /users/mfa/setup` used to write the freshly generated secret straight
//! into `users.totp_secret` — the column `POST /users/mfa/verify` checks against
//! — while `users.mfa_enabled` stayed `true` and nothing asked for a password or
//! for a confirmation. So an account with a working authenticator whose owner
//! merely *looked* at the wizard again (to move to a new phone, or because a
//! failed backup-code read had told the page MFA was off) came out of it
//! demanding a second factor whose secret nobody held: the app kept computing
//! codes from the old secret, the server verified against the new one, and no
//! code ever matched again. Backup codes were the only way in; without them, an
//! administrator.
//!
//! What the endpoints promise now, and what each test below pins:
//!
//!   * `setup` writes a slot no login reads, so abandoning it costs nothing;
//!   * the step that *replaces* a live authenticator asks for the account
//!     password, the same question `POST /users/mfa/disable` asks, because it is
//!     the same event — the factor protecting the account stops working;
//!   * a completed rotation is journalled as such, so an incident review can see
//!     which authenticator stopped working and when;
//!   * a secret handed out and left lying around cannot arm a factor forever.
//!
//! A first enrolment — the account has no second factor to lose — is unchanged,
//! and `mfa_enable_atomicity_tests` still holds that path.

use rg_db::sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

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

/// The step a code generated right now belongs to.
fn current_step() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::SystemTime::UNIX_EPOCH)
        .expect("a clock after the epoch")
        .as_secs()
        / 30
}

/// One `POST /users/mfa/setup`, returning the secret it handed out.
async fn setup(base: &str, token: &str) -> String {
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/setup"))
        .bearer_auth(token)
        .send()
        .await
        .expect("setup request");
    let status = response.status();
    let body: serde_json::Value = response.json().await.expect("setup response body");
    assert_eq!(status, 200, "setup failed: {body}");
    body["secret"]
        .as_str()
        .expect("setup returned no secret")
        .to_string()
}

/// One `POST /users/mfa/enable`, as `(status, body)`.
async fn enable(
    base: &str,
    token: &str,
    code: &str,
    password: Option<&str>,
) -> (u16, serde_json::Value) {
    let mut request = serde_json::json!({ "code": code });
    if let Some(password) = password {
        request["password"] = serde_json::Value::String(password.to_string());
    }
    let response = reqwest::Client::new()
        .post(format!("{base}/api/v1/users/mfa/enable"))
        .bearer_auth(token)
        .json(&request)
        .send()
        .await
        .expect("enable request");
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().await.expect("enable response body");
    (status, body)
}

/// Enrol TOTP over the real endpoints and hand back the live secret.
async fn enrol(base: &str, token: &str) -> String {
    let secret = setup(base, token).await;
    let (status, body) = enable(base, token, &code_for_step(&secret, current_step()), None).await;
    assert_eq!(status, 200, "first enrolment must succeed: {body}");
    secret
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

/// The card's scenario: the wizard is opened again and walked away from.
///
/// Nothing about this sequence says "rotate my factor", and the account is left
/// exactly as it was — which is the whole fix, stated as the login the owner
/// makes the next morning.
#[tokio::test]
async fn an_abandoned_re_enrolment_leaves_the_live_factor_working() {
    let (base, _db) = spawn_test_app_with_db().await;
    let username = "mfa_reenrol_abandoned";
    let (token, _user_id) = register_full(&base, username, "mfa_reenrol@example.com").await;
    let live = enrol(&base, &token).await;

    // Step 1 of 2, and then nothing: no `enable`, no code, the tab closed.
    let staged = setup(&base, &token).await;
    assert_ne!(staged, live, "setup handed out the secret already in use");

    let challenge = primary_factor(&base, username).await;
    let step = current_step();

    let (status, session) = verify(&base, &challenge, username, &code_for_step(&live, step)).await;
    assert_eq!(
        status, 200,
        "the authenticator the account was enrolled with stopped working after an abandoned setup"
    );
    assert!(
        !session.is_empty(),
        "a successful verify must issue a token"
    );

    // The other half of the same statement: the secret nobody confirmed is not
    // the factor either, so `setup` armed nothing.
    let (staged_status, staged_session) = verify(
        &base,
        &challenge,
        username,
        &code_for_step(&staged, step + 1),
    )
    .await;
    assert_eq!(
        staged_status, 401,
        "a secret from an abandoned setup passed the second factor"
    );
    assert!(staged_session.is_empty());
}

/// Replacing a live authenticator is the event `disable` asks a password for.
///
/// Without this, a stolen session is enough to move the second factor onto the
/// thief's own phone: `setup` hands the session holder a secret, and `enable`
/// would take their code as proof. The password is what the thief does not have.
#[tokio::test]
async fn replacing_the_live_authenticator_requires_the_account_password() {
    let (base, _db) = spawn_test_app_with_db().await;
    let username = "mfa_reenrol_rotation";
    let (token, _user_id) = register_full(&base, username, "mfa_rotation@example.com").await;
    let live = enrol(&base, &token).await;
    let staged = setup(&base, &token).await;

    let (refused, body) =
        enable(&base, &token, &code_for_step(&staged, current_step()), None).await;
    assert_eq!(
        refused, 400,
        "a code alone replaced the factor protecting the account: {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("password"),
        "the refusal must say what is missing: {body}"
    );

    let (wrong, body) = enable(
        &base,
        &token,
        &code_for_step(&staged, current_step()),
        Some("not-the-account-password"),
    )
    .await;
    assert_eq!(wrong, 401, "a wrong password replaced the factor: {body}");

    // Still the old authenticator, after both refusals.
    let challenge = primary_factor(&base, username).await;
    let step = current_step();
    let (status, session) = verify(&base, &challenge, username, &code_for_step(&live, step)).await;
    assert_eq!(status, 200, "a refused rotation broke the live factor");
    assert!(!session.is_empty());

    // And with the password, the rotation goes through.
    let (accepted, body) = enable(
        &base,
        &token,
        &code_for_step(&staged, current_step()),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(accepted, 200, "a confirmed rotation was refused: {body}");
    assert!(
        !body["backup_codes"]
            .as_array()
            .expect("a rotation must publish a recovery set")
            .is_empty(),
        "a rotation left the account with no backup codes: {body}"
    );

    // The spent-step marker went with the secret it belonged to, so the first
    // code of the new authenticator is accepted rather than read as a replay of
    // the login above.
    let challenge = primary_factor(&base, username).await;
    let step = current_step();
    let (new_status, new_session) =
        verify(&base, &challenge, username, &code_for_step(&staged, step)).await;
    assert_eq!(
        new_status, 200,
        "the authenticator the rotation confirmed does not open the account"
    );
    assert!(!new_session.is_empty());

    let (old_status, old_session) =
        verify(&base, &challenge, username, &code_for_step(&live, step + 1)).await;
    assert_eq!(
        old_status, 401,
        "the replaced authenticator still passes the second factor"
    );
    assert!(old_session.is_empty());
}

/// The journal has to tell a rotation apart from a first enrolment.
///
/// Both are `user.enable_mfa`; only one of them means "the authenticator this
/// account was using stopped working at this moment", which is the sentence an
/// incident review is reading the journal for.
#[tokio::test]
async fn a_rotation_is_journalled_as_the_end_of_the_previous_factor() {
    let (base, _db) = spawn_test_app_with_db().await;
    let username = "mfa_reenrol_journal";
    let (token, _user_id) = register_full(&base, username, "mfa_journal@example.com").await;
    let live = enrol(&base, &token).await;
    let staged = setup(&base, &token).await;
    let (accepted, body) = enable(
        &base,
        &token,
        &code_for_step(&staged, current_step()),
        Some(PASSWORD),
    )
    .await;
    assert_eq!(accepted, 200, "{body}");

    // The first registered account is the instance administrator, so this is the
    // owner reading their own events rather than a second, privileged actor.
    let response = reqwest::Client::new()
        .get(format!("{base}/api/v1/admin/audit/logs"))
        .query(&[("per_page", "100")])
        .bearer_auth(&token)
        .send()
        .await
        .expect("read the audit journal");
    let status = response.status();
    let text = response.text().await.expect("journal body");
    assert_eq!(status, 200, "reading the journal failed: {text}");
    let rows = serde_json::from_str::<serde_json::Value>(&text).expect("journal is JSON")["logs"]
        .as_array()
        .expect("`logs` array")
        .clone();

    let armed: Vec<serde_json::Value> = rows
        .iter()
        .filter(|row| row["action"] == "user.enable_mfa")
        .map(|row| {
            serde_json::from_str(
                row["details"]
                    .as_str()
                    .unwrap_or_else(|| panic!("`user.enable_mfa` recorded no details: {row}")),
            )
            .expect("details are JSON")
        })
        .collect();
    assert_eq!(
        armed.len(),
        2,
        "the journal must hold the first enrolment and the rotation: {rows:?}"
    );
    assert_eq!(
        armed
            .iter()
            .filter(|details| details["replaced_existing_factor"] == serde_json::json!(true))
            .count(),
        1,
        "exactly one of the two events retired an authenticator: {armed:?}"
    );

    // The secret is the factor; neither the one that was retired nor the one now
    // in use belongs in a journal served over the admin API.
    for secret in [&live, &staged] {
        assert!(
            !text.contains(secret.as_str()),
            "a TOTP secret reached `audit_log`"
        );
    }
}

/// A secret handed out and forgotten stops being able to arm a factor.
///
/// The setup response is the one place the plaintext secret is ever shown. With
/// no bound, a screenshot of that QR code stays a way to switch a second factor
/// on for as long as the account exists.
#[tokio::test]
async fn a_stale_setup_cannot_arm_a_factor() {
    let (base, db) = spawn_test_app_with_db().await;
    let username = "mfa_reenrol_stale";
    let (token, user_id) = register_full(&base, username, "mfa_stale@example.com").await;
    let staged = setup(&base, &token).await;

    // Age the enrolment past its window. The clock is the only input this branch
    // has, and moving the row is how a test reaches tomorrow.
    rg_db::entities::user::Entity::update_many()
        .col_expr(
            rg_db::entities::user::Column::PendingTotpSecretAt,
            rg_db::sea_orm::sea_query::Expr::value(Some(
                chrono::Utc::now() - chrono::Duration::minutes(31),
            )),
        )
        .filter(rg_db::entities::user::Column::Id.eq(user_id))
        .exec(&db)
        .await
        .expect("age the pending enrolment");

    let (status, body) = enable(&base, &token, &code_for_step(&staged, current_step()), None).await;
    assert_eq!(
        status, 400,
        "a setup from another day still armed the account: {body}"
    );
    assert!(
        body["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("expired"),
        "the refusal must say the setup has to be started again: {body}"
    );

    // And starting again works: the window is a bound on one enrolment, not a
    // way to wedge the account out of ever getting a second factor.
    let fresh = setup(&base, &token).await;
    let (retried, body) = enable(&base, &token, &code_for_step(&fresh, current_step()), None).await;
    assert_eq!(retried, 200, "a restarted enrolment must succeed: {body}");
}
