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
/// True when this process terminates TLS itself, when a proxy in front of it
/// reports `X-Forwarded-Proto: https`, or when a configured `external_url`
/// pins the scheme to `https`. The configured URL is the operator's own
/// statement about how clients reach the instance and it outranks both guesses:
/// a proxy that drops the forwarded header must not turn the `Secure` attribute
/// off an auth cookie, or HSTS off a response, for an instance that has already
/// declared itself HTTPS (Low finding, security audit). An `external_url` on
/// `http` — or one no scheme can be read from — answers `false` and lets the
/// transport decide.
pub(crate) fn request_is_https(state: &AppState, headers: &HeaderMap) -> bool {
    configured_https(state.external_url.as_deref())
        .unwrap_or_else(|| transport_is_https(state.tls_enabled, headers))
}

/// [`request_is_https`] for a layer that is handed only the listener's TLS
/// flag rather than the whole state — the security-headers middleware.
///
/// It knows nothing about `external_url`; callers that have one ask
/// [`configured_https`] first.
pub(crate) fn transport_is_https(tls_enabled: bool, headers: &HeaderMap) -> bool {
    tls_enabled || forwarded_https(headers)
}

/// The scheme `external_url` pins, when the deployment configured one.
///
/// `None` when no URL is configured, or the string names no scheme, which is
/// the signal to fall back to [`transport_is_https`]. Only `https` counts as
/// TLS: an `http` URL answers `false`, and so does anything unrecognised — the
/// worst case of a malformed URL must be an unprotected connection, never a
/// `Secure` cookie or an HSTS promise the deployment cannot keep.
pub(crate) fn configured_https(external_url: Option<&str>) -> Option<bool> {
    let scheme = external_url?.trim().split_once(':')?.0.trim();
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
            configured_https(Some("http://git.example.com")),
            Some(false)
        );
        assert_eq!(
            configured_https(Some("HTTPS://Git.Example.com")),
            Some(true)
        );
        // No scheme at all is not a configured choice.
        assert_eq!(configured_https(Some("git.example.com")), None);
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
