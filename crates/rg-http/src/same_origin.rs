//! The origin check for requests that ride on the session cookie (card_71613e98a0c5).
//!
//! The session cookie is `SameSite=Strict`, which keeps a browser from
//! attaching it to a request another *site* started — but that is the
//! browser's behaviour, not the server's check, and a page on a sibling
//! subdomain is the same site. So a request that carries the cookie and changes
//! state, or opens the notifications WebSocket, has to come from a page this
//! instance stands behind:
//!
//! * its `Origin` is one this instance publishes — `external_url` (or, without
//!   one, the scheme and `Host` the request arrived with) or an exact entry of
//!   `PLOMBIR_GIT_CORS_ORIGINS`, the documented cross-origin frontend; or
//! * the browser itself says the request is `same-origin` (or user-initiated,
//!   `none`) in `Sec-Fetch-Site`, a header no page can forge. That is what lets
//!   a development proxy that rewrites `Host` (`vite` with `changeOrigin`)
//!   through: the page and the API are one origin to the browser, whatever the
//!   backend sees in `Host`.
//!
//! Not checked: a request without the cookie — Bearer, PAT, Basic, runner
//! tokens, anonymous — because a browser does not attach those on a page's
//! behalf; the safe methods, which change nothing; and a request with neither
//! header, which is what git, docker, curl and the CLI send — they hold the
//! cookie only if their user gave it to them.

use std::sync::Arc;

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::error::AppError;
use crate::AppState;

/// What the guard compares an `Origin` with, fixed when the router is built.
#[derive(Clone)]
pub(crate) struct SameOriginPolicy {
    app: AppState,
    /// `PLOMBIR_GIT_CORS_ORIGINS`, parsed as the CORS layer parses it. A `*`
    /// entry is not carried over: a wildcard cannot carry credentials in a
    /// browser anyway, and it must not turn the cookie check off.
    cors_origins: Arc<[Origin]>,
}

impl SameOriginPolicy {
    pub(crate) fn new(app: AppState, cors_origins: Option<&str>) -> Self {
        let cors_origins = cors_origins
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|entry| !entry.is_empty() && *entry != "*")
            .filter_map(Origin::parse)
            .collect();
        Self { app, cors_origins }
    }
}

pub(crate) async fn same_origin_guard(
    State(policy): State<SameOriginPolicy>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(refusal) = refusal(&policy, request.method(), request.headers()) {
        return refusal.into_response();
    }
    next.run(request).await
}

fn refusal(policy: &SameOriginPolicy, method: &Method, headers: &HeaderMap) -> Option<AppError> {
    if !changes_state(method, headers) || !carries_session_cookie(headers) {
        return None;
    }
    let origin = header_str(headers, header::ORIGIN);
    let site = header_str(headers, "sec-fetch-site").map(|site| site.trim().to_ascii_lowercase());

    if origin.is_some_and(|origin| published(policy, headers, origin)) {
        return None;
    }
    if site
        .as_deref()
        .is_some_and(|site| site == "same-origin" || site == "none")
    {
        return None;
    }
    if origin.is_none() && site.is_none() {
        return None;
    }
    tracing::warn!(
        origin = origin.unwrap_or("-"),
        sec_fetch_site = site.as_deref().unwrap_or("-"),
        "refused a session-cookie request from a page this instance does not publish"
    );
    Some(AppError::forbidden(
        "this request carries the session cookie but comes from a page this instance does not \
         publish; sign in on this instance's own address, or add the page's origin to \
         PLOMBIR_GIT_CORS_ORIGINS",
    ))
}

/// Mutations, and the WebSocket handshake — a `GET` that hands the socket the
/// authority the cookie carries for as long as it stays open.
fn changes_state(method: &Method, headers: &HeaderMap) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        || header_str(headers, header::UPGRADE)
            .is_some_and(|value| value.trim().eq_ignore_ascii_case("websocket"))
}

fn carries_session_cookie(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|cookies| {
            cookies.split(';').any(|pair| {
                pair.trim_start()
                    .split_once('=')
                    .is_some_and(|(name, value)| {
                        name == crate::api::auth::AUTH_COOKIE_NAME && !value.is_empty()
                    })
            })
        })
}

fn header_str(headers: &HeaderMap, name: impl header::AsHeaderName) -> Option<&str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// Whether `origin` is one this instance publishes.
fn published(policy: &SameOriginPolicy, headers: &HeaderMap, origin: &str) -> bool {
    let Some(origin) = Origin::parse(origin) else {
        return false;
    };
    if policy.cors_origins.contains(&origin) {
        return true;
    }
    if let Some(configured) = policy.app.external_url.as_deref() {
        return Origin::parse(configured).is_some_and(|own| own == origin);
    }
    // No published address: the `Host` the request arrived with. Its scheme is
    // a guess — a TLS proxy that forwards no `X-Forwarded-Proto` makes an
    // `https` page look like `http` here — so a `Host` without a port matches
    // either default port. The host itself is not a guess: a page elsewhere
    // cannot make the victim's browser send this `Host`.
    let Some(host) = header_str(headers, header::HOST)
        .map(str::trim)
        .filter(|host| !host.is_empty())
    else {
        return false;
    };
    let Some(own) = Origin::parse(&format!("http://{host}")) else {
        return false;
    };
    if own.host != origin.host {
        return false;
    }
    let host_names_a_port = host
        .rsplit_once(':')
        .is_some_and(|(_, port)| !port.contains(']') && port.chars().all(|c| c.is_ascii_digit()));
    if host_names_a_port {
        own.port == origin.port
    } else {
        matches!(origin.port, Some(80) | Some(443))
    }
}

/// Scheme, host and port, the default port folded away: `https://git.example`
/// and `https://git.example:443` are one origin. A path is ignored, so an
/// `external_url` published with one still names its origin.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Origin {
    scheme: String,
    host: String,
    port: Option<u16>,
}

impl Origin {
    fn parse(value: &str) -> Option<Self> {
        let uri: axum::http::Uri = value.trim().parse().ok()?;
        let scheme = uri.scheme_str()?.to_ascii_lowercase();
        let default_port = match scheme.as_str() {
            "http" => 80,
            "https" => 443,
            _ => return None,
        };
        let host = uri.host()?.to_ascii_lowercase();
        if host.is_empty() {
            return None;
        }
        Some(Self {
            scheme,
            host,
            port: Some(uri.port_u16().unwrap_or(default_port)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_origin_compares_scheme_host_and_port_with_the_default_folded_away() {
        assert_eq!(
            Origin::parse("https://Git.Example.com"),
            Origin::parse("https://git.example.com:443/some/path")
        );
        assert_ne!(
            Origin::parse("https://git.example.com"),
            Origin::parse("http://git.example.com")
        );
        assert_ne!(
            Origin::parse("https://git.example.com"),
            Origin::parse("https://git.example.com:8443")
        );
        assert_eq!(Origin::parse("null"), None);
        assert_eq!(Origin::parse("file:///etc/passwd"), None);
    }

    #[test]
    fn only_a_non_empty_session_cookie_counts() {
        let mut headers = HeaderMap::new();
        headers.insert(
            header::COOKIE,
            "other=1; plombir_git_token=abc".parse().unwrap(),
        );
        assert!(carries_session_cookie(&headers));
        headers.insert(header::COOKIE, "plombir_git_token=; x=y".parse().unwrap());
        assert!(!carries_session_cookie(&headers));
        headers.insert(header::COOKIE, "not_plombir_git_token=abc".parse().unwrap());
        assert!(!carries_session_cookie(&headers));
    }

    #[test]
    fn the_websocket_handshake_counts_as_a_change_of_state() {
        let mut headers = HeaderMap::new();
        assert!(!changes_state(&Method::GET, &headers));
        headers.insert(header::UPGRADE, "WebSocket".parse().unwrap());
        assert!(changes_state(&Method::GET, &headers));
        assert!(changes_state(&Method::POST, &HeaderMap::new()));
        assert!(!changes_state(&Method::OPTIONS, &HeaderMap::new()));
    }
}
