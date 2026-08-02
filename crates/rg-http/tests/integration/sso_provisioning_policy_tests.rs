//! card_0ae3deacd3f0: who an SSO provider may create an account *for*.
//!
//! A configured public IdP used to mean open registration for the whole
//! internet: the callback found no OAuth link and no account on the asserted
//! email, and provisioned unconditionally. `[auth].registration = "closed"`
//! did not touch that path on purpose, so an operator who closed the front
//! door still had the side one wide open.
//!
//! These drive the real `authorize → callback` round trip against a mock OIDC
//! provider, because the gate has to hold on the path a browser actually
//! walks — not on a helper called directly. Each refusal is paired with the
//! same login under a permissive policy, so a green refusal proves the policy
//! and not a broken fixture.

use std::collections::HashMap;

use crate::common::{build_test_app_state, setup_test_db};
use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use rg_db::ops::sso_provider_ops::SsoProviderInput;

/// The address the mock IdP asserts for its one subject.
#[derive(Clone)]
struct MockIdp {
    base_url: String,
    email: String,
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
    Json(serde_json::json!({
        "sub": "subject-1",
        "preferred_username": "newcomer",
        "email": idp.email,
        "email_verified": true,
        "name": "Newcomer"
    }))
}

fn cookie_pair(headers: &HeaderMap, name: &str) -> String {
    let prefix = format!("{name}=");
    headers
        .get_all(header::SET_COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .find(|value| value.starts_with(&prefix))
        .unwrap_or_else(|| panic!("missing {name} cookie"))
        .split(';')
        .next()
        .unwrap()
        .to_string()
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

struct Harness {
    db: sea_orm::DatabaseConnection,
    base: String,
    client: reqwest::Client,
    app_server: tokio::task::JoinHandle<()>,
    idp_server: tokio::task::JoinHandle<()>,
    _app_dir: tempfile::TempDir,
}

impl Harness {
    /// One instance, one OIDC provider, and whatever provisioning policy the
    /// test is about.
    async fn start(
        email: &str,
        auto_provision: bool,
        allowed_email_domains: Option<&str>,
    ) -> Harness {
        let idp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp_addr = idp_listener.local_addr().unwrap().to_string();
        let idp_base = format!("http://{idp_addr}");
        let idp = MockIdp {
            base_url: idp_base.clone(),
            email: email.to_string(),
        };
        let idp_app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/userinfo", get(userinfo))
            .with_state(idp.clone())
            .merge(Router::new().route("/token", post(token)));
        let idp_server = tokio::spawn(async move {
            axum::serve(idp_listener, idp_app).await.unwrap();
        });
        crate::common::wait_for_listener(&idp_addr).await;

        let (db, app_dir) = setup_test_db().await;
        let repo_root = app_dir.path().join("repos");
        std::fs::create_dir_all(&repo_root).unwrap();
        let discovery_url = format!("{idp_base}/.well-known/openid-configuration");
        rg_db::ops::sso_provider_ops::upsert(
            &db,
            None,
            SsoProviderInput {
                name: "Mock IdP",
                slug: "idp",
                provider_type: "oidc",
                client_id: Some("client-id"),
                discovery_url: Some(&discovery_url),
                scopes: Some("openid profile email"),
                enabled: true,
                auto_provision,
                allowed_email_domains,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let app = rg_http::create_router_for_test(build_test_app_state(db.clone(), repo_root));
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
            app_server,
            idp_server,
            _app_dir: app_dir,
        }
    }

    /// One full first login, exactly as a browser drives it.
    async fn sign_in(&self) -> (StatusCode, String) {
        let authorize = self
            .client
            .get(format!("{}/api/v1/auth/sso/idp", self.base))
            .send()
            .await
            .unwrap();
        assert!(authorize.status().is_redirection());
        let state_cookie = cookie_pair(authorize.headers(), "forgekeep_sso_state");
        let verifier_cookie = cookie_pair(authorize.headers(), "forgekeep_sso_code_verifier");
        let state = signed_cookie_value(&state_cookie);
        let callback = self
            .client
            .get(format!(
                "{}/api/v1/auth/sso/idp/callback?code=valid-code&state={state}",
                self.base
            ))
            .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
            .send()
            .await
            .unwrap();
        (callback.status(), callback.text().await.unwrap())
    }

    async fn provisioned(&self, email: &str) -> bool {
        rg_db::ops::user_ops::find_by_email(&self.db, email)
            .await
            .unwrap()
            .is_some()
    }

    async fn linked(&self) -> bool {
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&self.db, "idp", "subject-1")
            .await
            .unwrap()
            .is_some()
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.app_server.abort();
        self.idp_server.abort();
    }
}

/// The defect itself: a valid identity nobody here knows must not become an
/// account when the provider is not allowed to create them — and the refusal
/// is a decision (403), not a crash (500).
#[tokio::test]
async fn a_provider_without_auto_provision_creates_no_account() {
    let app = Harness::start("newcomer@example.com", false, None).await;

    let (status, body) = app.sign_in().await;

    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a refused provisioning must answer 403, got body: {body}"
    );
    assert!(
        body.contains("does not create new accounts"),
        "the refusal has to say which rule refused, got: {body}"
    );
    assert!(
        !app.provisioned("newcomer@example.com").await,
        "a refused SSO login provisioned an account anyway"
    );
    assert!(
        !app.linked().await,
        "a refused SSO login left an OAuth link behind"
    );
}

/// The baseline that makes the refusal above mean something: the identical
/// login, refused only by the policy, succeeds when the policy allows it.
#[tokio::test]
async fn the_same_login_is_provisioned_when_the_provider_may() {
    let app = Harness::start("newcomer@example.com", true, None).await;

    let (status, _) = app.sign_in().await;

    assert_eq!(
        status,
        StatusCode::TEMPORARY_REDIRECT,
        "an allowed first login must complete"
    );
    assert!(
        app.provisioned("newcomer@example.com").await,
        "an allowed first login provisioned nothing"
    );
    assert!(app.linked().await, "an allowed first login left no link");
}

/// The narrower rule: the provider provisions, but not for this domain.
#[tokio::test]
async fn an_address_outside_the_allowlist_creates_no_account() {
    let app = Harness::start("newcomer@outsider.com", true, Some("example.com")).await;

    let (status, body) = app.sign_in().await;

    assert_eq!(status, StatusCode::FORBIDDEN, "body: {body}");
    assert!(
        body.contains("email domain"),
        "the refusal has to name the domain rule, got: {body}"
    );
    assert!(!app.provisioned("newcomer@outsider.com").await);
    assert!(!app.linked().await);
}

/// Its baseline, on the same allowlist: an address inside it goes through.
#[tokio::test]
async fn an_address_inside_the_allowlist_is_provisioned() {
    let app = Harness::start("newcomer@example.com", true, Some("example.com")).await;

    let (status, _) = app.sign_in().await;

    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert!(app.provisioned("newcomer@example.com").await);
}

/// Turning provisioning off keeps strangers out; it must not lock out the
/// people already on the instance. The account exists on the asserted address
/// before the login, so the callback takes the merge branch — which is not a
/// provisioning and is not the policy's business.
#[tokio::test]
async fn an_existing_account_still_signs_in_through_a_closed_provider() {
    let app = Harness::start("member@example.com", false, Some("nowhere.invalid")).await;
    rg_db::ops::user_ops::create_user(&app.db, "member", "member@example.com", "", "Member")
        .await
        .unwrap();

    let (status, body) = app.sign_in().await;

    assert_eq!(
        status,
        StatusCode::TEMPORARY_REDIRECT,
        "a provisioning switch locked out an account that already existed, body: {body}"
    );
    assert!(
        app.linked().await,
        "the existing account did not get its provider link"
    );
}
