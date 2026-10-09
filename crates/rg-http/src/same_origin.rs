//! The same-origin guard for cookie-authenticated, state-changing requests.
//!
//! The session cookie is `SameSite=Strict`, which is the first line of defence
//! against cross-site request forgery — but that is a browser behaviour, not a
//! server check, and it says nothing about a page that is same-site but not
//! same-origin. A request that carries the session cookie and changes state (or
//! opens a WebSocket) must therefore say where it came from, and that answer
//! must agree with the address this instance publishes.
//!
//! What is deliberately *not* checked: requests without the session cookie —
//! Bearer, PAT, Basic, runner tokens, anonymous — because a browser does not
//! attach those on its own; safe methods, which change nothing; and requests
//! that carry neither `Origin` nor `Sec-Fetch-Site`, which is what non-browser
//! clients send (git, docker, curl, the CLI).

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::api::auth::AUTH_COOKIE_NAME;
use crate::error::AppError;
use crate::AppState;

/// Refuses a cookie-carrying mutation whose `Origin` (or `Sec-Fetch-Site`) does
/// not match the address this instance is reached at.
pub(crate) async fn same_origin_guard(
    State(state): State<AppState>,
    request: Request,
    next: Next,
) -> Response {
    if let Some(refusal) = refuses(&state, request.method(), request.headers()) {
        return refusal;
    }
    next.run(request).await
}

fn refuses(state: &AppState, method: &Method, headers: &HeaderMap) -> Option<Response> {
    if !changes_state(method, headers) || !carries_session_cookie(headers) {
        return None;
    }

    if let Some(origin) = header_str(headers, header::ORIGIN) {
        if origin_matches(state, headers, origin) {
            return None;
        }
        return Some(
            AppError::forbidden(
                "this request carries the session cookie, and its Origin is not the address \
                 this instance publishes — a browser will not attach the cookie to a page that \
                 is not yours, so the request is refused",
            )
            .into_response(),
        );
    }

    // No `Origin`: modern browsers still say what they consider the request to
    // be, and only the two "not a cross-site request" answers pass.
    if let Some(site) = header_str(headers, "sec-fetch-site") {
        let site = site.trim().to_ascii_lowercase();
        if site == "same-origin" || site == "none" {
            return None;
        }
        return Some(
            AppError::forbidden(
                "this request carries the session cookie, and Sec-Fetch-Site says it is \
                 cross-site",
            )
            .into_response(),
        );
    }

    // Neither header: a non-browser client (git, docker, the CLI) or an older
    // browser. There is nothing to compare, and those clients do not attach the
    // cookie on someone else's behalf.
    None
}

/// Mutations, and the WebSocket handshake (a `GET` that hands the socket the
/// same authority the cookie carries).
fn changes_state(method: &Method, headers: &HeaderMap) -> bool {
    !matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
        || is_websocket_upgrade(headers)
}

fn is_websocket_upgrade(headers: &HeaderMap) -> bool {
    header_str(headers, header::UPGRADE)
        .map(|value| value.eq_ignore_ascii_case("websocket"))
        .unwrap_or(false)
}

/// Whether this request presents the session cookie at all.
fn carries_session_cookie(headers: &HeaderMap) -> bool {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .any(|cookies| {
            cookies.split(';').any(|pair| {
                pair.trim_start()
                    .split_once('=')
                    .is_some_and(|(name, _)| name == AUTH_COOKIE_NAME)
            })
        })
}

fn header_str<'a>(
    headers: &'a HeaderMap,
    name: impl axum::http::header::AsHeaderName,
) -> Option<&'a str> {
    headers.get(name).and_then(|value| value.to_str().ok())
}

/// The origin this instance publishes: `external_url` when configured, else the
/// scheme and `Host` the request itself arrived with.
fn published_origin(state: &AppState, headers: &HeaderMap) -> Option<String> {
    if let Some(configured) = state.external_url.as_deref() {
        return Some(configured.trim_end_matches('/').to_string());
    }
    let host = header_str(headers, header::HOST)?.trim();
    if host.is_empty() {
        return None;
    }
    let scheme = if crate::public_url::request_is_https(state, headers) {
        "https"
    } else {
        "http"
    };
    Some(format!("{scheme}://{host}"))
}

fn origin_matches(state: &AppState, headers: &HeaderMap, origin: &str) -> bool {
    published_origin(state, headers).is_some_and(|expected| same_origin(&expected, origin))
}

/// Scheme, host and port, with the scheme's default port folded away — so
/// `https://git.example.com` and `https://git.example.com:443` are one origin.
fn same_origin(a: &str, b: &str) -> bool {
    match (split_origin(a), split_origin(b)) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

fn split_origin(value: &str) -> Option<(String, String, u16)> {
    let (scheme, rest) = value.trim().split_once("://")?;
    let scheme = scheme.to_ascii_lowercase();
    let authority = rest.split(['/', '?', '#']).next()?.trim();
    if authority.is_empty() {
        return None;
    }
    let (host, port) = match authority.rsplit_once(':') {
        Some((host, port)) if !port.is_empty() && port.chars().all(|c| c.is_ascii_digit()) => {
            (host.to_ascii_lowercase(), port.parse().ok()?)
        }
        _ => (authority.to_ascii_lowercase(), default_port(&scheme)?),
    };
    if host.is_empty() {
        return None;
    }
    Some((scheme, host, port))
}

fn default_port(scheme: &str) -> Option<u16> {
    match scheme {
        "http" | "ws" => Some(80),
        "https" | "wss" => Some(443),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;

    fn headers(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut map = HeaderMap::new();
        for (name, value) in pairs {
            map.insert(*name, HeaderValue::from_str(value).unwrap());
        }
        map
    }

    #[test]
    fn default_ports_are_the_same_origin() {
        assert!(same_origin(
            "https://git.example.com",
            "https://git.example.com:443"
        ));
        assert!(same_origin(
            "http://git.example.com:80",
            "http://git.example.com/"
        ));
        assert!(same_origin(
            "HTTPS://Git.Example.com",
            "https://git.example.com"
        ));
        assert!(!same_origin(
            "https://git.example.com",
            "https://evil.example.com"
        ));
        assert!(!same_origin(
            "https://git.example.com",
            "http://git.example.com"
        ));
        assert!(!same_origin(
            "https://git.example.com:8443",
            "https://git.example.com"
        ));
    }

    #[test]
    fn opaque_and_unknown_origins_never_match() {
        assert!(!same_origin("https://git.example.com", "null"));
        assert!(!same_origin(
            "https://git.example.com",
            "file:///etc/passwd"
        ));
        assert!(!same_origin("https://git.example.com", ""));
    }

    #[test]
    fn only_mutations_and_upgrades_are_checked() {
        let empty = HeaderMap::new();
        assert!(!changes_state(&Method::GET, &empty));
        assert!(!changes_state(&Method::HEAD, &empty));
        assert!(!changes_state(&Method::OPTIONS, &empty));
        assert!(changes_state(&Method::POST, &empty));
        assert!(changes_state(&Method::DELETE, &empty));
        let upgrade = headers(&[("upgrade", "websocket")]);
        assert!(changes_state(&Method::GET, &upgrade));
    }

    #[test]
    fn only_a_real_session_cookie_counts() {
        assert!(carries_session_cookie(&headers(&[(
            "cookie",
            "plombir_git_token=abc; other=1"
        )])));
        assert!(carries_session_cookie(&headers(&[(
            "cookie",
            "other=1; plombir_git_token=abc"
        )])));
        assert!(!carries_session_cookie(&headers(&[(
            "cookie",
            "other=1; plombir_git_token_extra=abc"
        )])));
        assert!(!carries_session_cookie(&HeaderMap::new()));
    }
}
