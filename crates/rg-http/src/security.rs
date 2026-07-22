//! Security headers middleware for IronForge.
//!
//! Phase 22-D: Adds defense-in-depth HTTP security headers to all responses.
//! These headers protect against common web vulnerabilities (XSS, clickjacking,
//! MIME sniffing, etc.) without affecting functionality.
//!
//! H-2: CSP uses per-request nonces instead of 'unsafe-inline' for script-src.
//! The nonce is generated here and stored in request extensions for the SPA
//! handler to inject into `<script>` tags.

use axum::extract::Request;
use axum::http::{header, HeaderValue, Uri};
use axum::middleware::Next;
use axum::response::Response;
use std::collections::BTreeSet;

/// Per-request CSP nonce, stored in request extensions.
///
/// The SPA fallback handler reads this to inject `nonce="<value>"` into
/// all `<script>` tags in `index.html`.
#[derive(Clone, Debug)]
pub struct CspNonce(pub String);

/// Add one nonce attribute to every trusted SPA bootstrap script tag.
///
/// This deliberately performs a single replacement pass. Running separate
/// replacements for `<script>` and `<script ...>` causes the first result to
/// match the second pass and produces duplicate nonce attributes, which
/// Chromium rejects under a strict CSP.
pub(crate) fn inject_csp_nonce(html: &str, nonce: &str) -> String {
    html.replace("<script", &format!("<script nonce=\"{nonce}\""))
}

/// Middleware that adds security headers to all responses.
///
/// Headers added (Phase 22-D):
/// - `X-Content-Type-Options: nosniff` — prevent MIME sniffing
/// - `X-Frame-Options: DENY` — prevent clickjacking
/// - `X-XSS-Protection: 0` — disable legacy XSS filter (modern browsers deprecated it)
/// - `Referrer-Policy: strict-origin-when-cross-origin` — limit referrer info
/// - `Strict-Transport-Security` — HSTS (only added if request is HTTPS)
/// - `Content-Security-Policy` — restrict resource loading
/// - `Permissions-Policy` — disable unused browser features
/// - `Cross-Origin-Opener-Policy: same-origin`
/// - `Cross-Origin-Resource-Policy: same-origin`
pub async fn security_headers_middleware(request: Request, next: Next) -> Response {
    let is_https = is_https_uri(request.uri())
        || request
            .headers()
            .get("x-forwarded-proto")
            .and_then(|v| v.to_str().ok())
            .map(|v| v == "https")
            .unwrap_or(false);

    // H-2: Generate a per-request nonce for CSP.
    // 128 bits of entropy (16 bytes → 32 hex chars) — sufficient for CSP nonce.
    let nonce = uuid::Uuid::new_v4().simple().to_string();
    let mut request = request;
    request.extensions_mut().insert(CspNonce(nonce.clone()));

    let mut response = next.run(request).await;
    let headers = response.headers_mut();

    // Prevent MIME type sniffing
    headers.insert(
        header::HeaderName::from_static("x-content-type-options"),
        HeaderValue::from_static("nosniff"),
    );

    // Prevent clickjacking
    headers.insert(
        header::HeaderName::from_static("x-frame-options"),
        HeaderValue::from_static("DENY"),
    );

    // Disable legacy XSS filter (modern browsers handle this via CSP)
    headers.insert(
        header::HeaderName::from_static("x-xss-protection"),
        HeaderValue::from_static("0"),
    );

    // Limit referrer information leakage
    headers.insert(
        header::HeaderName::from_static("referrer-policy"),
        HeaderValue::from_static("strict-origin-when-cross-origin"),
    );

    // HSTS — only over HTTPS
    if is_https {
        headers.insert(
            header::HeaderName::from_static("strict-transport-security"),
            HeaderValue::from_static("max-age=31536000; includeSubDomains; preload"),
        );
    }

    // Content Security Policy — nonce-based (H-2)
    //
    // `script-src 'self' 'nonce-<value>'` replaces the previous `'unsafe-inline'`.
    // The SPA handler injects the nonce into all inline `<script>` tags so
    // SvelteKit hydration works without weakening CSP.
    // `style-src 'unsafe-inline'` is retained because SvelteKit inlines styles
    // and there is no runtime mechanism to nonce them in static-build mode.
    let csp = build_content_security_policy(&nonce);
    headers.insert(
        header::HeaderName::from_static("content-security-policy"),
        HeaderValue::from_str(&csp).expect("CSP header is valid ASCII"),
    );

    // Permissions Policy — disable unused browser features
    headers.insert(
        header::HeaderName::from_static("permissions-policy"),
        HeaderValue::from_static(
            "geolocation=(), microphone=(), camera=(), payment=(), usb=(), magnetometer=(), gyroscope=(), accelerometer=()",
        ),
    );

    // Cross-Origin policies for isolation
    headers.insert(
        header::HeaderName::from_static("cross-origin-opener-policy"),
        HeaderValue::from_static("same-origin"),
    );
    headers.insert(
        header::HeaderName::from_static("cross-origin-resource-policy"),
        HeaderValue::from_static("same-origin"),
    );

    response
}

/// Check if the request URI scheme is HTTPS.
fn is_https_uri(uri: &Uri) -> bool {
    uri.scheme().map(|s| s == "https").unwrap_or(false)
}

fn build_content_security_policy(nonce: &str) -> String {
    let cors_origins = std::env::var("IRONFORGE_CORS_ORIGINS").ok();
    let explicit_connect_src = std::env::var("IRONFORGE_CSP_CONNECT_SRC").ok();
    let connect_src = build_connect_src(cors_origins.as_deref(), explicit_connect_src.as_deref());

    format!(
        "default-src 'self'; \
         script-src 'self' 'nonce-{}'; \
         style-src 'self' 'unsafe-inline'; \
         img-src 'self' data: https:; \
         font-src 'self' data:; \
         connect-src {}; \
         frame-ancestors 'none'; \
         base-uri 'self'; \
         form-action 'self'",
        nonce, connect_src
    )
}

fn build_connect_src(cors_origins: Option<&str>, explicit_sources: Option<&str>) -> String {
    let mut sources = BTreeSet::from(["'self'".to_string()]);

    if let Some(origins) = cors_origins {
        for origin in split_source_list(origins) {
            add_connect_source(&mut sources, origin);
        }
    }

    if let Some(explicit) = explicit_sources {
        for source in split_source_list(explicit) {
            add_connect_source(&mut sources, source);
        }
    }

    sources.into_iter().collect::<Vec<_>>().join(" ")
}

fn split_source_list(value: &str) -> impl Iterator<Item = &str> {
    value
        .split([',', ' ', '\n', '\t'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
}

fn add_connect_source(sources: &mut BTreeSet<String>, source: &str) {
    if source == "*" || source == "'self'" {
        sources.insert(source.to_string());
        return;
    }

    let Ok(uri) = source.parse::<Uri>() else {
        sources.insert(source.to_string());
        return;
    };

    let Some(scheme) = uri.scheme_str() else {
        sources.insert(source.to_string());
        return;
    };
    let Some(authority) = uri.authority() else {
        sources.insert(source.to_string());
        return;
    };

    sources.insert(format!("{}://{}", scheme, authority));

    let ws_scheme = match scheme {
        "http" => Some("ws"),
        "https" => Some("wss"),
        "ws" | "wss" => None,
        _ => None,
    };
    if let Some(ws_scheme) = ws_scheme {
        sources.insert(format!("{}://{}", ws_scheme, authority));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    use axum::middleware::from_fn;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    async fn dummy_handler() -> &'static str {
        "ok"
    }

    #[test]
    fn spa_scripts_receive_exactly_one_csp_nonce() {
        let html = "<script>one()</script><script type=\"module\">two()</script>";
        let modified = inject_csp_nonce(html, "test-nonce");

        assert_eq!(modified.matches("nonce=\"test-nonce\"").count(), 2);
        assert!(!modified.contains("nonce=\"test-nonce\" nonce="));
        assert_eq!(
            modified,
            "<script nonce=\"test-nonce\">one()</script><script nonce=\"test-nonce\" type=\"module\">two()</script>"
        );
    }

    #[tokio::test]
    async fn test_security_headers_added() {
        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(from_fn(security_headers_middleware));

        let response = app
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();

        let headers = response.headers();
        assert_eq!(headers.get("x-content-type-options").unwrap(), "nosniff");
        assert_eq!(headers.get("x-frame-options").unwrap(), "DENY");
        assert_eq!(headers.get("x-xss-protection").unwrap(), "0");
        assert_eq!(
            headers.get("referrer-policy").unwrap(),
            "strict-origin-when-cross-origin"
        );
        assert!(headers.get("content-security-policy").is_some());
        assert!(headers.get("permissions-policy").is_some());
        assert!(headers.get("cross-origin-opener-policy").is_some());
    }

    #[tokio::test]
    async fn test_hsts_only_on_https() {
        let app = Router::new()
            .route("/", get(dummy_handler))
            .layer(from_fn(security_headers_middleware));

        // HTTP request — no HSTS
        let response = app
            .clone()
            .oneshot(Request::builder().uri("/").body(Body::empty()).unwrap())
            .await
            .unwrap();
        assert!(response
            .headers()
            .get("strict-transport-security")
            .is_none());

        // HTTPS request — has HSTS
        let response = app
            .oneshot(
                Request::builder()
                    .uri("https://example.com/")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert!(response
            .headers()
            .get("strict-transport-security")
            .is_some());
    }

    #[test]
    fn connect_src_includes_cors_origins_and_websocket_equivalents() {
        let connect_src =
            build_connect_src(Some("https://app.example.com,http://127.0.0.1:5173"), None);

        assert!(connect_src.contains("'self'"));
        assert!(connect_src.contains("https://app.example.com"));
        assert!(connect_src.contains("wss://app.example.com"));
        assert!(connect_src.contains("http://127.0.0.1:5173"));
        assert!(connect_src.contains("ws://127.0.0.1:5173"));
    }

    #[test]
    fn connect_src_accepts_explicit_sources() {
        let connect_src =
            build_connect_src(None, Some("https://api.example.com wss://api.example.com"));

        assert!(connect_src.contains("'self'"));
        assert!(connect_src.contains("https://api.example.com"));
        assert!(connect_src.contains("wss://api.example.com"));
    }
}
