//! What a signed-in account can do about itself.
//!
//! PUT    /api/v1/users/me/password        — change the password
//! POST   /api/v1/users/password/initial   — replace a password an administrator chose
//! PATCH  /api/v1/users/me                 — display name and bio
//! POST   /api/v1/users/me/email           — ask to move to another address
//! POST   /api/v1/users/verify-email       — follow a mailed confirmation link
//! DELETE /api/v1/users/me                 — delete the account
//! PUT    /api/v1/users/me/avatar          — upload a picture
//! DELETE /api/v1/users/me/avatar          — remove it
//! GET    /api/v1/avatars/{username}       — serve it
//!
//! The rules live in [`rg_core::user::account`]; this module is the HTTP
//! boundary: who may call, what is journalled, which session the caller walks
//! away with (card_ca894e30ac80, card_9f18b657580b, card_45f98ab2fe1a).

use axum::{
    body::Bytes,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderValue, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use serde::Deserialize;
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_credential};
use crate::api::auth::{AuthUser, SessionUser};
use crate::api::users::{build_auth_cookie, build_clear_cookie};
use crate::error::AppError;
use crate::AppState;

/// Largest avatar upload the route accepts — the body limit `routes.rs`
/// mounts, and the same number the core refuses past.
pub(crate) const AVATAR_UPLOAD_MAX_BYTES: usize = 512 * 1024;
const _: () = assert!(AVATAR_UPLOAD_MAX_BYTES == rg_core::user::account::MAX_AVATAR_BYTES);

fn directories(state: &AppState) -> rg_core::user::service::LdapDirectories<'_> {
    rg_core::user::service::LdapDirectories {
        encryption_key: &state.encryption_key,
        transport_policy: &state.ldap_transport_policy,
    }
}

/// The mail an address confirmation goes out through — `None` on an
/// instance that cannot send one with a link only it chose.
pub(crate) fn mailer(state: &AppState) -> Option<rg_core::user::account::Mailer<'_>> {
    Some(rg_core::user::account::Mailer {
        smtp: state.smtp_config.as_ref()?,
        base_url: state.external_url.as_deref()?,
    })
}

async fn load_user(
    state: &AppState,
    user_id: i64,
) -> Result<rg_db::entities::user::Model, AppError> {
    rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("user not found"))
}

/// A session for `user`, as the login answers one: the token in the body and
/// in the HttpOnly cookie.
fn session_response(
    state: &AppState,
    headers: &HeaderMap,
    status: StatusCode,
    user: &rg_db::entities::user::Model,
) -> Response {
    let token = match rg_core::auth::jwt::generate_token(
        user.id,
        &user.username,
        user.session_version,
        &state.jwt_secret,
        7,
    ) {
        Ok(token) => token,
        Err(error) => return AppError::from(error).into_response(),
    };
    let is_https = crate::public_url::request_is_https(state, headers);
    (
        status,
        [(header::SET_COOKIE, build_auth_cookie(&token, is_https))],
        Json(serde_json::json!({
            "token": token,
            "user_id": user.id,
            "username": user.username,
            "mfa_required": false,
        })),
    )
        .into_response()
}

/// The answer for an account whose password is settled and whose second
/// factor is still owed — the same shape `POST /users/login` gives it.
fn second_factor_response(
    state: &AppState,
    headers: &HeaderMap,
    user: &rg_db::entities::user::Model,
) -> Response {
    let challenge = match rg_core::auth::jwt::generate_mfa_challenge(
        user.id,
        &user.username,
        "password",
        &state.jwt_secret,
    ) {
        Ok(challenge) => challenge,
        Err(error) => return AppError::from(error).into_response(),
    };
    let is_https = crate::public_url::request_is_https(state, headers);
    let mut response = (
        StatusCode::OK,
        [(
            header::SET_COOKIE,
            crate::api::mfa::build_mfa_challenge_cookie(&challenge, is_https),
        )],
        Json(serde_json::json!({
            "token": "",
            "user_id": user.id,
            "username": user.username,
            "mfa_required": true,
        })),
    )
        .into_response();
    if let Ok(value) = HeaderValue::from_str(&build_clear_cookie(is_https)) {
        response.headers_mut().append(header::SET_COOKIE, value);
    }
    response
}

// ── Password ─────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct ChangePasswordRequest {
    pub current_password: String,
    pub new_password: String,
}

/// PUT /api/v1/users/me/password
#[utoipa::path(
    put,
    path = "/users/me/password",
    tag = "Users",
    request_body = ChangePasswordRequest,
    responses(
        (status = 200, description = "Password changed. Every other session is signed out; this \
            one continues with the token and cookie in the response. Personal access tokens and \
            SSH keys are separate credentials and stay valid.", body = serde_json::Value),
        (status = 400, description = "The new password breaks a rule, or the account's password lives with its identity provider", body = serde_json::Value),
        (status = 401, description = "The current password is wrong, or the account is locked", body = serde_json::Value),
        (status = 409, description = "The password was replaced by a reset while this request was in flight", body = serde_json::Value),
    ),
)]
pub async fn change_password(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    headers: HeaderMap,
    Json(body): Json<ChangePasswordRequest>,
) -> Response {
    let user = match load_user(&state, user_id).await {
        Ok(user) => user,
        Err(error) => return error.into_response(),
    };
    if user.auth_provider != "local" {
        return AppError::bad_request(format!(
            "this account signs in through {}, which holds its password",
            user.auth_provider
        ))
        .into_response();
    }
    let actor = match grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if let Err(error) = crate::api::mfa::confirm_account_password(
        &state,
        &user,
        &body.current_password,
        "password-change",
        &headers,
    )
    .await
    {
        return error.into_response();
    }
    match rg_core::user::account::change_password(&state.db, &user, &body.new_password).await {
        Ok(updated) => {
            record_credential(
                &state,
                &actor,
                "user.change_password",
                user_id,
                &headers,
                serde_json::json!({ "other_sessions_revoked": true }),
            )
            .await;
            session_response(&state, &headers, StatusCode::OK, &updated)
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct InitialPasswordRequest {
    /// Username or email, as on the login form.
    pub login: String,
    /// The password the administrator handed over.
    pub password: String,
    pub new_password: String,
}

/// POST /api/v1/users/password/initial
///
/// The second half of a login that answered `password_change_required`: the
/// administrator-chosen password is proved once more — through the same
/// lockout every password door counts against — and replaced in the same
/// request. Nothing before this point handed out a session.
#[utoipa::path(
    post,
    path = "/users/password/initial",
    tag = "Users",
    request_body = InitialPasswordRequest,
    responses(
        (status = 200, description = "Password replaced. A session, or — for an account with a \
            second factor — the MFA challenge `POST /users/login` would answer", body = serde_json::Value),
        (status = 400, description = "The new password breaks a rule", body = serde_json::Value),
        (status = 401, description = "Invalid credentials, or the account is locked", body = serde_json::Value),
        (status = 409, description = "No password change is pending for this account — sign in normally", body = serde_json::Value),
    ),
)]
pub async fn set_initial_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<InitialPasswordRequest>,
) -> Response {
    use rg_core::auth::lockout::PasswordAttempt;

    let lookup = if body.login.contains('@') {
        rg_db::ops::user_ops::find_by_email(&state.db, &body.login).await
    } else {
        rg_db::ops::user_ops::find_by_username(&state.db, &body.login).await
    };
    let found = match lookup {
        Ok(found) => found.filter(|user| user.auth_provider == "local"),
        Err(error) => return AppError::from(error).into_response(),
    };
    // Paid whether or not the account exists, exactly as on the login form.
    let password_ok = match rg_core::auth::password::verify_password_or_dummy(
        &body.password,
        found.as_ref().map(|user| user.password_hash.as_str()),
        crate::client_ip::from_headers(&headers),
    )
    .await
    {
        Ok(ok) => ok,
        Err(error) => return AppError::from(error).into_response(),
    };
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(&headers);
    let attempt = match rg_core::auth::lockout::settle_password_attempt(
        &state.db,
        found.as_ref(),
        password_ok,
        rg_core::auth::lockout::AttemptOrigin {
            login: &body.login,
            channel: "password-initial",
            ip_address: ip_address.as_deref(),
            user_agent: user_agent.as_deref(),
        },
    )
    .await
    {
        Ok(attempt) => attempt,
        Err(error) => return AppError::from(error).into_response(),
    };
    match attempt {
        PasswordAttempt::PasswordChangeRequired => {}
        // The right password, and nothing to replace: the ordinary login is
        // the way in. Saying so leaks nothing — only the holder gets here.
        PasswordAttempt::Accepted(_) | PasswordAttempt::SecondFactorRequired => {
            return AppError::conflict(
                "no password change is pending for this account; sign in normally",
            )
            .into_response();
        }
        PasswordAttempt::Rejected { locked } => {
            return AppError::unauthorized(if locked {
                "account is temporarily locked"
            } else {
                "invalid credentials"
            })
            .into_response();
        }
    }
    let Some(found) = found else {
        return AppError::unauthorized("invalid credentials").into_response();
    };
    // Re-read: the attempt above reset the failure counters on this row.
    let user = match load_user(&state, found.id).await {
        Ok(user) => user,
        Err(error) => return error.into_response(),
    };
    match rg_core::user::account::change_password(&state.db, &user, &body.new_password).await {
        Ok(updated) => {
            let actor =
                rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, updated.id).await;
            record_credential(
                &state,
                &actor,
                "user.change_password",
                updated.id,
                &headers,
                serde_json::json!({ "replaced_administrator_password": true }),
            )
            .await;
            if updated.mfa_enabled {
                second_factor_response(&state, &headers, &updated)
            } else {
                session_response(&state, &headers, StatusCode::OK, &updated)
            }
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Profile ──────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct UpdateProfileRequest {
    /// `null` or a blank string clears it; an absent key leaves it alone.
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "crate::api::clearable::double_option")]
    pub bio: Option<Option<String>>,
}

/// PATCH /api/v1/users/me
#[utoipa::path(
    patch,
    path = "/users/me",
    tag = "Users",
    request_body = UpdateProfileRequest,
    responses(
        (status = 200, description = "The updated profile", body = serde_json::Value),
        (status = 400, description = "A field is too long", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn update_profile(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    Json(body): Json<UpdateProfileRequest>,
) -> Response {
    match rg_core::user::account::update_profile(&state.db, user_id, body.display_name, body.bio)
        .await
    {
        Ok(user) => (StatusCode::OK, Json(serde_json::json!(user))).into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Address ──────────────────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct ChangeEmailRequest {
    pub email: String,
    /// The account's current password.
    pub password: String,
}

/// POST /api/v1/users/me/email
///
/// Nothing changes until the link mailed to the new address is followed. An
/// address another account holds is answered exactly like a free one.
#[utoipa::path(
    post,
    path = "/users/me/email",
    tag = "Users",
    request_body = ChangeEmailRequest,
    responses(
        (status = 202, description = "A confirmation link was mailed to the new address, if it can take one", body = serde_json::Value),
        (status = 400, description = "Not an address, the current one, or an account whose address comes from its identity provider", body = serde_json::Value),
        (status = 401, description = "The password is wrong, or the account is locked", body = serde_json::Value),
        (status = 409, description = "This instance has no outbound mail to confirm an address with", body = serde_json::Value),
    ),
)]
pub async fn request_email_change(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    headers: HeaderMap,
    Json(body): Json<ChangeEmailRequest>,
) -> Response {
    // `409`, not `403`: nothing about the caller is refused — the instance
    // cannot do this for anyone until its operator configures mail.
    let Some(mailer) = mailer(&state) else {
        return AppError::conflict(
            "changing the address needs outbound mail to confirm it, and this instance has none \
             configured ([smtp] and [server].external_url); ask an administrator",
        )
        .into_response();
    };
    let user = match load_user(&state, user_id).await {
        Ok(user) => user,
        Err(error) => return error.into_response(),
    };
    if user.auth_provider != "local" {
        return AppError::bad_request(format!(
            "this account's address comes from {}",
            user.auth_provider
        ))
        .into_response();
    }
    if let Err(error) = crate::api::mfa::confirm_account_password(
        &state,
        &user,
        &body.password,
        "email-change",
        &headers,
    )
    .await
    {
        return error.into_response();
    }
    match rg_core::user::account::request_email_change(&state.db, mailer, &user, &body.email).await
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "status": "confirmation_sent",
                "message": "Follow the link we mailed to the new address to finish the change.",
            })),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// POST /api/v1/users/me/email/verify
///
/// Mail a link that proves the account receives mail at the address it already
/// has — one registered on an open instance, by an administrator, or before
/// addresses were proved (card_2296f052332b). Inside the per-address cooldown
/// nothing new is sent; the link already in the inbox stays the live one.
#[utoipa::path(
    post,
    path = "/users/me/email/verify",
    tag = "Users",
    responses(
        (status = 202, description = "A confirmation link was mailed to the account's address", body = serde_json::Value),
        (status = 400, description = "The address is already confirmed", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "This instance has no outbound mail to confirm an address with", body = serde_json::Value),
    ),
)]
pub async fn request_email_verification(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
) -> Response {
    // `409`, as for an address change: the instance, not the caller, is what
    // cannot do this.
    let Some(mailer) = mailer(&state) else {
        return AppError::conflict(
            "confirming an address needs outbound mail, and this instance has none configured \
             ([smtp] and [server].external_url); addresses here are not confirmed",
        )
        .into_response();
    };
    let user = match load_user(&state, user_id).await {
        Ok(user) => user,
        Err(error) => return error.into_response(),
    };
    match rg_core::user::account::request_email_verification(&state.db, mailer, &user).await {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(serde_json::json!({
                "status": "confirmation_sent",
                "message": "Follow the link we mailed to your address to confirm it.",
            })),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[derive(Deserialize, ToSchema)]
pub struct ConfirmEmailRequest {
    pub token: String,
}

/// POST /api/v1/users/verify-email
#[utoipa::path(
    post,
    path = "/users/verify-email",
    tag = "Users",
    request_body = ConfirmEmailRequest,
    responses(
        (status = 201, description = "The registration this address was waiting for is now an account, signed in", body = serde_json::Value),
        (status = 200, description = "The account moved to the confirmed address, or confirmed the one it has", body = serde_json::Value),
        (status = 400, description = "The link is invalid, already used, or expired", body = serde_json::Value),
        (status = 403, description = "Registration was closed after the link was sent", body = serde_json::Value),
        (status = 409, description = "The name or the address was taken, or the account's address changed, before the link was followed", body = serde_json::Value),
    ),
)]
pub async fn confirm_email(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ConfirmEmailRequest>,
) -> Response {
    use rg_core::user::account::Confirmed;

    let permit = match rg_core::user::registration::authorize(&state.db, state.registration).await {
        Ok(permit) => permit,
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_core::user::account::confirm(&state.db, directories(&state), permit, &body.token).await
    {
        Ok(Confirmed::Registered(user)) => {
            let actor =
                rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, user.id).await;
            rg_core::audit::record(
                &state.db,
                &actor,
                "user.register",
                Some("user"),
                Some(user.id),
                Some(&user.username),
                Some(&headers),
                Some(serde_json::json!({ "email_confirmed": true })),
            )
            .await;
            crate::metrics::recorder::user_registered();
            crate::metrics::recorder::auth_event("register", "success");
            session_response(&state, &headers, StatusCode::CREATED, &user)
        }
        Ok(Confirmed::EmailChanged {
            user,
            previous_email,
        }) => {
            let actor =
                rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, user.id).await;
            record_credential(
                &state,
                &actor,
                "user.change_email",
                user.id,
                &headers,
                serde_json::json!({ "confirmed_by_link": true }),
            )
            .await;
            if let Some(mailer) = mailer(&state) {
                rg_core::user::account::notify_previous_address(
                    mailer,
                    &previous_email,
                    &user.username,
                );
            }
            (
                StatusCode::OK,
                Json(serde_json::json!({ "email": user.email })),
            )
                .into_response()
        }
        Ok(Confirmed::EmailVerified(user)) => {
            let actor =
                rg_core::audit::AuditActor::resolve_after_the_fact(&state.db, user.id).await;
            record_credential(
                &state,
                &actor,
                "user.verify_email",
                user.id,
                &headers,
                serde_json::json!({ "confirmed_by_link": true }),
            )
            .await;
            (
                StatusCode::OK,
                Json(serde_json::json!({ "email": user.email, "email_verified": true })),
            )
                .into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Deleting the account ─────────────────────────────────────────────────

#[derive(Deserialize, ToSchema)]
pub struct DeleteAccountRequest {
    /// The current password — required for an account that has one.
    pub password: Option<String>,
    /// The account's username, typed out — required for an account that
    /// signs in through an identity provider and so has no password here.
    pub confirm_username: Option<String>,
}

/// DELETE /api/v1/users/me
///
/// The same deletion an administrator runs — the account's repositories and
/// their storage go with it, and an account that still owns organizations is
/// refused until they are transferred or deleted.
#[utoipa::path(
    delete,
    path = "/users/me",
    tag = "Users",
    request_body = DeleteAccountRequest,
    responses(
        (status = 200, description = "The account is deleted and the session cookie cleared", body = serde_json::Value),
        (status = 400, description = "The confirmation is missing or does not match", body = serde_json::Value),
        (status = 401, description = "The password is wrong, or the account is locked", body = serde_json::Value),
        (status = 409, description = "The account is the instance's only administrator, or still owns organizations", body = serde_json::Value),
    ),
)]
pub async fn delete_account(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    headers: HeaderMap,
    Json(body): Json<DeleteAccountRequest>,
) -> Response {
    let user = match load_user(&state, user_id).await {
        Ok(user) => user,
        Err(error) => return error.into_response(),
    };
    if user.auth_provider == "local" {
        let Some(password) = body.password.as_deref() else {
            return AppError::bad_request("confirm with your password").into_response();
        };
        if let Err(error) = crate::api::mfa::confirm_account_password(
            &state,
            &user,
            password,
            "delete-account",
            &headers,
        )
        .await
        {
            return error.into_response();
        }
    } else if body.confirm_username.as_deref() != Some(user.username.as_str()) {
        return AppError::bad_request(
            "type the account's username in confirm_username to delete it",
        )
        .into_response();
    }
    if user.is_admin {
        match rg_db::ops::user_ops::count_active_admins(&state.db).await {
            Ok(admins) if admins <= 1 => {
                return AppError::conflict(
                    "this is the instance's only administrator account; make someone else an \
                     administrator before deleting it",
                )
                .into_response();
            }
            Ok(_) => {}
            Err(error) => return AppError::from(error).into_response(),
        }
    }
    // Resolved before the deletion: afterwards there is no account to name.
    let actor = match grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    match rg_core::user::service::delete_user(
        &state.db,
        &state.repo_root,
        state.blob_storage.as_ref(),
        state.oci_storage.as_ref(),
        user_id,
    )
    .await
    {
        Ok(()) => {
            rg_core::audit::record(
                &state.db,
                &actor,
                "user.delete_account",
                Some("user"),
                Some(user_id),
                Some(&user.username),
                Some(&headers),
                None,
            )
            .await;
            let is_https = crate::public_url::request_is_https(&state, &headers);
            (
                StatusCode::OK,
                [(header::SET_COOKIE, build_clear_cookie(is_https))],
                Json(serde_json::json!({ "deleted": true })),
            )
                .into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

// ── Avatar ───────────────────────────────────────────────────────────────

/// PUT /api/v1/users/me/avatar — the request body is the image itself.
#[utoipa::path(
    put,
    path = "/users/me/avatar",
    tag = "Users",
    request_body(content = Vec<u8>, content_type = "application/octet-stream",
        description = "A PNG, JPEG, GIF or WebP image of at most 512 KiB; the type is read from the bytes"),
    responses(
        (status = 200, description = "Stored; `avatar_url` is the address that serves it", body = serde_json::Value),
        (status = 400, description = "Not one of the accepted image types", body = serde_json::Value),
        (status = 413, description = "Larger than 512 KiB", body = serde_json::Value),
    ),
)]
pub async fn upload_avatar(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    body: Bytes,
) -> Response {
    let user = match load_user(&state, user_id).await {
        Ok(user) => user,
        Err(error) => return error.into_response(),
    };
    match rg_core::user::account::set_avatar(&state.db, &user, body.to_vec()).await {
        Ok(url) => (
            StatusCode::OK,
            Json(serde_json::json!({ "avatar_url": url })),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// DELETE /api/v1/users/me/avatar
#[utoipa::path(
    delete,
    path = "/users/me/avatar",
    tag = "Users",
    responses(
        (status = 204, description = "Removed"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn delete_avatar(State(state): State<AppState>, AuthUser(user_id): AuthUser) -> Response {
    match rg_core::user::account::remove_avatar(&state.db, user_id).await {
        Ok(()) => StatusCode::NO_CONTENT.into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[derive(Deserialize, utoipa::IntoParams)]
pub struct AvatarQuery {
    /// The digest `avatar_url` carries. With it the response is cached for
    /// good — a new picture is a new URL.
    pub v: Option<String>,
}

/// GET /api/v1/avatars/{username}
///
/// Public, like the name it sits next to on every page. Served under the
/// type the upload decided from the bytes, with `nosniff` and a policy that
/// lets the response be nothing but a picture.
#[utoipa::path(
    get,
    path = "/avatars/{username}",
    tag = "Users",
    params(("username" = String, Path, description = "Account name"), AvatarQuery),
    responses(
        (status = 200, description = "The image", content_type = "image/png"),
        (status = 404, description = "No such account, or it has no uploaded picture", body = serde_json::Value),
    ),
)]
pub async fn get_avatar(
    State(state): State<AppState>,
    Path(username): Path<String>,
    Query(query): Query<AvatarQuery>,
) -> Response {
    let user = match rg_db::ops::user_ops::find_active_by_username(&state.db, &username).await {
        Ok(Some(user)) if user.is_usable() => user,
        Ok(_) => return AppError::not_found("avatar not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    let avatar = match rg_db::ops::user_avatar_ops::find(&state.db, user.id).await {
        Ok(Some(avatar)) => avatar,
        Ok(None) => return AppError::not_found("avatar not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    let versioned = query
        .v
        .as_deref()
        .is_some_and(|v| !v.is_empty() && avatar.sha256.starts_with(v));
    let cache = if versioned {
        "public, max-age=31536000, immutable"
    } else {
        "public, max-age=300"
    };
    let content_type = HeaderValue::from_str(&avatar.content_type)
        .unwrap_or(HeaderValue::from_static("application/octet-stream"));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, content_type),
            (header::CACHE_CONTROL, HeaderValue::from_static(cache)),
            (
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            ),
            (
                header::CONTENT_SECURITY_POLICY,
                HeaderValue::from_static("default-src 'none'; sandbox"),
            ),
        ],
        avatar.bytes,
    )
        .into_response()
}
