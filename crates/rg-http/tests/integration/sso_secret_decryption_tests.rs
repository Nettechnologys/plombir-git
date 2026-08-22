//! card_671043329ecf: a `client_secret_enc` the server cannot decrypt is the
//! server's problem, and every SSO door has to say so the same way.
//!
//! The OAuth token-refresh door used to swallow the failure through a double
//! `unwrap_or_default()` — `Err(..)` became `None`, `None` became `""` — and
//! then asked the provider to refresh with an empty secret. The provider
//! refused, and the refusal came back as `400 failed to refresh token`: a
//! request the caller cannot fix, reported as theirs to fix. `authorize` and
//! `callback` classified the same failure as a 500. That door has since been
//! removed for want of any caller (card_76820bc5325e); the two that remain
//! keep the answer it was brought into line with.
//!
//! They share one `provider_config`, so what this file pins is the behaviour
//! rather than the shape: on one and the same broken row, every door answers
//! 5xx. Each assertion is paired with the identical request against a provider
//! whose secret is simply absent — legitimate, and still working — so a green
//! run cannot come from a fixture that breaks everything.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::extract::{Form, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use rg_db::ops::sso_provider_ops::SsoProviderInput;

use crate::common::{register_full, spawn_test_app_with_db, wait_for_listener};

/// Not base64, so `encryption::decrypt` fails before it ever reaches AES —
/// which is what an operator gets after rotating `[auth].encryption_key`
/// without re-encrypting what was stored under the old one.
const UNREADABLE_SECRET: &str = "!!! not a ciphertext !!!";

/// How many times the provider's token endpoint was actually called.
///
/// The acceptance this file answers has two halves — an unreadable secret must
/// answer `5xx` *and* must not reach the provider — and only the first is
/// visible in a status code. A door that asked the IdP to refresh with an empty
/// secret and then turned the IdP's refusal into a `5xx` would satisfy every
/// other assertion here while still leaking a broken instance as somebody
/// else's outage.
type TokenHits = Arc<AtomicUsize>;

async fn token(
    State(hits): State<TokenHits>,
    Form(_form): Form<HashMap<String, String>>,
) -> Json<serde_json::Value> {
    hits.fetch_add(1, Ordering::SeqCst);
    Json(serde_json::json!({
        "access_token": "refreshed-access-token",
        "refresh_token": "refreshed-refresh-token",
        "expires_in": 3600
    }))
}

/// The subject the seeded OAuth link already points at, so the baseline
/// callback completes a login instead of provisioning one.
///
/// The mock used to serve no `/userinfo` at all, which made the baseline
/// callback a `404` from the provider — dressed up as `400 failed to fetch
/// user info` by the handler of the day. The assertion below ("must not fail on
/// the server's side") passed on that `400` and therefore proved nothing about
/// the secret: the control never got as far as reading one. Once an unanswered
/// provider became the `502` it is (card_1a7da084c197), the hollow control
/// showed up as a failure. It is a real login now.
async fn userinfo() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "sub": "subject-1",
        "preferred_username": "sso-secret",
        "email": "sso-secret@example.test",
        "email_verified": true,
        "name": "SSO Secret"
    }))
}

struct Harness {
    db: sea_orm::DatabaseConnection,
    base: String,
    client: reqwest::Client,
    provider_id: i64,
    /// The CSRF/PKCE cookies `authorize` set, replayed on the callback. Carried
    /// by hand because the test client has no cookie jar — and carrying them is
    /// the point: without them the callback is refused at the CSRF check and
    /// never reaches the code that reads the secret.
    cookies: std::sync::Mutex<String>,
    /// Calls the mock provider's token endpoint has seen. See [`TokenHits`].
    token_hits: TokenHits,
    idp_server: tokio::task::JoinHandle<()>,
}

impl Harness {
    async fn start() -> Harness {
        let token_hits: TokenHits = Arc::new(AtomicUsize::new(0));
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
            .route("/userinfo", get(userinfo))
            .merge(
                Router::new()
                    .route("/token", post(token))
                    .with_state(token_hits.clone()),
            );
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

        let (_token, user_id) = register_full(&base, "sso-secret", "sso-secret@example.test").await;
        rg_db::ops::oauth_account_ops::upsert(
            &db,
            user_id,
            "idp",
            "subject-1",
            "sso-secret",
            "sso-secret@example.test",
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
            provider_id: provider.id,
            cookies: std::sync::Mutex::new(String::new()),
            token_hits,
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
    assert_eq!(
        baseline_callback.status(),
        307,
        "a provider without a stored secret must not merely avoid a 5xx — it must complete \
         the login, or the 5xx asserted below is measured against a control that never \
         reached the secret: {}",
        baseline_callback.status()
    );
    // The baseline did reach the provider, which is what makes the count
    // asserted after the break meaningful rather than a route nobody calls.
    let hits_before = app.token_hits.load(Ordering::SeqCst);
    assert_eq!(
        hits_before, 1,
        "the baseline callback must reach the provider's token endpoint"
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

    // The other half of the same defect, and the half a status code cannot
    // show: a door must stop at the unreadable secret, not ask the provider to
    // authenticate an empty one and then report its refusal. Counting the
    // provider's calls is what tells "we refused" apart from "the IdP refused
    // and we relabelled it".
    assert_eq!(
        app.token_hits.load(Ordering::SeqCst),
        hits_before,
        "a secret that will not decrypt must never reach the provider"
    );
}
