//! Git LFS File Locking API: `git lfs lock`, `unlock`, `locks`, and the
//! `locks/verify` call the client makes before every push (card_e8afcaf3edf6).
//!
//! Mounted under `/repos/{owner}/{name}/lfs/locks`, which is also where the
//! client looks for it: it derives `<remote>.git/info/lfs/locks` from the clone
//! URL, and the endpoint-discovery rewrite in `routes.rs` sends that here. The
//! bodies are the protocol's own (`application/vnd.git-lfs+json`): a refusal
//! the client should show is `{"message": …}`, and a path that is already
//! locked is a `409` carrying the lock that holds it, which is what the client
//! names in "already locked by …".
//!
//! Who may: listing needs read access to the repository; locking, verifying and
//! unlocking need write access; unlocking another person's lock needs `force`
//! and administrative access. All three are the shared repository gates. A
//! clone made from the SSH address is admitted by the credential
//! `git-lfs-authenticate` minted for it (card_8062fa65ca75): an `upload` grant
//! for a write, either grant for a listing, and a deploy key can list but holds
//! no lock.

use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;
use serde::Deserialize;

use crate::api::repo_access;
use crate::error::AppError;
use crate::AppState;
use rg_core::lfs::locks::{self, CreateOutcome, LockFilter, UnlockOutcome};
use rg_core::lfs::service::LfsActor;

const LFS_MEDIA_TYPE: &str = "application/vnd.git-lfs+json";

#[derive(Debug, Deserialize)]
pub struct LockRef {
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateLockRequest {
    pub path: String,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<LockRef>,
}

#[derive(Debug, Default, Deserialize)]
pub struct VerifyLocksRequest {
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<u64>,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<LockRef>,
}

#[derive(Debug, Default, Deserialize)]
pub struct UnlockRequest {
    #[serde(default)]
    pub force: bool,
    #[serde(default, rename = "ref")]
    pub git_ref: Option<LockRef>,
}

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct ListLocksQuery {
    /// Only the lock on this path.
    pub path: Option<String>,
    /// Only the lock with this id.
    pub id: Option<String>,
    /// The `next_cursor` of the previous page.
    pub cursor: Option<String>,
    /// Page size (default 100, at most 1000).
    pub limit: Option<u64>,
    /// The ref the client is on. Accepted and ignored: a lock covers its path
    /// on every branch.
    pub refspec: Option<String>,
}

/// A body in the protocol's media type.
fn lfs_json(status: StatusCode, body: serde_json::Value) -> Response {
    let mut response = (status, Json(body)).into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(LFS_MEDIA_TYPE),
    );
    response
}

/// A refusal the client shows. The body carries the message twice: at the top,
/// where the git-lfs client reads it (`{"message": …}`), and in the API's own
/// envelope, where the web UI and the rejection normalizer in `error.rs` look
/// for it. `application/json` is a media type the client decodes errors from as
/// well. A server failure keeps the sanitized envelope `AppError` gives it, so
/// nothing internal reaches the message the client prints.
fn lfs_refusal(error: AppError) -> Response {
    let status = error.status();
    if !status.is_client_error() {
        return error.into_response();
    }
    let message = error.to_string();
    (
        status,
        Json(serde_json::json!({
            "message": message,
            "error": { "code": error.code(), "message": message, "request_id": null },
        })),
    )
        .into_response()
}

/// A refusal that also names the lock it is about — the `409` of a path that
/// is already locked, which the client turns into "already locked by …".
fn lock_refusal(error: AppError, lock: &rg_core::lfs::locks::LockView) -> Response {
    let message = error.to_string();
    let mut response = (
        error.status(),
        Json(serde_json::json!({
            "lock": lock,
            "message": message,
            "error": { "code": error.code(), "message": message, "request_id": null },
        })),
    )
        .into_response();
    response.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static(LFS_MEDIA_TYPE),
    );
    response
}

fn core_refusal(error: anyhow::Error) -> Response {
    lfs_refusal(AppError::from(error))
}

async fn repository(
    state: &AppState,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, Response> {
    match rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name).await {
        Ok(Some(repo)) => Ok(repo),
        Ok(None) => Err(lfs_refusal(AppError::not_found("repository not found"))),
        Err(error) => Err(core_refusal(error)),
    }
}

/// The actor behind the credential `git-lfs-authenticate` minted on the SSH
/// port, when the request presents one — already re-gated against the account
/// or deploy key behind it (`api::lfs::ssh_grant`). It opens a write only when
/// it was minted for `upload`; either grant opens a listing.
///
/// `Ok(None)` means the request carries a session, a PAT or nothing, and the
/// handler asks the repository gate itself.
async fn ssh_granted(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
    write: bool,
) -> Result<Option<rg_core::lfs::service::LfsActor>, Response> {
    use rg_core::lfs::service::LfsActionKind;
    match crate::api::lfs::ssh_grant(state, headers, repo, |action| {
        !write || action == LfsActionKind::Upload
    })
    .await
    {
        Some(Ok(actor)) => Ok(Some(actor)),
        Some(Err(error)) => Err(lfs_refusal(error)),
        None => Ok(None),
    }
}

/// Every lock has an owner, so a deploy key — even one that may push — can
/// neither take nor release one.
fn deploy_key_holds_no_lock() -> Response {
    lfs_refusal(AppError::forbidden(
        "a deploy key cannot hold an LFS lock; lock files with a user account",
    ))
}

fn authentication_required() -> Response {
    lfs_refusal(AppError::unauthorized("authentication required"))
}

/// Create a lock: POST /repos/:owner/:name/lfs/locks
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/lfs/locks",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value, description = "`{path, ref: {name}}`", content_type = "application/vnd.git-lfs+json"),
    responses(
        (status = 201, description = "The lock", body = serde_json::Value),
        (status = 400, description = "The path cannot be locked", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "No write access", body = serde_json::Value),
        (status = 409, description = "Already locked; carries the lock that holds the path", body = serde_json::Value),
    ),
)]
pub async fn create_lock(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<CreateLockRequest>,
) -> Response {
    let repo = match repository(&state, &owner, &name).await {
        Ok(repo) => repo,
        Err(response) => return response,
    };
    let user_id = match ssh_granted(&state, &headers, &repo, true).await {
        Err(response) => return response,
        Ok(Some(LfsActor::User { user_id, .. })) => user_id,
        Ok(Some(LfsActor::DeployKey { .. })) => return deploy_key_holds_no_lock(),
        Ok(None) => {
            let actor = crate::api::auth::extract_user_id(&headers, &state.jwt_secret);
            if let Err(error) = repo_access::check_write_for(&state, &repo, actor).await {
                return lfs_refusal(error);
            }
            match actor {
                Some(user_id) => user_id,
                None => return authentication_required(),
            }
        }
    };
    let ref_name = request.git_ref.as_ref().and_then(|r| r.name.as_deref());
    match locks::create_lock(&state.db, repo.id, user_id, &request.path, ref_name).await {
        Ok(CreateOutcome::Created(lock)) => {
            lfs_json(StatusCode::CREATED, serde_json::json!({ "lock": lock }))
        }
        Ok(CreateOutcome::AlreadyLocked(lock)) => lock_refusal(
            AppError::conflict(format!(
                "`{}` is already locked by {}",
                lock.path, lock.owner.name
            )),
            &lock,
        ),
        Err(error) => core_refusal(error),
    }
}

/// List locks: GET /repos/:owner/:name/lfs/locks
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/lfs/locks",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ListLocksQuery,
    ),
    responses(
        (status = 200, description = "`{locks, next_cursor}`", body = serde_json::Value),
        (status = 400, description = "Malformed cursor, id or limit", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "No read access", body = serde_json::Value),
    ),
)]
pub async fn list_locks(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    Query(query): Query<ListLocksQuery>,
    headers: HeaderMap,
) -> Response {
    let repo = match repository(&state, &owner, &name).await {
        Ok(repo) => repo,
        Err(response) => return response,
    };
    match ssh_granted(&state, &headers, &repo, false).await {
        Err(response) => return response,
        Ok(Some(_)) => {}
        Ok(None) => {
            let actor = crate::api::auth::extract_user_id(&headers, &state.jwt_secret);
            if let Err(error) = repo_access::check_read_for(&state, &repo, actor).await {
                return lfs_refusal(error);
            }
        }
    }
    let filter = LockFilter {
        path: query.path.as_deref(),
        id: query.id.as_deref(),
        cursor: query.cursor.as_deref(),
        limit: query.limit,
    };
    match locks::list_locks(&state.db, repo.id, filter).await {
        Ok(page) => lfs_json(
            StatusCode::OK,
            serde_json::json!({
                "locks": page.locks,
                "next_cursor": page.next_cursor.unwrap_or_default(),
            }),
        ),
        Err(error) => core_refusal(error),
    }
}

/// Verify locks before a push: POST /repos/:owner/:name/lfs/locks/verify
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/lfs/locks/verify",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value, description = "`{cursor, limit, ref: {name}}`", content_type = "application/vnd.git-lfs+json"),
    responses(
        (status = 200, description = "`{ours, theirs, next_cursor}`", body = serde_json::Value),
        (status = 400, description = "Malformed cursor or limit", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "No write access", body = serde_json::Value),
    ),
)]
pub async fn verify_locks(
    State(state): State<AppState>,
    Path((owner, name)): Path<(String, String)>,
    headers: HeaderMap,
    Json(request): Json<VerifyLocksRequest>,
) -> Response {
    let repo = match repository(&state, &owner, &name).await {
        Ok(repo) => repo,
        Err(response) => return response,
    };
    // A deploy key that may push asks too; it holds no lock, so every lock is
    // someone else's.
    let holder = match ssh_granted(&state, &headers, &repo, true).await {
        Err(response) => return response,
        Ok(Some(LfsActor::User { user_id, .. })) => Some(user_id),
        Ok(Some(LfsActor::DeployKey { .. })) => None,
        Ok(None) => {
            let actor = crate::api::auth::extract_user_id(&headers, &state.jwt_secret);
            if let Err(error) = repo_access::check_write_for(&state, &repo, actor).await {
                return lfs_refusal(error);
            }
            match actor {
                Some(user_id) => Some(user_id),
                None => return authentication_required(),
            }
        }
    };
    match locks::verify_locks(
        &state.db,
        repo.id,
        holder,
        request.cursor.as_deref(),
        request.limit,
    )
    .await
    {
        Ok(page) => lfs_json(
            StatusCode::OK,
            serde_json::json!({
                "ours": page.ours,
                "theirs": page.theirs,
                "next_cursor": page.next_cursor.unwrap_or_default(),
            }),
        ),
        Err(error) => core_refusal(error),
    }
}

/// Remove a lock: POST /repos/:owner/:name/lfs/locks/:id/unlock
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/lfs/locks/{id}/unlock",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("id" = String, Path, description = "lock id"),
    ),
    request_body(content = serde_json::Value, description = "`{force, ref: {name}}`", content_type = "application/vnd.git-lfs+json"),
    responses(
        (status = 200, description = "The lock that was removed", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "No write access, someone else's lock without `force`, or `force` without admin access", body = serde_json::Value),
        (status = 404, description = "No such lock", body = serde_json::Value),
    ),
)]
pub async fn unlock(
    State(state): State<AppState>,
    Path((owner, name, id)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(request): Json<UnlockRequest>,
) -> Response {
    let repo = match repository(&state, &owner, &name).await {
        Ok(repo) => repo,
        Err(response) => return response,
    };
    let user_id = match ssh_granted(&state, &headers, &repo, true).await {
        Err(response) => return response,
        Ok(Some(LfsActor::User { user_id, .. })) => user_id,
        Ok(Some(LfsActor::DeployKey { .. })) => return deploy_key_holds_no_lock(),
        Ok(None) => {
            let actor = crate::api::auth::extract_user_id(&headers, &state.jwt_secret);
            if let Err(error) = repo_access::check_write_for(&state, &repo, actor).await {
                return lfs_refusal(error);
            }
            match actor {
                Some(user_id) => user_id,
                None => return authentication_required(),
            }
        }
    };
    // `force` is an administrator's word whether or not the lock turns out to
    // be someone else's: a writer saying it is told no, not quietly obliged
    // when the lock happens to be their own.
    if request.force {
        if let Err(error) = repo_access::check_admin_for(&state, &repo, Some(user_id)).await {
            return lfs_refusal(error);
        }
    }
    let id = match locks::parse_lock_id(&id, "id") {
        Ok(id) => id,
        Err(_) => return lfs_refusal(AppError::not_found("lock not found")),
    };
    match locks::unlock(&state.db, repo.id, user_id, id, request.force).await {
        Ok(UnlockOutcome::Unlocked(lock)) => {
            lfs_json(StatusCode::OK, serde_json::json!({ "lock": lock }))
        }
        Ok(UnlockOutcome::NotFound) => lfs_refusal(AppError::not_found("lock not found")),
        Ok(UnlockOutcome::OwnedByAnother(lock)) => lock_refusal(
            AppError::forbidden(format!(
                "`{}` is locked by {}; only a repository administrator can unlock it, with --force",
                lock.path, lock.owner.name
            )),
            &lock,
        ),
        Err(error) => core_refusal(error),
    }
}
