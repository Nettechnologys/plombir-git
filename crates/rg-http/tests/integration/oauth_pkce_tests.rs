//! End-to-end OIDC discovery, PKCE and callback-cookie regression coverage.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use crate::common::{build_test_app_state, setup_test_db};
use axum::extract::{Form, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use sha2::{Digest, Sha256};

#[derive(Clone)]
struct MockOidcState {
    base_url: String,
    token_calls: Arc<AtomicUsize>,
    last_verifier: Arc<Mutex<Option<String>>>,
}

async fn discovery(State(state): State<MockOidcState>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "issuer": state.base_url,
        "authorization_endpoint": format!("{}/authorize", state.base_url),
        "token_endpoint": format!("{}/token", state.base_url),
        "userinfo_endpoint": format!("{}/userinfo", state.base_url),
    }))
}

async fn token(
    State(state): State<MockOidcState>,
    Form(form): Form<HashMap<String, String>>,
) -> (StatusCode, Json<serde_json::Value>) {
    state.token_calls.fetch_add(1, Ordering::SeqCst);
    *state.last_verifier.lock().unwrap() = form.get("code_verifier").cloned();
    if form.get("code").map(String::as_str) != Some("valid-code")
        || form.get("grant_type").map(String::as_str) != Some("authorization_code")
    {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "invalid_request"})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "access_token": "mock-access-token",
            "refresh_token": "mock-refresh-token",
            "expires_in": 3600
        })),
    )
}

async fn userinfo(headers: HeaderMap) -> (StatusCode, Json<serde_json::Value>) {
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some("Bearer mock-access-token")
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(serde_json::json!({"error": "invalid_token"})),
        );
    }
    (
        StatusCode::OK,
        Json(serde_json::json!({
            "sub": "subject-1",
            "preferred_username": "oidc-user",
            "email": "oidc-user@example.com",
            "name": "OIDC User"
        })),
    )
}

/// Userinfo for an IdP that answers a *different person* on every call while
/// withholding the email — the shape behind `card_0a08de4d6707`. The third call
/// does return an address, and says out loud that it is unconfirmed.
async fn userinfo_without_a_usable_email(
    State(calls): State<Arc<AtomicUsize>>,
) -> (StatusCode, Json<serde_json::Value>) {
    let nth = calls.fetch_add(1, Ordering::SeqCst) + 1;
    let mut claims = serde_json::json!({
        "sub": format!("subject-{nth}"),
        "preferred_username": format!("nomail-{nth}"),
        "name": "No Mail",
    });
    if nth == 3 {
        claims["email"] = serde_json::json!("claimed@example.com");
        claims["email_verified"] = serde_json::json!(false);
    }
    (StatusCode::OK, Json(claims))
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

/// An SSO profile that identifies nobody must not be allowed to identify
/// *somebody*.
///
/// Before the identity gate, a provider that withheld the email left
/// `email = ""` on the profile, and `""` reached `find_by_email` as an ordinary
/// key: the first such login provisioned an account holding it, and every later
/// one matched that row and signed into a stranger's account. Two different
/// subjects go through the real callback here, and neither may end up anywhere
/// near the other.
#[tokio::test]
async fn sso_logins_without_a_usable_email_are_refused_instead_of_merged() {
    let userinfo_calls = Arc::new(AtomicUsize::new(0));
    let oidc_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let oidc_addr = oidc_listener.local_addr().unwrap().to_string();
    let oidc_base = format!("http://{oidc_addr}");
    let oidc_state = MockOidcState {
        base_url: oidc_base.clone(),
        token_calls: Arc::new(AtomicUsize::new(0)),
        last_verifier: Arc::new(Mutex::new(None)),
    };
    let oidc_app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .with_state(oidc_state)
        .merge(
            Router::new()
                .route("/userinfo", get(userinfo_without_a_usable_email))
                .with_state(userinfo_calls.clone()),
        );
    let oidc_server = tokio::spawn(async move {
        axum::serve(oidc_listener, oidc_app).await.unwrap();
    });
    crate::common::wait_for_listener(&oidc_addr).await;

    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    rg_db::ops::sso_provider_ops::upsert(
        &db,
        None,
        "Mock OIDC",
        "oidc-nomail",
        "oidc",
        Some("client-id"),
        None,
        Some(&format!("{oidc_base}/.well-known/openid-configuration")),
        Some("openid profile"),
        None,
        None,
        None,
        None,
        None,
        None,
        true,
        None,
    )
    .await
    .unwrap();

    let app = rg_http::create_router_for_test(build_test_app_state(db.clone(), repo_root));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let app_server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    // One full authorize → callback round trip, exactly as a browser drives it.
    let sign_in = |client: reqwest::Client, base: String| async move {
        let authorize = client
            .get(format!("{base}/api/v1/auth/sso/oidc-nomail"))
            .send()
            .await
            .unwrap();
        let state_cookie = cookie_pair(authorize.headers(), "forgekeep_sso_state");
        let verifier_cookie = cookie_pair(authorize.headers(), "forgekeep_sso_code_verifier");
        let state = signed_cookie_value(&state_cookie);
        let callback = client
            .get(format!(
                "{base}/api/v1/auth/sso/oidc-nomail/callback?code=valid-code&state={state}"
            ))
            .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
            .send()
            .await
            .unwrap();
        (callback.status(), callback.text().await.unwrap())
    };

    for expected_subject in 1..=2 {
        let (status, body) = sign_in(client.clone(), base.clone()).await;
        assert_eq!(
            status,
            StatusCode::BAD_REQUEST,
            "a login the provider would not name must not succeed"
        );
        assert!(
            body.contains("email"),
            "the refusal has to say which field is missing, got: {body}"
        );
        assert!(
            rg_db::ops::oauth_account_ops::find_by_provider_and_uid(
                &db,
                "oidc-nomail",
                &format!("subject-{expected_subject}"),
            )
            .await
            .unwrap()
            .is_none(),
            "a refused login must leave no OAuth link behind"
        );
        assert!(
            rg_db::ops::user_ops::find_by_username(&db, &format!("nomail-{expected_subject}"))
                .await
                .unwrap()
                .is_none(),
            "a refused login must provision no account"
        );
    }

    // Nothing was provisioned under the empty address either — the row that
    // used to become everyone's account.
    assert!(rg_db::ops::user_ops::find_by_email(&db, "")
        .await
        .unwrap()
        .is_none());

    // Third call: the provider does hand over an address, and reports it as
    // unconfirmed. That address is the key an existing local account would be
    // merged on, so an unconfirmed one does not get to nominate the owner.
    let (status, body) = sign_in(client.clone(), base.clone()).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        body.contains("unverified"),
        "the refusal has to name the reason, got: {body}"
    );
    assert!(
        rg_db::ops::user_ops::find_by_email(&db, "claimed@example.com")
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(userinfo_calls.load(Ordering::SeqCst), 3);

    app_server.abort();
    oidc_server.abort();
}

#[tokio::test]
async fn oidc_callback_uses_discovery_and_pkce_and_rejects_missing_verifier() {
    let token_calls = Arc::new(AtomicUsize::new(0));
    let last_verifier = Arc::new(Mutex::new(None));
    let oidc_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let oidc_addr = oidc_listener.local_addr().unwrap().to_string();
    let oidc_base = format!("http://{oidc_addr}");
    let oidc_state = MockOidcState {
        base_url: oidc_base.clone(),
        token_calls: token_calls.clone(),
        last_verifier: last_verifier.clone(),
    };
    let oidc_app = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/token", post(token))
        .route("/userinfo", get(userinfo))
        .with_state(oidc_state);
    let oidc_server = tokio::spawn(async move {
        axum::serve(oidc_listener, oidc_app).await.unwrap();
    });
    crate::common::wait_for_listener(&oidc_addr).await;

    let (db, app_dir) = setup_test_db().await;
    let repo_root = app_dir.path().join("repos");
    std::fs::create_dir_all(&repo_root).unwrap();
    rg_db::ops::sso_provider_ops::upsert(
        &db,
        None,
        "Mock OIDC",
        "oidc-test",
        "oidc",
        Some("client-id"),
        None,
        Some(&format!("{oidc_base}/.well-known/openid-configuration")),
        Some("openid profile email"),
        None,
        None,
        None,
        None,
        None,
        None,
        true,
        None,
    )
    .await
    .unwrap();

    let app = rg_http::create_router_for_test(build_test_app_state(db.clone(), repo_root));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();
    let base = format!("http://{addr}");
    let app_server = tokio::spawn(async move {
        let _app_dir = app_dir;
        axum::serve(listener, app).await.unwrap();
    });
    crate::common::wait_for_listener(&addr).await;
    let client = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap();

    let authorize = client
        .get(format!("{base}/api/v1/auth/sso/oidc-test"))
        .send()
        .await
        .unwrap();
    assert!(authorize.status().is_redirection());
    let state_cookie = cookie_pair(authorize.headers(), "forgekeep_sso_state");
    let verifier_cookie = cookie_pair(authorize.headers(), "forgekeep_sso_code_verifier");
    let state = signed_cookie_value(&state_cookie);
    let verifier = signed_cookie_value(&verifier_cookie);
    assert!((43..=128).contains(&verifier.len()));

    let location = authorize
        .headers()
        .get(header::LOCATION)
        .unwrap()
        .to_str()
        .unwrap();
    let auth_url = reqwest::Url::parse(location).unwrap();
    assert_eq!(auth_url.path(), "/authorize");
    let query = auth_url.query_pairs().collect::<HashMap<_, _>>();
    assert_eq!(query.get("state").map(|v| v.as_ref()), Some(state.as_str()));
    assert_eq!(
        query.get("code_challenge_method").map(|v| v.as_ref()),
        Some("S256")
    );
    let expected_challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(
        query.get("code_challenge").map(|v| v.as_ref()),
        Some(expected_challenge.as_str())
    );

    let callback = client
        .get(format!(
            "{base}/api/v1/auth/sso/oidc-test/callback?code=valid-code&state={state}"
        ))
        .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
        .send()
        .await
        .unwrap();
    assert!(callback.status().is_redirection());
    assert_eq!(callback.headers()[header::LOCATION], "/dashboard");
    let set_cookies = callback
        .headers()
        .get_all(header::SET_COOKIE)
        .iter()
        .map(|value| value.to_str().unwrap().to_string())
        .collect::<Vec<_>>();
    assert!(set_cookies
        .iter()
        .any(|cookie| cookie.starts_with("forgekeep_token=")));
    assert!(set_cookies
        .iter()
        .any(|cookie| cookie.starts_with("forgekeep_sso_state=;") && cookie.contains("Max-Age=0")));
    assert!(set_cookies.iter().any(|cookie| {
        cookie.starts_with("forgekeep_sso_code_verifier=;") && cookie.contains("Max-Age=0")
    }));
    assert_eq!(token_calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        last_verifier.lock().unwrap().as_deref(),
        Some(verifier.as_str())
    );

    let user = rg_db::ops::user_ops::find_by_username(&db, "oidc-user")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(user.email, "oidc-user@example.com");
    assert!(
        rg_db::ops::oauth_account_ops::find_by_provider_and_uid(&db, "oidc-test", "subject-1")
            .await
            .unwrap()
            .is_some()
    );

    let authorize_without_verifier = client
        .get(format!("{base}/api/v1/auth/sso/oidc-test"))
        .send()
        .await
        .unwrap();
    let state_cookie = cookie_pair(authorize_without_verifier.headers(), "forgekeep_sso_state");
    let state = signed_cookie_value(&state_cookie);
    let missing_verifier = client
        .get(format!(
            "{base}/api/v1/auth/sso/oidc-test/callback?code=valid-code&state={state}"
        ))
        .header(header::COOKIE, state_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_verifier.status(), StatusCode::FORBIDDEN);
    assert_eq!(token_calls.load(Ordering::SeqCst), 1);

    let authorize_mismatch = client
        .get(format!("{base}/api/v1/auth/sso/oidc-test"))
        .send()
        .await
        .unwrap();
    let state_cookie = cookie_pair(authorize_mismatch.headers(), "forgekeep_sso_state");
    let verifier_cookie = cookie_pair(authorize_mismatch.headers(), "forgekeep_sso_code_verifier");
    let mismatch = client
        .get(format!(
            "{base}/api/v1/auth/sso/oidc-test/callback?code=valid-code&state=wrong-state"
        ))
        .header(header::COOKIE, format!("{state_cookie}; {verifier_cookie}"))
        .send()
        .await
        .unwrap();
    assert_eq!(mismatch.status(), StatusCode::FORBIDDEN);
    assert_eq!(token_calls.load(Ordering::SeqCst), 1);

    app_server.abort();
    oidc_server.abort();
}
