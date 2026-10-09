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

use crate::common::{build_test_app_state_with, setup_test_db, SsoCallbackOutcome, StateOverrides};
use axum::extract::{Form, State};
use axum::http::{header, HeaderMap};
use axum::routing::{get, post};
use axum::{Json, Router};
use rg_db::ops::sso_provider_ops::SsoProviderInput;

/// The address the mock IdP asserts for its one subject.
#[derive(Clone)]
struct MockIdp {
    base_url: String,
    email: String,
    /// `None` leaves the claim out of the profile, as GitLab and many OIDC
    /// providers do.
    email_verified: Option<bool>,
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
    let mut profile = serde_json::json!({
        "sub": "subject-1",
        "preferred_username": "newcomer",
        "email": idp.email,
        "name": "Newcomer"
    });
    if let Some(verified) = idp.email_verified {
        profile["email_verified"] = serde_json::Value::Bool(verified);
    }
    Json(profile)
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
        Self::start_with_claim(email, auto_provision, allowed_email_domains, Some(true)).await
    }

    /// [`Harness::start`] with the provider's `email_verified` claim chosen.
    async fn start_with_claim(
        email: &str,
        auto_provision: bool,
        allowed_email_domains: Option<&str>,
        email_verified: Option<bool>,
    ) -> Harness {
        let idp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp_addr = idp_listener.local_addr().unwrap().to_string();
        let idp_base = format!("http://{idp_addr}");
        let idp = MockIdp {
            base_url: idp_base.clone(),
            email: email.to_string(),
            email_verified,
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
        rg_db::ops::sso_provider_ops::create(
            &db,
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
            app_server,
            idp_server,
            _app_dir: app_dir,
        }
    }

    /// One full first login, exactly as a browser drives it.
    async fn sign_in(&self) -> SsoCallbackOutcome {
        let authorize = self
            .client
            .get(format!("{}/api/v1/auth/sso/idp", self.base))
            .send()
            .await
            .unwrap();
        assert!(authorize.status().is_redirection());
        let state_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_state");
        let verifier_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_code_verifier");
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
        SsoCallbackOutcome::of(&callback)
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
/// names the rule that refused, not a crash.
#[tokio::test]
async fn a_provider_without_auto_provision_creates_no_account() {
    let app = Harness::start("newcomer@example.com", false, None).await;

    app.sign_in()
        .await
        .assert_refused("/login", "auto_provision_disabled");
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

    let outcome = app.sign_in().await;

    assert_eq!(
        outcome.refusal(),
        None,
        "an allowed first login must complete, went to: {}",
        outcome.location
    );
    assert!(
        outcome.session_issued,
        "an allowed first login issued no session"
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

    app.sign_in()
        .await
        .assert_refused("/login", "email_domain_not_allowed");
    assert!(!app.provisioned("newcomer@outsider.com").await);
    assert!(!app.linked().await);
}

/// Its baseline, on the same allowlist: an address inside it goes through.
#[tokio::test]
async fn an_address_inside_the_allowlist_is_provisioned() {
    let app = Harness::start("newcomer@example.com", true, Some("example.com")).await;

    assert_eq!(app.sign_in().await.refusal(), None);
    assert!(app.provisioned("newcomer@example.com").await);
}

/// card_7099e8a305bc: an allowlist says "accounts for our staff", and the
/// domain is evidence of that only when the provider vouched for the address.
/// A profile that does not say — GitLab, OIDC without the claim — or says
/// "unconfirmed" must not buy a stranger an account, nor the address with it.
#[tokio::test]
async fn an_allowlist_refuses_an_address_the_provider_did_not_confirm() {
    for claim in [None, Some(false)] {
        let app =
            Harness::start_with_claim("someone@example.com", true, Some("example.com"), claim)
                .await;

        // An explicit `false` is already refused one layer earlier, by the
        // identity gate, with a reason of its own; `None` reaches the
        // allowlist.
        let expected = if claim.is_none() {
            "email_not_verified"
        } else {
            "profile_unverified_email"
        };
        app.sign_in().await.assert_refused("/login", expected);
        assert!(
            !app.provisioned("someone@example.com").await,
            "claim {claim:?}: an unconfirmed address got an account through the allowlist"
        );
        assert!(!app.linked().await);
    }
}

/// Its baseline: the identical profile, now confirmed, is provisioned — and
/// with no allowlist the claim decides nothing.
#[tokio::test]
async fn a_confirmed_address_or_an_unrestricted_provider_still_provisions() {
    let app =
        Harness::start_with_claim("someone@example.com", true, Some("example.com"), Some(true))
            .await;
    let outcome = app.sign_in().await;
    assert_eq!(outcome.refusal(), None, "went to: {}", outcome.location);
    assert!(app.provisioned("someone@example.com").await);

    let app = Harness::start_with_claim("someone@example.com", true, None, None).await;
    let outcome = app.sign_in().await;
    assert_eq!(outcome.refusal(), None, "went to: {}", outcome.location);
    assert!(app.provisioned("someone@example.com").await);
}

/// Turning provisioning off keeps strangers out; it must not lock out the
/// people already on the instance. The identity is linked to an existing
/// account before the login, so the callback signs in through the link — which
/// is not a provisioning and is not the policy's business.
///
/// "Already on the instance" means *linked*. It used to mean "holds the email
/// the provider asserts", and that was the pre-hijack of card_4753cfe7b985:
/// see `sso_account_link_tests.rs` for what an unlinked account holding the
/// address gets now.
#[tokio::test]
async fn an_existing_account_still_signs_in_through_a_closed_provider() {
    let app = Harness::start("member@example.com", false, Some("nowhere.invalid")).await;
    let member =
        rg_db::ops::user_ops::create_user(&app.db, "member", "member@example.com", "", "Member")
            .await
            .unwrap();
    rg_db::ops::oauth_account_ops::link(
        &app.db,
        member.id,
        "idp",
        "subject-1",
        "member",
        "member@example.com",
    )
    .await
    .unwrap()
    .expect("the identity is linked");

    let outcome = app.sign_in().await;

    assert_eq!(
        outcome.refusal(),
        None,
        "a provisioning switch locked out an account that already existed, went to: {}",
        outcome.location
    );
    assert!(outcome.session_issued, "the existing member got no session");
    assert!(
        app.linked().await,
        "the existing account lost its provider link"
    );
}

/// A refused provisioning names the way in for somebody who does have an
/// account here — said to everyone, so it discloses nothing about whether this
/// person's address is taken. The sentence lives on the login page, keyed by
/// the refusal code (`web/src/lib/api/loginSsoError.test.ts`); what the server
/// owes it is the provider, so the page can name it.
#[tokio::test]
async fn a_refused_provisioning_points_an_existing_member_at_linking() {
    let app = Harness::start("newcomer@example.com", false, None).await;

    let outcome = app.sign_in().await;

    outcome.assert_refused("/login", "auto_provision_disabled");
    assert!(
        outcome.location.ends_with("&provider=idp"),
        "the refusal does not name the provider to link: {}",
        outcome.location
    );
}
