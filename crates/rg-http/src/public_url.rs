//! The address this instance gives clients for itself.
//!
//! Every URL the server writes into a response for a client to follow next — an
//! LFS action href, the OCI token realm, the CI OIDC issuer, a package index's
//! download links, an SSO callback, a WebAuthn origin, a password-reset link —
//! answers the same question: what is my public base URL? It used to be
//! answered once per feature, by four different rules, and each was wrong on
//! some deployment (card_f78054e9e98f):
//!
//! - LFS and the OIDC issuer wrote `http://` unconditionally, so an instance
//!   serving its own `[tls]` sent clients to `http://` on a TLS port;
//! - the OCI realm and the package indexes guessed `https` from the host name
//!   and never read `external_url`, so a plain-HTTP instance on `git.lan:8080`
//!   advertised `https://git.lan:8080` and an instance behind a proxy
//!   advertised the internal `Host`;
//! - SSO guessed the scheme from a `:443` / `:8443` port, which a browser never
//!   writes into `Host` for the default port.
//!
//! The rule now lives here once: the configured `external_url` wins; without
//! it the request's `Host` is used, with `https` exactly when the request
//! arrived over TLS — the server's own listener, or a proxy in front of it that
//! says so in `X-Forwarded-Proto`.

use axum::http::{header, HeaderMap};

use crate::error::AppError;
use crate::AppState;

/// Whether this request reached the client side of the connection over TLS.
///
/// A configured `external_url` decides when it names a scheme: it is the
/// address the operator published and the one a browser visits, so a proxy
/// that terminates TLS and forwards no `X-Forwarded-Proto` must not take the
/// `Secure` attribute off the session cookie, and an `http` URL must not be
/// talked into `Secure` by a header any client can send. Without one, true when
/// this process terminates TLS itself or a proxy in front of it reports
/// `X-Forwarded-Proto: https`. The forwarded header is not a trust boundary in
/// that case: it only decides the scheme of URLs handed back to the same
/// caller, who controls `Host` just as fully.
pub(crate) fn request_is_https(state: &AppState, headers: &HeaderMap) -> bool {
    configured_https(state.external_url.as_deref())
        .unwrap_or_else(|| transport_is_https(state.tls_enabled, headers))
}

/// The transport half of [`request_is_https`], for a layer that is handed the
/// listener's TLS flag rather than the whole state — the security-headers
/// middleware, which asks [`configured_https`] first itself.
pub(crate) fn transport_is_https(tls_enabled: bool, headers: &HeaderMap) -> bool {
    tls_enabled || forwarded_https(headers)
}

/// The scheme a configured `external_url` pins: `Some(true)` for `https`,
/// `Some(false)` for any other scheme, `None` when no URL is configured or it
/// names no scheme — the signal to ask the transport instead. Anything but
/// `https` reads as plain on purpose: the worst a malformed URL may cost is a
/// cookie without `Secure`, never an HSTS promise the deployment cannot keep.
pub(crate) fn configured_https(external_url: Option<&str>) -> Option<bool> {
    let (scheme, _) = external_url?.trim().split_once("://")?;
    let scheme = scheme.trim();
    if scheme.is_empty() {
        return None;
    }
    Some(scheme.eq_ignore_ascii_case("https"))
}

/// This instance's public base URL for this request, without a trailing slash.
///
/// `None` only when `external_url` is unset and the request carries no usable
/// `Host` — there is then no honest address to name.
pub(crate) fn public_base_url(state: &AppState, headers: &HeaderMap) -> Option<String> {
    if let Some(url) = state.external_url.as_deref() {
        return Some(url.trim_end_matches('/').to_string());
    }
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|host| !host.is_empty())?;
    let scheme = if request_is_https(state, headers) {
        "https"
    } else {
        "http"
    };
    Some(format!("{scheme}://{host}"))
}

/// [`public_base_url`], refusing the request when there is no address to name
/// rather than inventing a `localhost` one the client would then follow.
pub(crate) fn require_public_base_url(
    state: &AppState,
    headers: &HeaderMap,
) -> Result<String, AppError> {
    public_base_url(state, headers).ok_or_else(missing_host)
}

/// The refusal [`require_public_base_url`] answers with, for callers that have
/// to wrap it in a protocol's own error envelope.
pub(crate) fn missing_host() -> AppError {
    AppError::bad_request("Host header is required when external_url is not configured")
}

fn forwarded_https(headers: &HeaderMap) -> bool {
    headers
        .get("x-forwarded-proto")
        .and_then(|value| value.to_str().ok())
        // A proxy chain appends: the first entry is the hop the client spoke to.
        .and_then(|value| value.split(',').next())
        .is_some_and(|proto| proto.trim().eq_ignore_ascii_case("https"))
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
    fn a_configured_public_url_pins_the_scheme() {
        assert_eq!(
            configured_https(Some("https://git.example.com")),
            Some(true)
        );
        assert_eq!(
            configured_https(Some("HTTPS://Git.Example.com/")),
            Some(true)
        );
        assert_eq!(
            configured_https(Some("http://git.example.com")),
            Some(false)
        );
        assert_eq!(configured_https(Some("ftp://git.example.com")), Some(false));
        // No scheme at all is no statement about one.
        assert_eq!(configured_https(Some("git.example.com")), None);
        assert_eq!(configured_https(Some("git.example.com:443")), None);
        assert_eq!(configured_https(Some("")), None);
        assert_eq!(configured_https(None), None);
    }

    #[test]
    fn the_first_forwarded_hop_decides_the_scheme() {
        assert!(forwarded_https(&headers(&[(
            "x-forwarded-proto",
            "HTTPS, http"
        )])));
        assert!(!forwarded_https(&headers(&[(
            "x-forwarded-proto",
            "http, https"
        )])));
        assert!(!forwarded_https(&headers(&[])));
    }
}
