//! card_1a7da084c197: whose fault is it when the identity provider does not
//! answer.
//!
//! `GET /auth/sso/{slug}/callback` used to answer `400` to every failure of the
//! provider leg — a DNS failure, a connect timeout, a GitHub `500`. `400` means
//! "fix your request", and there is nothing in the request to fix: the person
//! signing in can only try again, and every retry layer between them and us
//! reads a `4xx` as "do not bother". The two cases have to be different
//! answers.
//!
//! These drive the real `authorize → callback` round trip against a mock OIDC
//! provider whose failure mode each test picks, because the classification has
//! to hold on the path a browser actually walks. Every outage test is paired
//! with the `400`s that must survive it — a provider that refuses the grant, a
//! profile that identifies nobody — so a green `502` proves the split and not a
//! handler that answers `502` to everything.

use std::collections::HashMap;

use crate::common::{build_test_app_state, setup_test_db};
use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use rg_db::ops::sso_provider_ops::SsoProviderInput;

/// What the mock provider does when ForgeKeep calls it.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Behaviour {
    /// Everything answers; the login completes.
    Healthy,
    /// The token endpoint is down.
    TokenServerError,
    /// The token endpoint is up and refuses the grant — an expired or already
    /// redeemed `code`, the one failure here the caller can act on.
    TokenRefusesGrant,
    /// The userinfo endpoint is down.
    UserinfoServerError,
    /// Nothing is listening on the userinfo endpoint at all.
    UserinfoUnreachable,
    /// The provider answers, and its answer identifies nobody.
    UserinfoWithoutEmail,
}

#[derive(Clone)]
struct MockIdp {
    base_url: String,
    /// Where the discovery document points userinfo. Its own address normally;
    /// a closed port for [`Behaviour::UserinfoUnreachable`].
    userinfo_url: String,
    behaviour: Behaviour,
}

async fn discovery(State(idp): State<MockIdp>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": idp.base_url,
        "authorization_endpoint": format!("{}/authorize", idp.base_url),
        "token_endpoint": format!("{}/token", idp.base_url),
        "userinfo_endpoint": idp.userinfo_url,
    }))
}

async fn token(
    State(idp): State<MockIdp>,
    Form(_form): Form<HashMap<String, String>>,
) -> axum::response::Response {
    use axum::response::IntoResponse;
    match idp.behaviour {
        Behaviour::TokenServerError => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "server_error"})),
        )
            .into_response(),
        Behaviour::TokenRefusesGrant => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_grant"})),
        )
            .into_response(),
        _ => Json(serde_json::json!({
            "access_token": "mock-access-token",
            "expires_in": 3600
        }))
        .into_response(),
    }
}

async fn userinfo(State(idp): State<MockIdp>) -> axum::response::Response {
    use axum::response::IntoResponse;
    match idp.behaviour {
        Behaviour::UserinfoServerError => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": "server_error"})),
        )
            .into_response(),
        // A profile with no address at all: `SsoIdentityDefect::MissingEmail`,
        // and that one really is the caller's to fix.
        Behaviour::UserinfoWithoutEmail => Json(serde_json::json!({
            "sub": "subject-1",
            "preferred_username": "newcomer",
            "name": "Newcomer"
        }))
        .into_response(),
        _ => Json(serde_json::json!({
            "sub": "subject-1",
            "preferred_username": "newcomer",
            "email": "newcomer@example.com",
            "email_verified": true,
            "name": "Newcomer"
        }))
        .into_response(),
    }
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
    base: String,
    client: reqwest::Client,
    app_server: tokio::task::JoinHandle<()>,
    idp_server: tokio::task::JoinHandle<()>,
    outage_server: Option<tokio::task::JoinHandle<()>>,
    _app_dir: tempfile::TempDir,
}

impl Harness {
    async fn start(behaviour: Behaviour) -> Harness {
        let idp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp_addr = idp_listener.local_addr().unwrap().to_string();
        let idp_base = format!("http://{idp_addr}");

        // Keep ownership of the outage port and sever every connection before
        // an HTTP response exists. The client still observes a status-less
        // transport failure, but no parallel test can claim the address.
        let (userinfo_url, outage_server) = if behaviour == Behaviour::UserinfoUnreachable {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let server = tokio::spawn(async move {
                while let Ok((stream, _)) = listener.accept().await {
                    drop(stream);
                }
            });
            (format!("http://{addr}/userinfo"), Some(server))
        } else {
            (format!("{idp_base}/userinfo"), None)
        };

        let idp = MockIdp {
            base_url: idp_base.clone(),
            userinfo_url,
            behaviour,
        };
        let idp_app = Router::new()
            .route("/.well-known/openid-configuration", get(discovery))
            .route("/userinfo", get(userinfo))
            .route("/token", post(token))
            .with_state(idp);
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
                auto_provision: true,
                ..Default::default()
            },
        )
        .await
        .unwrap();

        let app = rg_http::create_router_for_test(build_test_app_state(db, repo_root));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap().to_string();
        let base = format!("http://{addr}");
        let app_server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        crate::common::wait_for_listener(&addr).await;

        Harness {
            base,
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            app_server,
            idp_server,
            outage_server,
            _app_dir: app_dir,
        }
    }

    /// One full login attempt, exactly as a browser drives it.
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
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.app_server.abort();
        self.idp_server.abort();
        if let Some(server) = &self.outage_server {
            server.abort();
        }
    }
}

/// The baseline that makes every refusal below mean something: the identical
/// login completes when the provider is healthy.
#[tokio::test]
async fn a_healthy_provider_still_signs_in() {
    let (status, body) = Harness::start(Behaviour::Healthy).await.sign_in().await;

    assert_eq!(
        status,
        StatusCode::TEMPORARY_REDIRECT,
        "the healthy round trip must still complete, got body: {body}"
    );
}

/// The defect: the provider's own `500` was signed as the client's bad request.
#[tokio::test]
async fn a_userinfo_endpoint_that_answers_500_is_not_the_clients_fault() {
    let (status, body) = Harness::start(Behaviour::UserinfoServerError)
        .await
        .sign_in()
        .await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a provider that failed to answer must not be reported as a bad request, got body: {body}"
    );
    assert!(
        !body.contains("failed to fetch user info"),
        "the operator's reason must stay in the log, not in the client's body: {body}"
    );
}

/// Same failure one layer lower: the peer severs the transport before sending
/// an HTTP status — the case a status-only classification would miss.
#[tokio::test]
async fn a_userinfo_endpoint_that_refuses_the_connection_is_not_the_clients_fault() {
    let (status, body) = Harness::start(Behaviour::UserinfoUnreachable)
        .await
        .sign_in()
        .await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "an unreachable provider must not be reported as a bad request, got body: {body}"
    );
}

/// The token exchange half of the same split.
#[tokio::test]
async fn a_token_endpoint_that_answers_500_is_not_the_clients_fault() {
    let (status, body) = Harness::start(Behaviour::TokenServerError)
        .await
        .sign_in()
        .await;

    assert_eq!(
        status,
        StatusCode::BAD_GATEWAY,
        "a failed token exchange against a broken provider is not a bad request, got body: {body}"
    );
}

/// The `400` that has to survive the change: a provider that *answered* and
/// refused the grant is an expired or already-redeemed code, and starting the
/// sign-in again is exactly the right remedy.
#[tokio::test]
async fn a_provider_that_refuses_the_grant_is_still_a_400() {
    let (status, body) = Harness::start(Behaviour::TokenRefusesGrant)
        .await
        .sign_in()
        .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a refused authorization grant is the one thing here the caller can fix, got body: {body}"
    );
    assert!(
        body.contains("start the sign-in again"),
        "the refusal has to say what to do about it, got: {body}"
    );
}

/// The other `400` that has to survive: the provider answered fine, and its
/// answer identifies nobody. Same handler, same call, opposite classification.
#[tokio::test]
async fn a_profile_without_an_email_is_still_a_400() {
    let (status, body) = Harness::start(Behaviour::UserinfoWithoutEmail)
        .await
        .sign_in()
        .await;

    assert_eq!(
        status,
        StatusCode::BAD_REQUEST,
        "a profile that identifies nobody is the caller's to fix, got body: {body}"
    );
    assert!(
        body.contains("no email address"),
        "the refusal has to name the defect, got: {body}"
    );
}
