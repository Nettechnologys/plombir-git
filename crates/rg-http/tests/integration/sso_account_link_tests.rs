//! card_4753cfe7b985: an SSO identity joins an existing account only when that
//! account asks for it.
//!
//! A first sign-in through a provider used to attach the identity to whatever
//! account held the email address the provider asserted. Local registration
//! never verifies addresses, so an attacker could register `victim@corp.example`
//! with a password of their own and wait: the victim's first SSO sign-in landed
//! in the attacker's account, and everything the victim pushed afterwards was
//! readable with the attacker's password and tokens.
//!
//! Every test here drives the real `authorize → callback` (or `link →
//! callback`) round trip against a mock OIDC provider, and asserts on what the
//! round trip *left behind* — the `oauth_accounts` row, the journal, the session
//! cookie — not only on the status code: a refusal that still wrote the link
//! is the bug with a different number on it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::common::{
    build_test_app_state_with, register_full, setup_test_db, SsoCallbackOutcome, StateOverrides,
};
use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use rg_db::ops::sso_provider_ops::SsoProviderInput;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};

/// The jwt secret `build_test_app_state_with` signs sessions with.
const TEST_JWT_SECRET: &str = "test-secret-key";

/// Who the mock IdP says is signing in. Mutable, so one test can sign in as
/// two different people.
#[derive(Clone)]
struct Profile {
    sub: String,
    email: String,
    email_verified: Option<bool>,
}

#[derive(Clone)]
struct MockIdp {
    base_url: String,
    profile: Arc<Mutex<Profile>>,
}

async fn discovery(State(idp): State<MockIdp>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": idp.base_url,
        "authorization_endpoint": format!("{}/authorize", idp.base_url),
        "token_endpoint": format!("{}/token", idp.base_url),
        "userinfo_endpoint": format!("{}/userinfo", idp.base_url),
    }))
}

async fn token(Form(_form): Form<HashMap<String, String>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "access_token": "mock-access-token",
        "expires_in": 3600
    }))
}

async fn userinfo(State(idp): State<MockIdp>) -> Json<serde_json::Value> {
    let profile = idp.profile.lock().unwrap().clone();
    let mut body = serde_json::json!({
        "sub": profile.sub,
        "preferred_username": "provider-person",
        "email": profile.email,
        "name": "Provider Person"
    });
    if let Some(verified) = profile.email_verified {
        body["email_verified"] = serde_json::Value::Bool(verified);
    }
    Json(body)
}

fn set_cookie_pair(headers: &HeaderMap, name: &str) -> Option<String> {
    let prefix = format!("{name}=");
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&prefix))
        .map(|value| value.split(';').next().unwrap().to_string())
}

fn cookie_pair(headers: &HeaderMap, name: &str) -> String {
    set_cookie_pair(headers, name).unwrap_or_else(|| panic!("missing {name} cookie"))
}

/// The session cookie a response set, if it set a non-empty one.
fn issued_session(headers: &HeaderMap) -> Option<String> {
    set_cookie_pair(headers, "plombir_git_token")
        .and_then(|pair| pair.split_once('=').map(|(_, value)| value.to_string()))
        .filter(|value| !value.is_empty())
}

fn signed_cookie_value(cookie: &str) -> String {
    cookie
        .split_once('=')
        .unwrap()
        .1
        .rsplit_once(':')
        .unwrap()
        .0
        .to_string()
}

struct Callback {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
    outcome: SsoCallbackOutcome,
}

struct Harness {
    db: sea_orm::DatabaseConnection,
    base: String,
    client: reqwest::Client,
    profile: Arc<Mutex<Profile>>,
    app_server: tokio::task::JoinHandle<()>,
    idp_server: tokio::task::JoinHandle<()>,
    _app_dir: tempfile::TempDir,
}

impl Harness {
    async fn start(profile: Profile, auto_provision: bool) -> Harness {
        let idp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp_addr = idp_listener.local_addr().unwrap().to_string();
        let idp_base = format!("http://{idp_addr}");
        let profile = Arc::new(Mutex::new(profile));
        let idp = MockIdp {
            base_url: idp_base.clone(),
            profile: profile.clone(),
        };
        let idp_app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/userinfo", get(userinfo))
            .with_state(idp)
            .merge(Router::new().route("/token", post(token)));
        let idp_server = tokio::spawn(async move {
            axum::serve(idp_listener, idp_app).await.unwrap();
        });
        crate::common::wait_for_listener(&idp_addr).await;

        let (db, app_dir) = setup_test_db().await;
        let repo_root = app_dir.path().join("repos");
        std::fs::create_dir_all(&repo_root).unwrap();
        let discovery_url = format!("{idp_base}/.well-known/openid-configuration");
        rg_db::ops::sso_provider_ops::create(
            &db,
            SsoProviderInput {
                name: "Corp IdP",
                slug: "idp",
                provider_type: "oidc",
                client_id: Some("client-id"),
                discovery_url: Some(&discovery_url),
                scopes: Some("openid profile email"),
                enabled: true,
                auto_provision,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let app = rg_http::create_router_for_test(build_test_app_state_with(
            db.clone(),
            repo_root,
            StateOverrides {
                oidc_transport_policy: Some(
                    rg_core::auth::sso::OidcTransportPolicy::parse(std::slice::from_ref(&idp_base))
                        .expect("test IdP origin is exact"),
                ),
                ..Default::default()
            },
        ));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let base = format!("http://{addr}");
        let app_server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        crate::common::wait_for_listener(&addr).await;

        Harness {
            db,
            base,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            profile,
            app_server,
            idp_server,
            _app_dir: app_dir,
        }
    }

    fn sign_in_as(&self, profile: Profile) {
        *self.profile.lock().unwrap() = profile;
    }

    async fn callback(&self, cookies: &[String]) -> Callback {
        let state_cookie = cookies
            .iter()
            .find(|cookie| cookie.starts_with("plombir_git_sso_state="))
            .expect("a state cookie");
        let state = signed_cookie_value(state_cookie);
        let response = self
            .client
            .get(format!(
                "{}/api/v1/auth/sso/idp/callback?code=valid-code&state={state}",
                self.base
            ))
            .header(header::COOKIE, cookies.join("; "))
            .send()
            .await
            .unwrap();
        let outcome = SsoCallbackOutcome::of(&response);
        Callback {
            status: response.status(),
            headers: response.headers().clone(),
            body: response.text().await.unwrap(),
            outcome,
        }
    }

    /// An ordinary sign-in, exactly as a browser drives it from the login page.
    async fn sign_in(&self) -> Callback {
        let authorize = self
            .client
            .get(format!("{}/api/v1/auth/sso/idp", self.base))
            .send()
            .await
            .unwrap();
        assert!(authorize.status().is_redirection());
        let cookies = vec![
            cookie_pair(authorize.headers(), "plombir_git_sso_state"),
            cookie_pair(authorize.headers(), "plombir_git_sso_code_verifier"),
        ];
        self.callback(&cookies).await
    }

    /// `POST /auth/sso/idp/link` with `credential`; the cookies it set, when it
    /// succeeded.
    async fn start_link(&self, credential: &str) -> (StatusCode, Vec<String>, String) {
        let response = self
            .client
            .post(format!("{}/api/v1/auth/sso/idp/link", self.base))
            .bearer_auth(credential)
            .send()
            .await
            .unwrap();
        let status = response.status();
        let cookies = [
            "plombir_git_sso_state",
            "plombir_git_sso_code_verifier",
            "plombir_git_sso_link",
        ]
        .iter()
        .filter_map(|name| set_cookie_pair(response.headers(), name))
        .collect();
        (status, cookies, response.text().await.unwrap())
    }

    async fn link_of(&self, sub: &str) -> Option<rg_db::entities::oauth_account::Model> {
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&self.db, "idp", sub)
            .await
            .unwrap()
    }

    async fn links_of_user(&self, user_id: i64) -> usize {
        rg_db::ops::oauth_account_ops::find_by_user_id(&self.db, user_id)
            .await
            .unwrap()
            .len()
    }

    async fn link_journal_entries(&self, user_id: i64) -> usize {
        rg_db::entities::audit_log::Entity::find()
            .filter(rg_db::entities::audit_log::Column::Action.eq("user.link_oauth_account"))
            .filter(rg_db::entities::audit_log::Column::ResourceId.eq(user_id))
            .all(&self.db)
            .await
            .unwrap()
            .len()
    }

    async fn successful_logins(&self, user_id: i64) -> usize {
        rg_db::entities::login_log::Entity::find()
            .filter(rg_db::entities::login_log::Column::UserId.eq(user_id))
            .filter(rg_db::entities::login_log::Column::Success.eq(true))
            .all(&self.db)
            .await
            .unwrap()
            .len()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.app_server.abort();
        self.idp_server.abort();
    }
}

fn victim(email_verified: Option<bool>) -> Profile {
    Profile {
        sub: "victim-subject".to_string(),
        email: "victim@corp.example".to_string(),
        email_verified,
    }
}

/// Everything the pre-hijack would have left behind, asserted absent.
async fn assert_nothing_attached(app: &Harness, response: &Callback, squatter_id: i64) {
    assert!(
        app.link_of("victim-subject").await.is_none(),
        "the victim's identity was linked somewhere: {}",
        response.body
    );
    assert_eq!(
        app.links_of_user(squatter_id).await,
        0,
        "the account holding the address gained a provider link"
    );
    assert_eq!(app.link_journal_entries(squatter_id).await, 0);
    assert_eq!(
        issued_session(&response.headers),
        None,
        "a refused first sign-in still issued a session"
    );
    assert!(
        set_cookie_pair(&response.headers, "plombir_git_mfa_challenge").is_none(),
        "a refused first sign-in still issued an MFA challenge"
    );
    assert_eq!(
        app.successful_logins(squatter_id).await,
        0,
        "the login journal records a sign-in into the account holding the address"
    );
}

/// The defect itself. The address belongs to a local account somebody else
/// registered with a password of their own; the provider has verified that the
/// person signing in owns it. Neither fact makes the local account theirs.
#[tokio::test]
async fn a_first_sso_sign_in_never_enters_a_local_account_holding_its_address() {
    let app = Harness::start(victim(Some(true)), true).await;
    let (_, squatter_id) = register_full(&app.base, "squatter", "victim@corp.example").await;

    let response = app.sign_in().await;

    // The login page turns `link_required` into "sign in to that account and
    // link <provider> under Settings → Security"; the provider travels along
    // so the page can name it.
    response.outcome.assert_refused("/login", "link_required");
    assert!(
        response.outcome.location.ends_with("&provider=idp"),
        "the refusal does not name the provider to link: {}",
        response.outcome.location
    );
    assert_nothing_attached(&app, &response, squatter_id).await;
}

/// A provider that has not vouched for the address — GitLab, an OIDC provider
/// without the claim — must not turn a closed provider into a question about
/// which addresses have accounts here: a taken address and a free one get the
/// same answer, byte for byte.
#[tokio::test]
async fn an_unverified_address_on_a_closed_provider_does_not_say_whether_it_is_taken() {
    let app = Harness::start(victim(None), false).await;
    let (_, squatter_id) = register_full(&app.base, "squatter", "victim@corp.example").await;

    let taken = app.sign_in().await;
    app.sign_in_as(Profile {
        sub: "stranger-subject".to_string(),
        email: "stranger@corp.example".to_string(),
        email_verified: None,
    });
    let free = app.sign_in().await;

    taken
        .outcome
        .assert_refused("/login", "auto_provision_disabled");
    assert_eq!(
        (taken.status, &taken.outcome.location, &taken.body),
        (free.status, &free.outcome.location, &free.body),
        "a closed provider answered a taken address differently from a free one"
    );
    assert_nothing_attached(&app, &taken, squatter_id).await;
}

/// The same unverified address on a provider that does create accounts: it
/// cannot be created (the address is taken) and it is not adopted either.
#[tokio::test]
async fn an_unverified_address_on_an_open_provider_does_not_enter_the_account_either() {
    let app = Harness::start(victim(None), true).await;
    let (_, squatter_id) = register_full(&app.base, "squatter", "victim@corp.example").await;

    let response = app.sign_in().await;

    response.outcome.assert_refused("/login", "link_required");
    assert_nothing_attached(&app, &response, squatter_id).await;
}

/// The way in that replaces the merge: from inside the account, link the
/// provider; afterwards the provider signs in to that account. The provider's
/// address need not match the account's — the session proves the account and
/// the round trip proves the identity, so the address proves nothing here.
#[tokio::test]
async fn a_signed_in_account_links_a_provider_and_then_signs_in_through_it() {
    let app = Harness::start(
        Profile {
            sub: "alice-subject".to_string(),
            email: "alice.work@corp.example".to_string(),
            email_verified: Some(true),
        },
        false,
    )
    .await;
    let (session, alice_id) = register_full(&app.base, "alice", "alice@home.example").await;

    let (status, cookies, body) = app.start_link(&session).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let start: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert!(
        start["authorize_url"]
            .as_str()
            .is_some_and(|url| url.contains("/authorize")),
        "{start}"
    );
    assert_eq!(
        cookies.len(),
        3,
        "state, verifier and link intent: {cookies:?}"
    );

    let linked = app.callback(&cookies).await;
    assert_eq!(
        linked.status,
        StatusCode::TEMPORARY_REDIRECT,
        "body: {}",
        linked.body
    );
    assert_eq!(
        linked.headers[header::LOCATION],
        "/settings/security?sso_linked=idp"
    );
    assert_eq!(
        issued_session(&linked.headers),
        None,
        "completing a link must not mint a session; the browser already has one"
    );
    let link = app
        .link_of("alice-subject")
        .await
        .expect("the identity is linked");
    assert_eq!(link.user_id, alice_id);
    assert_eq!(app.link_journal_entries(alice_id).await, 1);

    // The closed provider creates nobody, and still signs in a linked member.
    let signed_in = app.sign_in().await;
    assert_eq!(
        signed_in.status,
        StatusCode::TEMPORARY_REDIRECT,
        "body: {}",
        signed_in.body
    );
    let token = issued_session(&signed_in.headers).expect("the sign-in issued a session");
    let claims = rg_core::auth::jwt::validate_token(&token, TEST_JWT_SECRET)
        .expect("the session is one this server minted");
    assert_eq!(claims.sub, alice_id.to_string());
    assert_eq!(
        app.link_journal_entries(alice_id).await,
        1,
        "signing in through a link is not a second link"
    );
}

/// A linked identity is a way to sign in, so a token scoped to less than the
/// account must not be able to create one.
#[tokio::test]
async fn linking_a_provider_needs_a_login_session_not_a_token() {
    let app = Harness::start(victim(Some(true)), true).await;
    let (session, _) = register_full(&app.base, "alice", "alice@home.example").await;
    let pat = app
        .client
        .post(format!("{}/api/v1/users/tokens", app.base))
        .bearer_auth(&session)
        // Scoped to the account itself, so the scope layer lets it reach a
        // `User` route at all: what refuses it has to be the session rule.
        .json(&serde_json::json!({ "name": "ci", "scopes": "user" }))
        .send()
        .await
        .unwrap();
    assert_eq!(pat.status(), StatusCode::CREATED);
    let pat = pat.json::<serde_json::Value>().await.unwrap()["token"]
        .as_str()
        .unwrap()
        .to_string();

    let (status, cookies, body) = app.start_link(&pat).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert!(
        body.contains("requires a login session"),
        "refused for some other reason than being a token: {body}"
    );
    assert!(
        cookies.is_empty(),
        "a refused link still set cookies: {cookies:?}"
    );
}

/// A link request is the session's, and ends with it.
#[tokio::test]
async fn a_link_request_does_not_outlive_the_session_that_made_it() {
    let app = Harness::start(victim(Some(true)), false).await;
    let (session, alice_id) = register_full(&app.base, "alice", "alice@home.example").await;
    let (status, cookies, body) = app.start_link(&session).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");

    rg_db::ops::user_ops::invalidate_sessions(&app.db, alice_id)
        .await
        .unwrap();
    let response = app.callback(&cookies).await;

    response
        .outcome
        .assert_refused("/settings/security", "session_ended");
    assert!(app.link_of("victim-subject").await.is_none());
    assert_eq!(app.links_of_user(alice_id).await, 0);
    assert_eq!(app.link_journal_entries(alice_id).await, 0);
}

/// An intent rewritten to name another account is refused — and is not quietly
/// treated as an ordinary sign-in either.
#[tokio::test]
async fn a_forged_link_request_is_refused_rather_than_signed_in() {
    let app = Harness::start(victim(Some(true)), true).await;
    let (session, alice_id) = register_full(&app.base, "alice", "alice@home.example").await;
    let (_, bob_id) = register_full(&app.base, "bob", "bob@home.example").await;
    let (_, cookies, _) = app.start_link(&session).await;

    let forged: Vec<String> = cookies
        .into_iter()
        .map(
            |cookie| match cookie.strip_prefix("plombir_git_sso_link=") {
                Some(value) => {
                    let (_, rest) = value.split_once('.').unwrap();
                    format!("plombir_git_sso_link={bob_id}.{rest}")
                }
                None => cookie,
            },
        )
        .collect();
    let response = app.callback(&forged).await;

    response
        .outcome
        .assert_refused("/settings/security", "state_invalid");
    assert_eq!(issued_session(&response.headers), None);
    assert!(app.link_of("victim-subject").await.is_none());
    assert_eq!(app.links_of_user(alice_id).await, 0);
    assert_eq!(app.links_of_user(bob_id).await, 0);
}

/// An identity already linked to one account cannot be moved to another by
/// linking it there.
#[tokio::test]
async fn an_identity_linked_to_another_account_stays_where_it_is() {
    let app = Harness::start(victim(Some(true)), false).await;
    let (_, bob_id) = register_full(&app.base, "bob", "bob@home.example").await;
    rg_db::ops::oauth_account_ops::link(
        &app.db,
        bob_id,
        "idp",
        "victim-subject",
        "provider-person",
        "victim@corp.example",
    )
    .await
    .unwrap()
    .expect("bob holds the identity");
    let (session, alice_id) = register_full(&app.base, "alice", "alice@home.example").await;

    let (status, cookies, body) = app.start_link(&session).await;
    assert_eq!(status, StatusCode::OK, "body: {body}");
    let response = app.callback(&cookies).await;

    response
        .outcome
        .assert_refused("/settings/security", "linked_elsewhere");
    assert_eq!(
        app.link_of("victim-subject").await.unwrap().user_id,
        bob_id,
        "the identity moved"
    );
    assert_eq!(app.links_of_user(alice_id).await, 0);
    assert_eq!(app.link_journal_entries(alice_id).await, 0);
}

/// The merge was also the way back into an SSO-only account whose last link
/// had been dropped. With it gone, the last way in cannot be removed — neither
/// the provider link nor, once that has been replaced, the passkey.
#[tokio::test]
async fn the_last_way_into_an_sso_only_account_cannot_be_removed() {
    let app = Harness::start(
        Profile {
            sub: "solo-subject".to_string(),
            email: "solo@corp.example".to_string(),
            email_verified: Some(true),
        },
        true,
    )
    .await;
    let first = app.sign_in().await;
    assert_eq!(
        first.status,
        StatusCode::TEMPORARY_REDIRECT,
        "body: {}",
        first.body
    );
    let session = issued_session(&first.headers).expect("the first sign-in issued a session");
    let user_id = app.link_of("solo-subject").await.unwrap().user_id;

    let unlink = || async {
        app.client
            .delete(format!("{}/api/v1/auth/sso/idp/unlink", app.base))
            .bearer_auth(&session)
            .send()
            .await
            .unwrap()
            .status()
    };

    assert_eq!(unlink().await, StatusCode::CONFLICT);
    assert!(
        app.link_of("solo-subject").await.is_some(),
        "a refused unlink removed the only way in"
    );

    let passkey = rg_db::ops::passkey_credential_ops::create(
        &app.db,
        user_id,
        "solo-credential",
        "{}",
        "Laptop",
        "localhost",
    )
    .await
    .unwrap();
    assert_eq!(
        unlink().await,
        StatusCode::OK,
        "with a passkey the link is no longer the only way in"
    );

    let delete_passkey = app
        .client
        .delete(format!("{}/api/v1/users/passkeys/{}", app.base, passkey.id))
        .bearer_auth(&session)
        .send()
        .await
        .unwrap()
        .status();
    assert_eq!(delete_passkey, StatusCode::CONFLICT);
    assert_eq!(
        rg_db::ops::passkey_credential_ops::list_by_user(&app.db, user_id)
            .await
            .unwrap()
            .len(),
        1,
        "a refused delete removed the only way in"
    );
}

/// One identity per provider per account: the links are addressed by provider
/// slug everywhere (unlink, the settings page), so a second one would make
/// either of them reachable only at random.
#[tokio::test]
async fn an_account_cannot_hold_two_identities_from_one_provider() {
    let app = Harness::start(victim(Some(true)), false).await;
    let (session, alice_id) = register_full(&app.base, "alice", "alice@home.example").await;
    rg_db::ops::oauth_account_ops::link(
        &app.db,
        alice_id,
        "idp",
        "first-subject",
        "alice",
        "alice@corp.example",
    )
    .await
    .unwrap()
    .expect("alice holds one identity");

    let (status, cookies, body) = app.start_link(&session).await;

    assert_eq!(status, StatusCode::CONFLICT, "body: {body}");
    assert!(cookies.is_empty(), "a refused link still set cookies");
    assert!(app.link_of("victim-subject").await.is_none());
    assert_eq!(app.links_of_user(alice_id).await, 1);
}
