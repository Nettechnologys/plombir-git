//! card_671043329ecf: a `client_secret_enc` the server cannot decrypt is the
//! server's problem, and all three SSO doors have to say so the same way.
//!
//! `refresh_token` used to swallow the failure through a double
//! `unwrap_or_default()` — `Err(..)` became `None`, `None` became `""` — and
//! then asked the provider to refresh with an empty secret. The provider
//! refused, and the refusal came back as `400 failed to refresh token`: a
//! request the caller cannot fix, reported as theirs to fix. `authorize` and
//! `callback` classified the same failure as a 500.
//!
//! The three now share one `provider_config`, so what this file pins is the
//! behaviour rather than the shape: on one and the same broken row, every door
//! answers 5xx. Each assertion is paired with the identical request against a
//! provider whose secret is simply absent — legitimate, and still working — so
//! a green run cannot come from a fixture that breaks everything.

use std::collections::HashMap;

use axum::extract::Form;
use axum::routing::{get, post};
use axum::{Json, Router};
use rg_db::ops::sso_provider_ops::SsoProviderInput;

use crate::common::{register_full, spawn_test_app_with_db, wait_for_listener};

/// Not base64, so `encryption::decrypt` fails before it ever reaches AES —
/// which is what an operator gets after rotating `[auth].encryption_key`
/// without re-encrypting what was stored under the old one.
const UNREADABLE_SECRET: &str = "!!! not a ciphertext !!!";

async fn token(Form(_form): Form<HashMap<String, String>>) -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "access_token": "refreshed-access-token",
        "refresh_token": "refreshed-refresh-token",
        "expires_in": 3600
    }))
}

struct Harness {
    db: sea_orm::DatabaseConnection,
    base: String,
    client: reqwest::Client,
    token: String,
    provider_id: i64,
    /// The CSRF/PKCE cookies `authorize` set, replayed on the callback. Carried
    /// by hand because the test client has no cookie jar — and carrying them is
    /// the point: without them the callback is refused at the CSRF check and
    /// never reaches the code that reads the secret.
    cookies: std::sync::Mutex<String>,
    idp_server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start() -> Harness {
        let idp_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let idp_addr = idp_listener.local_addr().unwrap().to_string();
        let idp_base = format!("http://{idp_addr}");
        let discovery_base = idp_base.clone();
        let idp_app = Router::new()
            .route(
                "/.well-known/openid-configuration",
                get(move || {
                    let base = discovery_base.clone();
                    async move {
                        Json(serde_json::json!({
                            "issuer": base,
                            "authorization_endpoint": format!("{base}/authorize"),
                            "token_endpoint": format!("{base}/token"),
                            "userinfo_endpoint": format!("{base}/userinfo"),
                        }))
                    }
                }),
            )
            .route("/token", post(token));
        let idp_server = tokio::spawn(async move {
            axum::serve(idp_listener, idp_app).await.unwrap();
        });
        wait_for_listener(&idp_addr).await;

        let (base, db) = spawn_test_app_with_db().await;
        let provider = rg_db::ops::sso_provider_ops::upsert(
            &db,
            None,
            SsoProviderInput {
                name: "Mock IdP",
                slug: "idp",
                provider_type: "oidc",
                client_id: Some("client-id"),
                discovery_url: Some(&format!("{idp_base}/.well-known/openid-configuration")),
                scopes: Some("openid profile email"),
                enabled: true,
                ..Default::default()
            },
        )
        .await
        .expect("seed SSO provider");

        let (token, user_id) =
            register_full(&base, "sso-secret", "sso-secret@example.test").await;
        rg_db::ops::oauth_account_ops::upsert(
            &db, user_id, "idp", "subject-1", "sso-secret", "sso-secret@example.test", None, None,
            None,
        )
        .await
        .expect("seed OAuth account link");

        Harness {
            db,
            base,
            // Redirects must not be followed — the provider is a mock.
            client: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .build()
                .unwrap(),
            token,
            provider_id: provider.id,
            cookies: std::sync::Mutex::new(String::new()),
            idp_server,
        }
    }

    /// Store a secret that cannot be decrypted, leaving every other column of
    /// the row exactly as it was.
    async fn break_the_secret(&self) {
        let current = rg_db::ops::sso_provider_ops::find_by_id(&self.db, self.provider_id)
            .await
            .expect("read provider")
            .expect("provider must exist");
        rg_db::ops::sso_provider_ops::upsert(
            &self.db,
            Some(self.provider_id),
            SsoProviderInput {
                name: &current.name,
                slug: &current.slug,
                provider_type: &current.provider_type,
                client_id: current.client_id.as_deref(),
                client_secret_enc: Some(UNREADABLE_SECRET),
                discovery_url: current.discovery_url.as_deref(),
                scopes: current.scopes.as_deref(),
                enabled: current.enabled,
                ..Default::default()
            },
        )
        .await
        .expect("store the unreadable secret");
    }

    async fn authorize(&self) -> reqwest::Response {
        self.client
            .get(format!("{}/api/v1/auth/sso/idp", self.base))
            .send()
            .await
            .expect("authorize request")
    }

    async fn callback(&self, csrf_state: &str) -> reqwest::Response {
        let cookies = self.cookies.lock().expect("cookie jar").clone();
        self.client
            .get(format!(
                "{}/api/v1/auth/sso/idp/callback?code=any-code&state={csrf_state}",
                self.base
            ))
            .header("cookie", cookies)
            .send()
            .await
            .expect("callback request")
    }

    async fn refresh(&self) -> reqwest::Response {
        self.client
            .post(format!("{}/api/v1/auth/sso/idp/refresh", self.base))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({"refresh_token": "stored-refresh-token"}))
            .send()
            .await
            .expect("refresh request")
    }

    /// Run a login start and hand back the `state` the provider is supposed to
    /// echo, so the callback gets past the CSRF check and reaches the code that
    /// actually reads the secret.
    async fn start_login(&self) -> String {
        let response = self.authorize().await;
        assert!(
            response.status().is_redirection(),
            "the baseline login start must redirect, got {}",
            response.status()
        );
        let cookies: Vec<String> = response
            .headers()
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .filter_map(|value| value.split(';').next())
            .map(str::to_string)
            .collect();
        assert!(
            !cookies.is_empty(),
            "the login start must set the CSRF and PKCE cookies"
        );
        *self.cookies.lock().expect("cookie jar") = cookies.join("; ");

        let location = response
            .headers()
            .get("location")
            .expect("a redirect carries a location")
            .to_str()
            .expect("the location is text")
            .to_string();
        let url = reqwest::Url::parse(&location).expect("the location is a URL");
        url.query_pairs()
            .find(|(key, _)| key == "state")
            .map(|(_, value)| value.into_owned())
            .expect("the authorize URL carries the CSRF state")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.idp_server.abort();
    }
}

#[tokio::test]
async fn every_sso_door_answers_an_unreadable_client_secret_the_same_way() {
    let app = Harness::start().await;

    // ── Baseline: no stored secret at all, which is legitimate ──
    let csrf_state = app.start_login().await;
    let baseline_callback = app.callback(&csrf_state).await;
    assert_ne!(
        baseline_callback.status(),
        403,
        "the replayed cookies must carry the callback past the CSRF check — otherwise \
         the 5xx asserted below would prove nothing about the secret"
    );
    assert!(
        !baseline_callback.status().is_server_error(),
        "a provider without a stored secret must not fail on the server's side: {}",
        baseline_callback.status()
    );
    let baseline_refresh = app.refresh().await;
    assert_eq!(
        baseline_refresh.status(),
        200,
        "a provider without a stored secret must still refresh: {}",
        baseline_refresh.text().await.unwrap_or_default()
    );

    // ── The one column that changes ──
    app.break_the_secret().await;

    let start = app.authorize().await;
    assert!(
        start.status().is_server_error(),
        "a secret the server cannot read is the server's failure, not a redirect: {}",
        start.status()
    );

    // The cookies from the baseline start are still in the jar, so the CSRF
    // check passes and the callback reaches the secret.
    let callback = app.callback(&csrf_state).await;
    assert!(
        callback.status().is_server_error(),
        "the callback must classify the unreadable secret as ours: {}",
        callback.status()
    );

    let refresh = app.refresh().await;
    assert!(
        refresh.status().is_server_error(),
        "the refresh must not blame the caller for a secret they cannot see \
         (this answered 400 'failed to refresh token'): {}",
        refresh.status()
    );
}
