//! Account-owned public keys trusted for commit signatures.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use sea_orm::Set;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_credential};
use crate::{
    api::auth::{AuthUser, SessionUser},
    error::AppError,
    AppState,
};

#[derive(Debug, Clone, Copy, Deserialize, Serialize, ToSchema)]
#[serde(rename_all = "lowercase")]
pub enum SigningKeyKind {
    Gpg,
    Ssh,
}

impl SigningKeyKind {
    fn as_str(self) -> &'static str {
        match self {
            Self::Gpg => "gpg",
            Self::Ssh => "ssh",
        }
    }
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateSigningKeyRequest {
    pub title: String,
    pub kind: SigningKeyKind,
    pub public_key: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SigningKeyResponse {
    pub id: i64,
    pub title: String,
    pub kind: String,
    pub public_key: String,
    pub fingerprint: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

impl From<rg_db::entities::commit_signing_key::Model> for SigningKeyResponse {
    fn from(key: rg_db::entities::commit_signing_key::Model) -> Self {
        Self {
            id: key.id,
            title: key.title,
            kind: key.kind,
            public_key: key.public_key,
            fingerprint: key.fingerprint,
            created_at: key.created_at,
        }
    }
}

#[utoipa::path(
    get,
    path = "/users/signing-keys",
    tag = "Users",
    responses((status = 200, body = [SigningKeyResponse]))
)]
pub async fn list_signing_keys(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
) -> impl IntoResponse {
    match rg_db::ops::commit_signing_key_ops::list_by_user(&state.db, user_id).await {
        Ok(keys) => (
            StatusCode::OK,
            Json(
                keys.into_iter()
                    .map(SigningKeyResponse::from)
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/users/signing-keys",
    tag = "Users",
    request_body = CreateSigningKeyRequest,
    responses(
        (status = 201, body = SigningKeyResponse),
        (status = 400, description = "Invalid public key"),
        (status = 403, description = "A login session and verified email are required"),
        (status = 409, description = "Key already registered")
    )
)]
pub async fn create_signing_key(
    State(state): State<AppState>,
    SessionUser(user_id): SessionUser,
    headers: HeaderMap,
    Json(body): Json<CreateSigningKeyRequest>,
) -> impl IntoResponse {
    let actor = match grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let account = match rg_db::ops::user_ops::find_by_id(&state.db, user_id).await {
        Ok(Some(account)) => account,
        Ok(None) => return AppError::not_found("account not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    if account.email_verified_at.is_none() {
        return AppError::forbidden("verify your email before registering a commit signing key")
            .into_response();
    }
    if !rg_git::signatures::safe_signing_email(&account.email) {
        return AppError::bad_request("account email cannot be used as a signing principal")
            .into_response();
    }
    let title = body.title.trim();
    if title.is_empty() || title.chars().count() > 100 {
        return AppError::bad_request("key title must be 1–100 characters").into_response();
    }

    let (public_key, fingerprint) = match body.kind {
        SigningKeyKind::Ssh => {
            let source = body.public_key.trim();
            if source.len() > 16_384 {
                return AppError::bad_request("SSH public key is too large").into_response();
            }
            let fingerprint = match rg_core::auth::ssh_key::fingerprint_from_openssh(source) {
                Ok(fingerprint) => fingerprint,
                Err(error) => return AppError::bad_request(error).into_response(),
            };
            let mut parts = source.split_whitespace();
            let canonical = format!(
                "{} {}",
                parts.next().unwrap_or_default(),
                parts.next().unwrap_or_default()
            );
            (canonical, fingerprint)
        }
        SigningKeyKind::Gpg => {
            let source = body.public_key;
            let email = account.email.clone();
            match tokio::task::spawn_blocking(move || {
                rg_git::signatures::validate_gpg_public_key(&source, &email)
            })
            .await
            {
                Ok(Ok(result)) => result,
                Ok(Err(error)) => return AppError::bad_request(error).into_response(),
                Err(error) => return AppError::internal(error).into_response(),
            }
        }
    };

    let model = rg_db::entities::commit_signing_key::ActiveModel {
        id: sea_orm::NotSet,
        user_id: Set(user_id),
        title: Set(title.to_string()),
        kind: Set(body.kind.as_str().to_string()),
        public_key: Set(public_key),
        fingerprint: Set(fingerprint),
        created_at: Set(chrono::Utc::now()),
    };
    match rg_db::ops::commit_signing_key_ops::create(&state.db, model).await {
        Ok(key) => {
            record_credential(
                &state, &actor, "user.add_commit_signing_key", user_id, &headers,
                serde_json::json!({"key_id": key.id, "kind": key.kind, "fingerprint": key.fingerprint}),
            ).await;
            (StatusCode::CREATED, Json(SigningKeyResponse::from(key))).into_response()
        }
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => {
            AppError::conflict("this signing key is already registered").into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/users/signing-keys/{id}",
    tag = "Users",
    params(("id" = i64, Path, description = "Signing key id")),
    responses((status = 204), (status = 404, description = "Key not found"))
)]
pub async fn delete_signing_key(
    State(state): State<AppState>,
    AuthUser(user_id): AuthUser,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> impl IntoResponse {
    let actor = match grant_actor(&state, user_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    let key = match rg_db::ops::commit_signing_key_ops::find_by_id(&state.db, id).await {
        Ok(Some(key)) if key.user_id == user_id => key,
        Ok(_) => return AppError::not_found("signing key not found").into_response(),
        Err(error) => return AppError::from(error).into_response(),
    };
    match rg_db::ops::commit_signing_key_ops::delete_by_user(&state.db, id, user_id).await {
        Ok(true) => {
            record_credential(
                &state, &actor, "user.remove_commit_signing_key", user_id, &headers,
                serde_json::json!({"key_id": key.id, "kind": key.kind, "fingerprint": key.fingerprint}),
            ).await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => AppError::not_found("signing key not found").into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}
