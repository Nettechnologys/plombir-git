//! Repository-scoped SSH deploy key management.

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use sea_orm::Set;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::access_audit::{grant_actor, record_grant};
use crate::api::repo_access::RepoAdmin;
use crate::{error::AppError, AppState};

/// What the journal says about a deploy key, and what it deliberately leaves
/// out.
///
/// `read_only` is the field that makes the entry worth reading: a key added
/// with `read_only: false` is **push access to this repository** for whoever
/// holds the private half — `rg-ssh` reads the column directly and lets
/// `git-receive-pack` through on it. An entry that named only the key would
/// answer "a key was added" and not "somebody can now push", which is the
/// question an incident review is actually asking (card_2a9beaf7b207).
///
/// The fingerprint rather than the key: the title is chosen by whoever adds the
/// key and identifies nothing, while the fingerprint is what the SSH server
/// matched on. The public key itself is not secret, but it is bulk — the
/// fingerprint is the same identity in a form an operator can compare by eye.
fn deploy_key_details(key: &rg_db::entities::deploy_key::Model) -> serde_json::Value {
    serde_json::json!({
        "title": key.title,
        "fingerprint": key.fingerprint,
        "read_only": key.read_only,
    })
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct CreateDeployKeyRequest {
    pub title: String,
    #[serde(alias = "key")]
    pub public_key: String,
    #[serde(default = "default_read_only")]
    pub read_only: bool,
}

fn default_read_only() -> bool {
    true
}

#[derive(Debug, Serialize, ToSchema)]
pub struct DeployKeyResponse {
    pub id: i64,
    pub title: String,
    pub public_key: String,
    pub fingerprint: String,
    pub read_only: bool,
    /// `null` once the account that added the key has been deleted. The key
    /// itself is the repository's and keeps working
    /// (`m20260805_000004_repo_config_outlives_its_author`).
    pub created_by_id: Option<i64>,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub last_used_at: Option<chrono::DateTime<chrono::Utc>>,
}

impl From<rg_db::entities::deploy_key::Model> for DeployKeyResponse {
    fn from(key: rg_db::entities::deploy_key::Model) -> Self {
        Self {
            id: key.id,
            title: key.title,
            public_key: key.public_key,
            fingerprint: key.fingerprint,
            read_only: key.read_only,
            created_by_id: key.created_by_id,
            created_at: key.created_at,
            last_used_at: key.last_used_at,
        }
    }
}

#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/keys",
    tag = "Repositories",
    params(("owner" = String, Path), ("name" = String, Path)),
    responses((status = 200, body = [DeployKeyResponse]), (status = 403, body = serde_json::Value))
)]
pub async fn list_deploy_keys(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match rg_db::ops::deploy_key_ops::list_by_repo(&state.db, repo.id).await {
        Ok(keys) => (
            StatusCode::OK,
            Json(
                keys.into_iter()
                    .map(DeployKeyResponse::from)
                    .collect::<Vec<_>>(),
            ),
        )
            .into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/keys",
    tag = "Repositories",
    request_body = CreateDeployKeyRequest,
    params(("owner" = String, Path), ("name" = String, Path)),
    responses(
        (status = 201, body = DeployKeyResponse),
        (status = 400, body = serde_json::Value),
        (status = 403, body = serde_json::Value),
        (status = 409, body = serde_json::Value)
    )
)]
pub async fn create_deploy_key(
    State(state): State<AppState>,
    Path((owner, _)): Path<(String, String)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<CreateDeployKeyRequest>,
) -> impl IntoResponse {
    let title = body.title.trim();
    if title.is_empty() || title.chars().count() > 100 {
        return AppError::bad_request("deploy key title must contain 1-100 characters")
            .into_response();
    }
    let public_key = body.public_key.trim();
    if public_key.len() > 16_384 {
        return AppError::bad_request("SSH public key is too large").into_response();
    }
    let fingerprint = match rg_core::auth::ssh_key::fingerprint_from_openssh(public_key) {
        Ok(fingerprint) => fingerprint,
        Err(error) => return AppError::bad_request(error).into_response(),
    };

    let duplicate_user_key =
        match rg_db::ops::ssh_key_ops::find_by_fingerprint(&state.db, &fingerprint).await {
            Ok(key) => key.is_some(),
            Err(error) => return AppError::from(error).into_response(),
        };
    let duplicate_deploy_key =
        match rg_db::ops::deploy_key_ops::find_by_fingerprint(&state.db, &fingerprint).await {
            Ok(key) => key.is_some(),
            Err(error) => return AppError::from(error).into_response(),
        };
    if duplicate_user_key || duplicate_deploy_key {
        return AppError::conflict("this SSH key is already registered").into_response();
    }

    // Before the key exists, per the rule in `access_audit`: a failed name
    // lookup afterwards would leave a live push credential whose author is
    // blank, and this way it is a 5xx from a request that granted nothing.
    let actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };

    let model = rg_db::entities::deploy_key::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: Set(repo.id),
        created_by_id: Set(Some(actor_id)),
        title: Set(title.to_string()),
        public_key: Set(public_key.to_string()),
        fingerprint: Set(fingerprint),
        read_only: Set(body.read_only),
        created_at: Set(chrono::Utc::now()),
        last_used_at: Set(None),
    };
    match rg_db::ops::deploy_key_ops::create(&state.db, model).await {
        Ok(key) => {
            record_grant(
                &state,
                &actor,
                "repo.add_deploy_key",
                &owner,
                &repo,
                &headers,
                deploy_key_details(&key),
            )
            .await;
            (StatusCode::CREATED, Json(DeployKeyResponse::from(key))).into_response()
        }
        // The fingerprint is unique; the checks above catch a key registered
        // earlier, so reaching here means a concurrent insert won the race.
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => {
            AppError::conflict("this SSH key is already registered").into_response()
        }
        Err(error) => AppError::from(error).into_response(),
    }
}

#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/keys/{id}",
    tag = "Repositories",
    params(("owner" = String, Path), ("name" = String, Path), ("id" = i64, Path)),
    responses((status = 204), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value))
)]
pub async fn delete_deploy_key(
    State(state): State<AppState>,
    Path((owner, _, id)): Path<(String, String, i64)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
) -> impl IntoResponse {
    let key = match deploy_key_in_repo(&state, repo.id, id).await {
        Ok(key) => key,
        Err(error) => return error.into_response(),
    };
    // The row is read before it is deleted for the same reason the collaborator
    // revocation reads its membership first: "deploy key #4 was revoked" tells a
    // review nothing about what stopped working.
    let revoked = deploy_key_details(&key);
    let actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    // The lookup above and this `DELETE` are two statements. A concurrent
    // revocation that lands in between leaves this one deleting nothing, and
    // "revoked" is the most expensive answer to get wrong — so the 204 comes
    // from `rows_affected`, not from the lookup that preceded it.
    match rg_db::ops::deploy_key_ops::delete_by_id(&state.db, key.id).await {
        Ok(true) => {
            record_grant(
                &state,
                &actor,
                "repo.remove_deploy_key",
                &owner,
                &repo,
                &headers,
                revoked,
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => AppError::not_found("deploy key not found").into_response(),
        Err(error) => AppError::from(error).into_response(),
    }
}

/// Fetch a deploy key and re-anchor it to the repository the caller was
/// authorized for.
///
/// `{id}` is a global `deploy_keys` primary key while `RepoAdmin` only ever
/// proves something about `{owner}/{name}`, so administering one repository must
/// not revoke another one's push credentials. A mismatch answers 404 rather than
/// 403: a 403 would still confirm the id exists, which is most of what an
/// id-walking caller wants to learn.
///
/// Only one route needs it today. It is a named helper rather than the inline
/// comparison it replaces because `global_id_anchor_guard` can read a call and
/// cannot read a comparison — so the second deploy-key route to be written is
/// held to this by the build instead of by review.
async fn deploy_key_in_repo(
    state: &AppState,
    repo_id: i64,
    key_id: i64,
) -> Result<rg_db::entities::deploy_key::Model, AppError> {
    match rg_db::ops::deploy_key_ops::find_by_id(&state.db, key_id).await {
        Ok(Some(key)) if key.repo_id == repo_id => Ok(key),
        Ok(_) => Err(AppError::not_found("deploy key not found")),
        Err(error) => Err(AppError::from(error)),
    }
}
