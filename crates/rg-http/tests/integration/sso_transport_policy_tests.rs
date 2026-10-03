//! Custom OIDC may use plaintext HTTP only through an exact operator-owned
//! origin exception. Discovery cannot use its own authority to authorize the
//! client-secret or Bearer-token sinks it names.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};

use crate::common::{
    build_test_app_state_with, setup_test_db, wait_for_listener, StateOverrides,
    TEST_ENCRYPTION_KEY,
};

#[derive(Clone)]
struct DiscoveryState {
    token_url: String,
    userinfo_url: String,
    hits: Arc<AtomicUsize>,
}

async fn discovery(State(state): State<DiscoveryState>) -> Json<serde_json::Value> {
    state.hits.fetch_add(1, Ordering::SeqCst);
    Json(serde_json::json!({
        "issuer": "https://issuer.example",
        "authorization_endpoint": "https://issuer.example/authorize",
        "token_endpoint": state.token_url,
        "userinfo_endpoint": state.userinfo_url,
    }))
}

#[derive(Clone)]
struct TokenState {
    hits: Arc<AtomicUsize>,
    client_secret: Arc<Mutex<Option<String>>>,
}

async fn token(
    State(state): State<TokenState>,
    Form(form): Form<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    state.hits.fetch_add(1, Ordering::SeqCst);
    *state.client_secret.lock().unwrap() = form.get("client_secret").cloned();
    Json(serde_json::json!({"access_token": "sink-access-token"}))
}

#[derive(Clone)]
struct UserinfoState {
    hits: Arc<AtomicUsize>,
    authorization: Arc<Mutex<Option<String>>>,
}

async fn userinfo(
    State(state): State<UserinfoState>,
    headers: HeaderMap,
) -> Json<serde_json::Value> {
    state.hits.fetch_add(1, Ordering::SeqCst);
    *state.authorization.lock().unwrap() = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    Json(serde_json::json!({
        "sub": "transport-subject",
        "preferred_username": "transport-user",
        "email": "transport-user@example.test",
        "email_verified": true,
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

struct TestApp {
    base: String,
    server: tokio::task::JoinHandle<()>,
    _dir: tempfile::TempDir,
}

impl Drop for TestApp {
    fn drop(&mut self) {
        self.server.abort();
    }
}

async fn spawn_app(discovery_url: &str, allowed_origins: &[String]) -> TestApp {
    let (db, dir) = setup_test_db().await;
    let repo_root = dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    let key = rg_core::auth::encryption::derive_key(TEST_ENCRYPTION_KEY);
    let encrypted_secret = rg_core::auth::encryption::encrypt("sink-client-secret", &key)
        .expect("encrypt the test client secret");
    rg_db::ops::sso_provider_ops::create(
        &db,
        rg_db::ops::sso_provider_ops::SsoProviderInput {
            name: "Transport IdP",
            slug: "transport-idp",
            provider_type: "oidc",
            client_id: Some("transport-client"),
            client_secret_enc: Some(&encrypted_secret),
            discovery_url: Some(discovery_url),
            scopes: Some("openid profile email"),
            enabled: true,
            auto_provision: true,
            ..Default::default()
        },
    )
    .await
    .expect("seed transport IdP");

    let policy = rg_core::auth::sso::OidcTransportPolicy::parse(allowed_origins)
        .expect("test origins are exact");
    let app = rg_http::create_router_for_test(build_test_app_state_with(
        db,
        repo_root,
        StateOverrides {
            oidc_transport_policy: Some(policy),
            ..Default::default()
        },
    ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    wait_for_listener(&addr).await;
    TestApp {
        base,
        server,
        _dir: dir,
    }
}

#[tokio::test]
async fn oidc_plaintext_sinks_need_their_own_exact_origin_opt_ins() {
    let discovery_hits = Arc::new(AtomicUsize::new(0));
    let token_hits = Arc::new(AtomicUsize::new(0));
    let userinfo_hits = Arc::new(AtomicUsize::new(0));
    let seen_secret = Arc::new(Mutex::new(None));
    let seen_authorization = Arc::new(Mutex::new(None));

    let token_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let token_addr = token_listener.local_addr().unwrap().to_string();
    let token_origin = format!("http://{token_addr}");
    let token_state = TokenState {
        hits: token_hits.clone(),
        client_secret: seen_secret.clone(),
    };
    let token_server = tokio::spawn(async move {
        axum::serve(
            token_listener,
            Router::new()
                .route("/token", post(token))
                .with_state(token_state),
        )
        .await
        .unwrap();
    });
    wait_for_listener(&token_addr).await;

    let userinfo_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let userinfo_addr = userinfo_listener.local_addr().unwrap().to_string();
    let userinfo_origin = format!("http://{userinfo_addr}");
    let userinfo_state = UserinfoState {
        hits: userinfo_hits.clone(),
        authorization: seen_authorization.clone(),
    };
    let userinfo_server = tokio::spawn(async move {
        axum::serve(
            userinfo_listener,
            Router::new()
                .route("/userinfo", get(userinfo))
                .with_state(userinfo_state),
        )
        .await
        .unwrap();
    });
    wait_for_listener(&userinfo_addr).await;

    let discovery_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let discovery_addr = discovery_listener.local_addr().unwrap().to_string();
    let discovery_origin = format!("http://{discovery_addr}");
    let discovery_url = format!("{discovery_origin}/.well-known/openid-configuration");
    let discovery_state = DiscoveryState {
        token_url: format!("{token_origin}/token"),
        userinfo_url: format!("{userinfo_origin}/userinfo"),
        hits: discovery_hits.clone(),
    };
    let discovery_server = tokio::spawn(async move {
        axum::serve(
            discovery_listener,
            Router::new()
                .route("/.well-known/openid-configuration", get(discovery))
                .with_state(discovery_state),
        )
        .await
        .unwrap();
    });
    wait_for_listener(&discovery_addr).await;

    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // Secure default: reject before even the plaintext discovery connection.
    let secure_app = spawn_app(&discovery_url, &[]).await;
    let response = client
        .get(format!("{}/api/v1/auth/sso/transport-idp", secure_app.base))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_server_error());
    assert_eq!(discovery_hits.load(Ordering::SeqCst), 0);
    assert_eq!(token_hits.load(Ordering::SeqCst), 0);
    assert_eq!(userinfo_hits.load(Ordering::SeqCst), 0);

    // Discovery trust is not authority to widen plaintext transport to the
    // token or userinfo origins named by its JSON.
    let discovery_only = spawn_app(&discovery_url, std::slice::from_ref(&discovery_origin)).await;
    let response = client
        .get(format!(
            "{}/api/v1/auth/sso/transport-idp",
            discovery_only.base
        ))
        .send()
        .await
        .unwrap();
    assert!(response.status().is_server_error());
    assert_eq!(discovery_hits.load(Ordering::SeqCst), 1);
    assert_eq!(token_hits.load(Ordering::SeqCst), 0);
    assert_eq!(userinfo_hits.load(Ordering::SeqCst), 0);
    assert!(seen_secret.lock().unwrap().is_none());
    assert!(seen_authorization.lock().unwrap().is_none());

    // Only when all three exact origins are named does the real browser flow
    // deliver the client secret and Bearer token to the intended sinks.
    let allowed = spawn_app(
        &discovery_url,
        &[
            discovery_origin.clone(),
            token_origin.clone(),
            userinfo_origin.clone(),
        ],
    )
    .await;
    let authorize = client
        .get(format!("{}/api/v1/auth/sso/transport-idp", allowed.base))
        .send()
        .await
        .unwrap();
    assert!(authorize.status().is_redirection());
    let state_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_state");
    let verifier_cookie = cookie_pair(authorize.headers(), "plombir_git_sso_code_verifier");
    let state = signed_cookie_value(&state_cookie);
    let callback = client
        .get(format!(
            "{}/api/v1/auth/sso/transport-idp/callback?code=valid-code&state={state}",
            allowed.base
        ))
        .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(callback.status(), StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(token_hits.load(Ordering::SeqCst), 1);
    assert_eq!(userinfo_hits.load(Ordering::SeqCst), 1);
    assert_eq!(
        seen_secret.lock().unwrap().as_deref(),
        Some("sink-client-secret")
    );
    assert_eq!(
        seen_authorization.lock().unwrap().as_deref(),
        Some("Bearer sink-access-token")
    );

    discovery_server.abort();
    token_server.abort();
    userinfo_server.abort();
}
