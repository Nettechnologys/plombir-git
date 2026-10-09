//! What an account can do about itself, and how an administrator hands one
//! over — over real HTTP, through the production route table.
//!
//! * card_ca894e30ac80 — change the password (other sessions out, tokens
//!   kept), edit the profile, upload a picture, move to another address,
//!   delete the account.
//! * card_9f18b657580b — with registration closed and no identity provider,
//!   an administrator creates the account; its first sign-in has to replace
//!   the password the administrator chose, and so does a reset.
//! * card_45f98ab2fe1a — `verify-email` registration answers a taken and a
//!   free address identically, and creates the account only from the link.

use reqwest::StatusCode;
use rg_core::user::registration::RegistrationMode;
use sea_orm::{EntityTrait, PaginatorTrait};

use crate::common::{register_full, register_user, spawn_test_app_with_overrides, StateOverrides};

const PW: &str = "Qz7$wRtm";

fn client() -> reqwest::Client {
    reqwest::Client::new()
}

async fn login(base: &str, login: &str, password: &str) -> (StatusCode, serde_json::Value) {
    let resp = client()
        .post(format!("{base}/api/v1/users/login"))
        .json(&serde_json::json!({ "login": login, "password": password }))
        .send()
        .await
        .unwrap();
    let status = resp.status();
    (status, resp.json().await.unwrap_or(serde_json::Value::Null))
}

async fn me_status(base: &str, token: &str) -> StatusCode {
    client()
        .get(format!("{base}/api/v1/users/me"))
        .bearer_auth(token)
        .send()
        .await
        .unwrap()
        .status()
}

/// An SMTP relay nobody listens on: mail is "configured", nothing arrives.
fn unreachable_smtp() -> rg_core::email::SmtpConfig {
    rg_core::email::SmtpConfig::new("127.0.0.1", 9, "mailer", "secret", "noreply@example.com")
}

// ── card_ca894e30ac80 ────────────────────────────────────────────────────

/// The acceptance: changing the password signs every other session out, the
/// session that changed it continues on the token it is handed, and the
/// account's personal access tokens keep working.
#[tokio::test]
async fn a_password_change_signs_other_sessions_out_and_keeps_tokens() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let first = register_user(&base, "alice", "alice@example.com", PW).await;
    let (_, second) = login(&base, "alice", PW).await;
    let second = second["token"].as_str().unwrap().to_string();
    let pat: serde_json::Value = client()
        .post(format!("{base}/api/v1/users/tokens"))
        .bearer_auth(&first)
        .json(&serde_json::json!({ "name": "ci", "scopes": "repo" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let pat = pat["token"].as_str().unwrap().to_string();
    let created = client()
        .post(format!("{base}/api/v1/repos"))
        .bearer_auth(&first)
        .json(&serde_json::json!({ "name": "notes", "private": true }))
        .send()
        .await
        .unwrap();
    assert!(created.status().is_success(), "{}", created.status());
    let pat_reads_repo = |pat: String| {
        let base = base.clone();
        async move {
            client()
                .get(format!("{base}/api/v1/repos/alice/notes"))
                .bearer_auth(pat)
                .send()
                .await
                .unwrap()
                .status()
        }
    };
    assert_eq!(pat_reads_repo(pat.clone()).await, StatusCode::OK);

    let changed = client()
        .put(format!("{base}/api/v1/users/me/password"))
        .bearer_auth(&first)
        .json(&serde_json::json!({ "current_password": PW, "new_password": "N3w$ecret!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(changed.status(), StatusCode::OK);
    let changed: serde_json::Value = changed.json().await.unwrap();
    let fresh = changed["token"].as_str().unwrap();

    assert_eq!(me_status(&base, &first).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&base, &second).await, StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&base, fresh).await, StatusCode::OK);
    assert_eq!(
        pat_reads_repo(pat.clone()).await,
        StatusCode::OK,
        "a PAT outlives the change"
    );

    assert_eq!(login(&base, "alice", PW).await.0, StatusCode::UNAUTHORIZED);
    assert_eq!(login(&base, "alice", "N3w$ecret!").await.0, StatusCode::OK);
}

#[tokio::test]
async fn a_password_change_needs_the_current_password() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let token = register_user(&base, "bob", "bob@example.com", PW).await;
    let refused = client()
        .put(format!("{base}/api/v1/users/me/password"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "current_password": "wrong", "new_password": "N3w$ecret!" }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(me_status(&base, &token).await, StatusCode::OK);
}

#[tokio::test]
async fn the_profile_is_edited_and_cleared_by_its_owner() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let token = register_user(&base, "carol", "carol@example.com", PW).await;
    let saved: serde_json::Value = client()
        .patch(format!("{base}/api/v1/users/me"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "display_name": " Carol ", "bio": "builds things" }))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(saved["display_name"], "Carol");
    assert_eq!(saved["bio"], "builds things");

    let cleared = client()
        .patch(format!("{base}/api/v1/users/me"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "bio": null }))
        .send()
        .await
        .unwrap();
    assert_eq!(cleared.status(), StatusCode::OK);
    let cleared: serde_json::Value = cleared.json().await.unwrap();
    assert_eq!(
        cleared["display_name"], "Carol",
        "an absent key is left alone"
    );
    assert!(cleared["bio"].is_null());

    let too_long = client()
        .patch(format!("{base}/api/v1/users/me"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "display_name": "x".repeat(256) }))
        .send()
        .await
        .unwrap();
    assert_eq!(too_long.status(), StatusCode::BAD_REQUEST);
}

/// A picture is stored under the type its bytes are, served with `nosniff`,
/// and anything that is not one of the accepted images is refused — an SVG
/// above all, which is a document that can run script.
#[tokio::test]
async fn an_avatar_is_served_as_the_image_its_bytes_are() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let token = register_user(&base, "dave", "dave@example.com", PW).await;
    let png = [b"\x89PNG\r\n\x1a\n".as_slice(), &[0u8; 32]].concat();

    let uploaded = client()
        .put(format!("{base}/api/v1/users/me/avatar"))
        .bearer_auth(&token)
        .header("content-type", "text/html")
        .body(png.clone())
        .send()
        .await
        .unwrap();
    assert_eq!(uploaded.status(), StatusCode::OK);
    let url = uploaded.json::<serde_json::Value>().await.unwrap()["avatar_url"]
        .as_str()
        .unwrap()
        .to_string();
    assert!(url.starts_with("/api/v1/avatars/dave?v="), "{url}");

    let served = client().get(format!("{base}{url}")).send().await.unwrap();
    assert_eq!(served.status(), StatusCode::OK);
    assert_eq!(served.headers()["content-type"], "image/png");
    assert_eq!(served.headers()["x-content-type-options"], "nosniff");
    assert_eq!(served.bytes().await.unwrap().as_ref(), png.as_slice());

    let svg = client()
        .put(format!("{base}/api/v1/users/me/avatar"))
        .bearer_auth(&token)
        .body(r#"<svg xmlns="http://www.w3.org/2000/svg"><script>alert(1)</script></svg>"#)
        .send()
        .await
        .unwrap();
    assert_eq!(svg.status(), StatusCode::BAD_REQUEST);

    let removed = client()
        .delete(format!("{base}/api/v1/users/me/avatar"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(removed.status(), StatusCode::NO_CONTENT);
    let gone = client().get(format!("{base}{url}")).send().await.unwrap();
    assert_eq!(gone.status(), StatusCode::NOT_FOUND);
}

/// An account deletes itself with its password — and the instance's only
/// administrator cannot, or nobody could administer it afterwards.
#[tokio::test]
async fn an_account_deletes_itself_but_not_the_last_administrator() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let founder = register_user(&base, "founder", "founder@example.com", PW).await;
    let leaver = register_user(&base, "leaver", "leaver@example.com", PW).await;

    let last_admin = client()
        .delete(format!("{base}/api/v1/users/me"))
        .bearer_auth(&founder)
        .json(&serde_json::json!({ "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(last_admin.status(), StatusCode::CONFLICT);

    let wrong = client()
        .delete(format!("{base}/api/v1/users/me"))
        .bearer_auth(&leaver)
        .json(&serde_json::json!({ "password": "wrong" }))
        .send()
        .await
        .unwrap();
    assert_eq!(wrong.status(), StatusCode::UNAUTHORIZED);

    let deleted = client()
        .delete(format!("{base}/api/v1/users/me"))
        .bearer_auth(&leaver)
        .json(&serde_json::json!({ "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(deleted.status(), StatusCode::OK);
    assert!(rg_db::ops::user_ops::find_by_username(&db, "leaver")
        .await
        .unwrap()
        .is_none());
    assert_eq!(login(&base, "leaver", PW).await.0, StatusCode::UNAUTHORIZED);
}

/// Without outbound mail there is no way to prove a new address, so the
/// change is refused out loud rather than applied unproved.
#[tokio::test]
async fn an_address_change_needs_mail_to_confirm_it() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let token = register_user(&base, "erin", "erin@example.com", PW).await;
    let refused = client()
        .post(format!("{base}/api/v1/users/me/email"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "email": "erin@new.example", "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(refused.status(), StatusCode::CONFLICT);
}

/// With mail, the address changes only once the link is followed.
#[tokio::test]
async fn an_address_moves_when_its_link_is_followed() {
    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        smtp_config: Some(unreachable_smtp()),
        external_url: Some("https://git.example.test".to_string()),
        ..Default::default()
    })
    .await;
    let (token, user_id) = register_full(&base, "frank", "frank@example.com").await;
    let asked = client()
        .post(format!("{base}/api/v1/users/me/email"))
        .bearer_auth(&token)
        .json(&serde_json::json!({ "email": "frank@new.example", "password": PW }))
        .send()
        .await
        .unwrap();
    assert_eq!(asked.status(), StatusCode::ACCEPTED);
    let user = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        user.email, "frank@example.com",
        "nothing moves before the link"
    );
    assert_eq!(
        user.email_verified_at, None,
        "an address typed into an open registration is not proved"
    );

    // The token itself only ever leaves in the mail; re-issue the pending row
    // with one this test knows, the way the request above wrote it.
    let token_hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"known-token"));
    rg_db::ops::email_confirmation_ops::replace_pending_email_change(
        &db,
        user_id,
        "frank@new.example",
        &token_hash,
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    let confirmed = client()
        .post(format!("{base}/api/v1/users/verify-email"))
        .json(&serde_json::json!({ "token": "known-token" }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::OK);
    let user = rg_db::ops::user_ops::find_by_id(&db, user_id)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.email, "frank@new.example");
    assert!(
        user.email_verified_at.is_some(),
        "the followed link proved the new address"
    );

    let again = client()
        .post(format!("{base}/api/v1/users/verify-email"))
        .json(&serde_json::json!({ "token": "known-token" }))
        .send()
        .await
        .unwrap();
    assert_eq!(again.status(), StatusCode::BAD_REQUEST, "a link works once");
}

// ── card_9f18b657580b ────────────────────────────────────────────────────

/// The acceptance: registration closed, no identity provider — the
/// administrator creates the account, its holder signs in and has to choose
/// a password before anything opens.
#[tokio::test]
async fn an_administrator_adds_a_colleague_to_a_closed_instance() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides {
        registration: Some(RegistrationMode::Closed),
        ..Default::default()
    })
    .await;
    // The first account on an empty instance is admitted and is its admin.
    let admin = register_user(&base, "root", "root@example.com", PW).await;
    let closed = client()
        .post(format!("{base}/api/v1/users/register"))
        .json(
            &serde_json::json!({ "username": "gina", "email": "gina@example.com", "password": PW }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(closed.status(), StatusCode::FORBIDDEN);

    let created = client()
        .post(format!("{base}/api/v1/admin/users"))
        .bearer_auth(&admin)
        .json(&serde_json::json!({ "username": "gina", "email": "gina@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let created: serde_json::Value = created.json().await.unwrap();
    let temporary = created["temporary_password"].as_str().unwrap().to_string();
    assert_eq!(created["user"]["username"], "gina");

    // The handed-over password opens no session.
    let (status, body) = login(&base, "gina", &temporary).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["password_change_required"], true);
    assert_eq!(body["token"], "");

    // Nor any other door: the registry's Basic login says why.
    let registry = client()
        .get(format!("{base}/v2/auth/token?service=plombir-git"))
        .basic_auth("gina", Some(&temporary))
        .send()
        .await
        .unwrap();
    assert_eq!(registry.status(), StatusCode::UNAUTHORIZED);
    assert!(registry.text().await.unwrap().contains("administrator"));

    // A weak replacement is refused and changes nothing.
    let weak = client()
        .post(format!("{base}/api/v1/users/password/initial"))
        .json(
            &serde_json::json!({ "login": "gina", "password": temporary, "new_password": "short" }),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(weak.status(), StatusCode::BAD_REQUEST);

    let chosen = client()
        .post(format!("{base}/api/v1/users/password/initial"))
        .json(&serde_json::json!({
            "login": "gina",
            "password": temporary,
            "new_password": "G1na$own",
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(chosen.status(), StatusCode::OK);
    let chosen: serde_json::Value = chosen.json().await.unwrap();
    assert_eq!(
        me_status(&base, chosen["token"].as_str().unwrap()).await,
        StatusCode::OK
    );

    assert_eq!(
        login(&base, "gina", &temporary).await.0,
        StatusCode::UNAUTHORIZED
    );
    let (status, body) = login(&base, "gina", "G1na$own").await;
    assert_eq!(status, StatusCode::OK);
    assert!(body.get("password_change_required").is_none());

    // Once replaced, there is nothing left to replace.
    let nothing_pending = client()
        .post(format!("{base}/api/v1/users/password/initial"))
        .json(&serde_json::json!({ "login": "gina", "password": "G1na$own", "new_password": "An0ther$pw" }))
        .send()
        .await
        .unwrap();
    assert_eq!(nothing_pending.status(), StatusCode::CONFLICT);
}

/// A reset by an administrator signs the account out everywhere and owes a
/// password change at the next sign-in, like a new account.
#[tokio::test]
async fn an_administrator_reset_signs_out_and_owes_a_change() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let admin = register_user(&base, "root", "root@example.com", PW).await;
    let (holder, holder_id) = register_full(&base, "hank", "hank@example.com").await;

    let own = client()
        .post(format!("{base}/api/v1/admin/users/1/password-reset"))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap();
    assert_eq!(
        own.status(),
        StatusCode::BAD_REQUEST,
        "not the administrator's own"
    );

    let reset = client()
        .post(format!(
            "{base}/api/v1/admin/users/{holder_id}/password-reset"
        ))
        .bearer_auth(&admin)
        .send()
        .await
        .unwrap();
    assert_eq!(reset.status(), StatusCode::OK);
    let temporary = reset.json::<serde_json::Value>().await.unwrap()["temporary_password"]
        .as_str()
        .unwrap()
        .to_string();

    assert_eq!(me_status(&base, &holder).await, StatusCode::UNAUTHORIZED);
    assert_eq!(login(&base, "hank", PW).await.0, StatusCode::UNAUTHORIZED);
    let (_, body) = login(&base, "hank", &temporary).await;
    assert_eq!(body["password_change_required"], true);

    let not_admin = client()
        .post(format!(
            "{base}/api/v1/admin/users/{holder_id}/password-reset"
        ))
        .bearer_auth(&holder)
        .send()
        .await
        .unwrap();
    assert!(
        not_admin.status() == StatusCode::UNAUTHORIZED
            || not_admin.status() == StatusCode::FORBIDDEN
    );
}

#[tokio::test]
async fn an_administrator_cannot_create_a_taken_name() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let admin = register_user(&base, "root", "root@example.com", PW).await;
    for body in [
        serde_json::json!({ "username": "root", "email": "other@example.com" }),
        serde_json::json!({ "username": "other", "email": "root@example.com" }),
    ] {
        let taken = client()
            .post(format!("{base}/api/v1/admin/users"))
            .bearer_auth(&admin)
            .json(&body)
            .send()
            .await
            .unwrap();
        assert_eq!(taken.status(), StatusCode::CONFLICT, "{body}");
    }
    let reserved = client()
        .post(format!("{base}/api/v1/admin/users"))
        .bearer_auth(&admin)
        .json(&serde_json::json!({ "username": "settings", "email": "s@example.com" }))
        .send()
        .await
        .unwrap();
    assert_eq!(reserved.status(), StatusCode::BAD_REQUEST);
}

// ── card_45f98ab2fe1a ────────────────────────────────────────────────────

fn verify_email_instance() -> StateOverrides {
    StateOverrides {
        registration: Some(RegistrationMode::VerifyEmail),
        smtp_config: Some(unreachable_smtp()),
        external_url: Some("https://git.example.test".to_string()),
        ..Default::default()
    }
}

async fn register(base: &str, username: &str, email: &str) -> (StatusCode, Vec<u8>) {
    let resp = client()
        .post(format!("{base}/api/v1/users/register"))
        .json(&serde_json::json!({ "username": username, "email": email, "password": PW }))
        .send()
        .await
        .unwrap();
    (resp.status(), resp.bytes().await.unwrap().to_vec())
}

/// The acceptance: a taken address and a free one get the same status and the
/// same body, and neither creates an account until the link is followed.
#[tokio::test]
async fn a_taken_and_a_free_address_get_the_same_answer() {
    let (base, db) = spawn_test_app_with_overrides(verify_email_instance()).await;
    // The bootstrap account is created at once, mail or no mail.
    register_user(&base, "root", "root@example.com", PW).await;
    let accounts = rg_db::entities::user::Entity::find()
        .count(&db)
        .await
        .unwrap();

    let free = register(&base, "ivan", "ivan@example.com").await;
    let taken = register(&base, "jill", "root@example.com").await;
    assert_eq!(free.0, StatusCode::ACCEPTED);
    assert_eq!(
        free, taken,
        "the answer must not say whether the address has an account"
    );
    assert_eq!(
        rg_db::entities::user::Entity::find()
            .count(&db)
            .await
            .unwrap(),
        accounts,
        "nothing exists before the address is proved"
    );

    // A username is public, and refused out loud as on every other door.
    let (status, _) = register(&base, "root", "someone@example.com").await;
    assert_eq!(status, StatusCode::CONFLICT);
}

/// Following the link creates the account the registration described, signed
/// in; the link is spent by doing so.
#[tokio::test]
async fn the_link_creates_the_account() {
    let (base, db) = spawn_test_app_with_overrides(verify_email_instance()).await;
    register_user(&base, "root", "root@example.com", PW).await;
    assert_eq!(
        register(&base, "kate", "kate@example.com").await.0,
        StatusCode::ACCEPTED
    );

    // Re-issue the pending row with a token this test knows.
    let token_hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"kate-link"));
    let password_hash = rg_core::auth::password::hash_password(PW).await.unwrap();
    rg_db::ops::email_confirmation_ops::replace_pending_registration(
        &db,
        "kate@example.com",
        "kate",
        &password_hash,
        &token_hash,
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();

    let confirmed = client()
        .post(format!("{base}/api/v1/users/verify-email"))
        .json(&serde_json::json!({ "token": "kate-link" }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::CREATED);
    let body: serde_json::Value = confirmed.json().await.unwrap();
    assert_eq!(body["username"], "kate");
    assert_eq!(
        me_status(&base, body["token"].as_str().unwrap()).await,
        StatusCode::OK
    );
    assert_eq!(login(&base, "kate", PW).await.0, StatusCode::OK);
    let user = rg_db::ops::user_ops::find_by_username(&db, "kate")
        .await
        .unwrap()
        .unwrap();
    assert!(
        !user.is_admin,
        "only the bootstrap account is an administrator"
    );
    assert!(
        user.email_verified_at.is_some(),
        "an account created by its confirmation link has a proved address"
    );
}

// ── card_2296f052332b ────────────────────────────────────────────────────

/// An account whose address was only typed — an open registration — proves it
/// by a mailed link: the link marks `email_verified_at`, a second request is
/// refused as needless, and a link that outlived an address change proves
/// nothing. Without outbound mail the instance says so instead of pretending.
#[tokio::test]
async fn an_existing_account_proves_its_address_by_a_link() {
    let (base, _db) = spawn_test_app_with_overrides(StateOverrides::default()).await;
    let token = register_user(&base, "nomail", "nomail@example.com", PW).await;
    let refused = client()
        .post(format!("{base}/api/v1/users/me/email/verify"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        refused.status(),
        StatusCode::CONFLICT,
        "no outbound mail, no proof — the instance's state, not the caller's rights"
    );

    let (base, db) = spawn_test_app_with_overrides(StateOverrides {
        smtp_config: Some(unreachable_smtp()),
        external_url: Some("https://git.example.test".to_string()),
        ..Default::default()
    })
    .await;
    let (token, user_id) = register_full(&base, "olga", "olga@example.com").await;
    let me = |token: String| {
        let base = base.clone();
        async move {
            client()
                .get(format!("{base}/api/v1/users/me"))
                .bearer_auth(token)
                .send()
                .await
                .unwrap()
                .json::<serde_json::Value>()
                .await
                .unwrap()
        }
    };
    assert!(
        me(token.clone()).await["email_verified_at"].is_null(),
        "an open registration's address is not proved"
    );
    let asked = client()
        .post(format!("{base}/api/v1/users/me/email/verify"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(asked.status(), StatusCode::ACCEPTED);
    let pending = rg_db::entities::email_confirmation::Entity::find()
        .all(&db)
        .await
        .unwrap()
        .into_iter()
        .filter(|row| {
            row.user_id == Some(user_id)
                && row.purpose == rg_db::entities::email_confirmation::PURPOSE_EMAIL_VERIFY
        })
        .count();
    assert_eq!(pending, 1, "the request recorded one live link");

    // A link for an address the account has since left proves nothing.
    let stale_hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"stale-link"));
    rg_db::ops::email_confirmation_ops::replace_pending_email_verification(
        &db,
        user_id,
        "olga@old.example",
        &stale_hash,
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    let stale = client()
        .post(format!("{base}/api/v1/users/verify-email"))
        .json(&serde_json::json!({ "token": "stale-link" }))
        .send()
        .await
        .unwrap();
    assert_eq!(stale.status(), StatusCode::CONFLICT);
    assert!(me(token.clone()).await["email_verified_at"].is_null());

    // The token only ever leaves in the mail; re-issue the row with a known one.
    let token_hash = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(b"olga-link"));
    rg_db::ops::email_confirmation_ops::replace_pending_email_verification(
        &db,
        user_id,
        "olga@example.com",
        &token_hash,
        chrono::Utc::now() + chrono::Duration::hours(1),
    )
    .await
    .unwrap();
    let confirmed = client()
        .post(format!("{base}/api/v1/users/verify-email"))
        .json(&serde_json::json!({ "token": "olga-link" }))
        .send()
        .await
        .unwrap();
    assert_eq!(confirmed.status(), StatusCode::OK);
    let body: serde_json::Value = confirmed.json().await.unwrap();
    assert_eq!(body["email_verified"], true);
    assert!(
        me(token.clone()).await["email_verified_at"].is_string(),
        "the followed link proved the address"
    );

    let again = client()
        .post(format!("{base}/api/v1/users/me/email/verify"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    assert_eq!(
        again.status(),
        StatusCode::BAD_REQUEST,
        "a proved address needs no new link"
    );
}
