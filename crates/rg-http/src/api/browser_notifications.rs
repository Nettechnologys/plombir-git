//! Browser witness bootstrap and the explicitly placed full-login hooks.
use crate::{error::AppError, AppState};
use axum::http::{
    header::{COOKIE, SET_COOKIE},
    HeaderMap, HeaderValue,
};
use rg_core::notification::browser::{self, Witness, COOKIE_NAME};

/// Only authentication ceremonies bootstrap evidence. Ordinary API traffic,
/// session refresh and revoked-cookie recovery keep their existing cookie
/// contract and cannot renew or create browser evidence as a side effect.
pub(crate) fn is_ceremony(method: &axum::http::Method, path: &str) -> bool {
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    if *method == axum::http::Method::POST {
        matches!(
            path,
            "/users/register"
                | "/users/login"
                | "/users/password/initial"
                | "/users/verify-email"
                | "/users/reset-password"
                | "/users/mfa/verify"
                | "/users/passkeys/login/start"
                | "/users/passkeys/login/finish"
        )
    } else if *method == axum::http::Method::GET {
        path.strip_prefix("/auth/sso/").is_some_and(|suffix| {
            suffix != "providers" && (!suffix.contains('/') || suffix.ends_with("/callback"))
        })
    } else {
        false
    }
}

/// Make freshly issued evidence available to this authentication request and
/// return the one Set-Cookie header the outer middleware must publish. Existing
/// valid cookies are not renewed. Only this non-authorizing cookie is replaced.
pub(crate) fn bootstrap(state: &AppState, headers: &mut HeaderMap) -> Option<HeaderValue> {
    let now = chrono::Utc::now().timestamp();
    if crate::api::auth::cookie_value(headers, COOKIE_NAME)
        .and_then(|value| Witness::verify(value, &state.jwt_secret, now))
        .is_some()
    {
        return None;
    }
    let witness = Witness::issue(&state.jwt_secret, now);
    let https = crate::public_url::request_is_https(state, headers);
    let mut cookies = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .map(str::trim)
        .filter(|cookie| {
            cookie
                .split_once('=')
                .is_none_or(|(key, _)| key != COOKIE_NAME)
        })
        .filter(|cookie| !cookie.is_empty())
        .map(str::to_owned)
        .collect::<Vec<_>>();
    cookies.push(format!("{COOKIE_NAME}={}", witness.value()));
    if let Ok(value) = HeaderValue::from_str(&cookies.join("; ")) {
        headers.remove(COOKIE);
        headers.insert(COOKIE, value);
    }
    HeaderValue::from_str(&witness.cookie(https, now)).ok()
}

pub(crate) fn append_cookie(response: &mut axum::response::Response, cookie: Option<HeaderValue>) {
    if let Some(cookie) = cookie {
        response.headers_mut().append(SET_COOKIE, cookie);
    }
}

/// `token` must be the session just issued after full proof. No login-log,
/// first-factor, refresh, PAT or test-notification path calls this function.
pub(crate) async fn completed_token(
    state: &AppState,
    headers: &HeaderMap,
    token: &str,
    method: &str,
) -> Result<(), AppError> {
    let claims = rg_core::auth::jwt::validate_token(token, &state.jwt_secret)
        .filter(|claims| claims.pat_id.is_none())
        .ok_or_else(|| AppError::internal("completed login has no session"))?;
    let id = claims
        .sub
        .parse::<i64>()
        .map_err(|_| AppError::internal("completed login has no account"))?;
    completed(state, headers, id, claims.session_version, method).await
}

pub(crate) async fn completed(
    state: &AppState,
    headers: &HeaderMap,
    user_id: i64,
    session_version: i64,
    method: &str,
) -> Result<(), AppError> {
    let now = chrono::Utc::now().timestamp();
    let witness = crate::api::auth::cookie_value(headers, COOKIE_NAME)
        .and_then(|value| Witness::verify(value, &state.jwt_secret, now))
        .unwrap_or_else(|| Witness::issue(&state.jwt_secret, now));
    match browser::complete(&state.db_write, user_id, session_version, &witness, method)
        .await
        .map_err(AppError::from)?
    {
        Some(_) => Ok(()),
        None => Err(AppError::SessionRevoked(
            "account changed before login completed".into(),
        )),
    }
}
