//! Git LFS REST API endpoints.
//!
//! Implements the LFS batch API and object upload/download endpoints.

use anyhow::Context as _;
use axum::body::Body;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tokio::io::AsyncWriteExt;

use crate::api::repo_access;
use crate::error::AppError;
use crate::AppState;

/// Where LFS objects live and what has to be true about that directory. Shared
/// with the background compressor in `rg_core::lfs::service`, so a handler
/// failure and a maintenance-pass failure name the same directory the same way.
use rg_core::platform::fs::{discard_file_async, LFS_STORAGE_HINT};

#[derive(Debug, Default, Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct LfsActionQuery {
    expires: Option<i64>,
    signature: Option<String>,
    /// The account the URL was issued to, echoed back so the signature can be
    /// recomputed over it. It is covered by the HMAC, so neither editing nor
    /// dropping it produces a URL that verifies.
    actor: Option<i64>,
    /// The `users.session_version` the issuing session held, when the URL was
    /// issued to a session. Covered by the HMAC like `actor`, and echoed for
    /// the same reason — with the difference that this one is also the thing
    /// [`signer_still_stands`] compares against the account's current
    /// generation, which is how a revoked session stops being able to redeem
    /// what it minted.
    session: Option<i64>,
    /// `access_tokens.id`, when the URL was issued to a personal access token
    /// instead. Mutually exclusive with `session` and covered by the same HMAC,
    /// so a caller cannot swap one question for the other: a PAT-issued URL is
    /// checked against the PAT still existing, which is what revoking a PAT
    /// actually does (card_e4e177acd095).
    pat: Option<i64>,
    /// `ssh_keys.id`, when the URL was issued through `git-lfs-authenticate` to
    /// an account that signed in on the SSH port with that key. Mutually
    /// exclusive with `session` / `pat`; deleting the key revokes the URL.
    ssh_key: Option<i64>,
    /// `deploy_keys.id`, when the URL was issued through `git-lfs-authenticate`
    /// to a deploy key. Carried without `actor`: a deploy key is not an account.
    deploy_key: Option<i64>,
}

/// One actionable error for a filesystem failure on an LFS object path.
///
/// `lfs_root(repo_root, owner, repo)` builds the directory from the request but
/// never echoes it back, so a bare `io::Error` reaching the client is an errno
/// against a path only the server knows.
fn lfs_path_error(what: &str, path: &std::path::Path, error: &std::io::Error) -> AppError {
    AppError::internal(rg_core::platform::fs::describe_path_error(
        what,
        path,
        error,
        LFS_STORAGE_HINT,
    ))
}

/// Does the session a signed URL was issued under still stand?
///
/// This is [`crate::api::auth::session_standing_middleware`]'s question — both
/// halves of it — asked again here because the middleware cannot ask it: a
/// presigned URL carries no session for it to read. That is the entire point of
/// the shape, and the price is that the capability answers for itself for as
/// long as it lives. Deactivating an account therefore left up to six hours of
/// write access to a private repository standing, which is the same distance
/// between "revoked" and "revoked, eventually" that the middleware was built to
/// close.
///
/// Both halves, because the account outliving its session is the *normal* case
/// for the two acts a user performs on purpose: a password reset and a
/// `POST /users/logout` leave `is_usable()` true and bump
/// `users.session_version` instead. A check reading only the first was blind to
/// exactly the scenario the reset exists for — the owner changes their password
/// *because* the session was stolen, the thief's JWT dies at once, and the
/// upload URL that session already minted stayed write access to the private
/// repository for the rest of its six hours (card_c742da1794e4).
///
/// Fails closed and keeps the two answers apart: `401` for an account that is
/// gone or disabled or a session that has been revoked, `503` for a database
/// that could not be asked — a client is right to retry the second and wrong to
/// retry the first. The final owner read is a conditional no-op update rather
/// than a snapshot: it contends with account retirement/DELETE and returns the
/// fresh row from that ordering before this capability may publish success.
async fn signer_still_stands(
    state: &AppState,
    user_id: i64,
    credential: rg_core::lfs::service::LfsCredential,
) -> Result<(), AppError> {
    use rg_core::lfs::service::LfsCredential;

    // The credential's own question, and only that one. Asking a PAT-issued URL
    // about the owner's session generation revokes it on an event the token is
    // explicitly meant to survive, and leaves it standing through the one event
    // that means the token *was* revoked. An SSH key is the same kind of
    // standing credential as a PAT: deleting it is what revokes it.
    let token_stands = match credential {
        LfsCredential::Session { .. } => true,
        LfsCredential::SshKey { id } => {
            match rg_db::ops::ssh_key_ops::find_by_id(&state.db, id).await {
                Ok(key) => key.is_some_and(|key| key.user_id == user_id),
                Err(error) => {
                    tracing::error!(
                        user_id,
                        ssh_key_id = id,
                        error = %format!("{error:#}"),
                        "could not verify SSH key standing for an LFS credential"
                    );
                    return Err(AppError::service_unavailable(
                        "could not verify account standing",
                    ));
                }
            }
        }
        LfsCredential::Token { id } => {
            match rg_db::ops::token_ops::find_by_id(&state.db, id).await {
                Ok(Some(token)) => {
                    token.user_id == user_id
                        && token.expires_at.is_none_or(|at| at > chrono::Utc::now())
                }
                Ok(None) => false,
                Err(error) => {
                    tracing::error!(
                        user_id,
                        token_id = id,
                        error = %format!("{error:#}"),
                        "could not verify token standing for a signed LFS action URL"
                    );
                    return Err(AppError::service_unavailable(
                        "could not verify account standing",
                    ));
                }
            }
        }
    };

    if !token_stands {
        tracing::warn!(
            user_id,
            "rejecting signed LFS action URL: the credential that issued it was revoked"
        );
        return Err(AppError::unauthorized(
            "LFS action URL belongs to a disabled account or a revoked credential",
        ));
    }

    let account_stands =
        match rg_db::ops::user_ops::finalize_standing_credential_owner(&state.db, user_id).await {
            Ok(Some(user)) => user,
            Ok(None) => {
                tracing::warn!(
                user_id,
                "rejecting signed LFS action URL: the account it was issued to is retiring or gone"
            );
                return Err(AppError::unauthorized(
                    "LFS action URL belongs to a disabled account or a revoked credential",
                ));
            }
            Err(error) => {
                tracing::error!(
                    user_id,
                    error = %format!("{error:#}"),
                    "could not finalize account standing for a signed LFS action URL"
                );
                return Err(AppError::service_unavailable(
                    "could not verify account standing",
                ));
            }
        };

    let credential_stands = match credential {
        LfsCredential::Session { version } => account_stands.session_version == version,
        LfsCredential::Token { .. } | LfsCredential::SshKey { .. } => true,
    };

    if account_stands.is_usable() && credential_stands {
        return Ok(());
    }
    tracing::warn!(
        user_id,
        "rejecting signed LFS action URL: account is disabled, or the credential that issued it \
         was revoked"
    );
    Err(AppError::unauthorized(
        "LFS action URL belongs to a disabled account or a revoked credential",
    ))
}

/// Is this request carrying a signed action URL that is *still* good for what
/// it asks?
///
/// Returns `Ok(true)` when a signature was presented and honoured — the caller
/// then needs no further gate — and `Ok(false)` when none was presented at all,
/// which leaves the ordinary credential path to decide.
///
/// "Honoured" is three questions, not one, and the URL used to answer only the
/// first two. A signature proves the server issued this capability and that it
/// has not expired. [`signer_still_stands`] proves the account behind it is
/// still an account, and that the session it was minted under has not been
/// ended. Neither says anything about *access to this repository*,
/// and that is the thing a signed URL most obviously outlives: drop a
/// collaborator and their download URLs keep working for the rest of the hour
/// and their upload URLs for the rest of the six. The account is untouched
/// throughout, so the standing check is perfectly happy.
///
/// So the repository gate runs here too, against the actor the signature is
/// issued to — `query.actor`, which is covered by the HMAC and therefore not
/// something a caller can edit. It is the same pair the job-log WebSocket
/// settled on when it hit this exact shape (`sol_5d659e316c85`): standing *and*
/// a re-read of the repository, because the handshake's answer expires when the
/// repository flips to private or a collaborator is dropped.
///
/// That covers the anonymous issue as well, which used to be waved through with
/// the argument that "a public repository's read gate would admit this caller
/// with no credentials at all". True when the URL was minted; not true an hour
/// later, after the repository was made private. `check_read_for(…, None)`
/// answers `401` for exactly that case and keeps admitting the anonymous caller
/// while the repository really is public.
///
/// The cost is one permission read per object, which is what the unsigned
/// branch has always paid.
async fn authorize_signed_action(
    state: &AppState,
    repo_model: &rg_db::entities::repository::Model,
    oid: &str,
    action: rg_core::lfs::service::LfsActionKind,
    query: &LfsActionQuery,
) -> Result<bool, AppError> {
    match (&query.expires, &query.signature) {
        (None, None) => Ok(false),
        (Some(expires), Some(signature)) => {
            let signed_actor = signed_actor(query)?;
            match rg_core::lfs::service::verify_action_url(
                state.jwt_secret.as_bytes(),
                action,
                repo_model.id,
                oid,
                *expires,
                signed_actor,
                signature,
                chrono::Utc::now().timestamp(),
            ) {
                Ok(()) => {
                    actor_still_may(state, repo_model, action, signed_actor).await?;
                    Ok(true)
                }
                Err(rg_core::lfs::service::LfsActionSignatureError::Expired) => {
                    Err(AppError::gone("LFS action URL has expired"))
                }
                Err(rg_core::lfs::service::LfsActionSignatureError::Invalid) => {
                    Err(AppError::forbidden("invalid LFS action URL signature"))
                }
            }
        }
        _ => Err(AppError::forbidden("incomplete LFS action URL signature")),
    }
}

/// May the actor an LFS capability was issued to still do what it asks, on this
/// repository, right now?
///
/// One answer for every capability this file honours — a signed action URL and
/// the credential `git-lfs-authenticate` hands out on the SSH port — so the two
/// cannot drift: the account and the credential behind it must still stand
/// ([`signer_still_stands`]), and the repository gate must still admit the
/// account. A deploy key has no account; its row is re-read instead
/// ([`repo_access::check_deploy_key_for`]).
async fn actor_still_may(
    state: &AppState,
    repo_model: &rg_db::entities::repository::Model,
    action: rg_core::lfs::service::LfsActionKind,
    actor: Option<rg_core::lfs::service::LfsActor>,
) -> Result<(), AppError> {
    use rg_core::lfs::service::{LfsActionKind, LfsActor};

    let actor_id = match actor {
        Some(LfsActor::DeployKey { key_id }) => {
            return repo_access::check_deploy_key_for(
                state,
                repo_model,
                key_id,
                action == LfsActionKind::Upload,
            )
            .await;
        }
        Some(LfsActor::User {
            user_id,
            credential,
        }) => {
            signer_still_stands(state, user_id, credential).await?;
            Some(user_id)
        }
        None => None,
    };
    match action {
        LfsActionKind::Download => repo_access::check_read_for(state, repo_model, actor_id).await,
        LfsActionKind::Upload => repo_access::check_write_for(state, repo_model, actor_id).await,
    }
}

/// The credential `git-lfs-authenticate` minted on the SSH port, when this
/// request presents one.
///
/// `None` when the `Authorization` header is anything else, so every other
/// caller keeps the path it always had. A grant is honoured only for the
/// repository and the operation it was minted for — and, like a signed URL,
/// only while the actor behind it still may ([`actor_still_may`]): the SSH
/// gate's answer is minutes old by now, and a key deleted in between must not
/// keep working for the rest of the grant.
async fn ssh_grant_actor(
    state: &AppState,
    headers: &HeaderMap,
    repo_model: &rg_db::entities::repository::Model,
    operation: &str,
) -> Option<Result<rg_core::lfs::service::LfsActor, AppError>> {
    use rg_core::lfs::service::{
        verify_ssh_grant, LfsActionKind, LfsActionSignatureError, SSH_GRANT_AUTH_SCHEME,
    };

    let token = headers
        .get(axum::http::header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix(SSH_GRANT_AUTH_SCHEME)?
        .strip_prefix(' ')?;
    let grant = match verify_ssh_grant(
        state.jwt_secret.as_bytes(),
        token,
        chrono::Utc::now().timestamp(),
    ) {
        Ok(grant) => grant,
        Err(LfsActionSignatureError::Expired) => {
            return Some(Err(AppError::unauthorized(
                "the SSH LFS credential has expired; git-lfs fetches a new one over SSH",
            )))
        }
        Err(LfsActionSignatureError::Invalid) => {
            return Some(Err(AppError::unauthorized("invalid SSH LFS credential")))
        }
    };
    if grant.repo_id != repo_model.id
        || LfsActionKind::from_operation(operation) != Some(grant.action)
    {
        return Some(Err(AppError::forbidden(
            "the SSH LFS credential was issued for another repository or operation",
        )));
    }
    Some(
        actor_still_may(state, repo_model, grant.action, Some(grant.actor))
            .await
            .map(|()| grant.actor),
    )
}

/// The user behind this request's credentials, if any.
///
/// LFS answers "who is calling"; whether that caller may read or write the
/// repository is [`repo_access::check_read_for`] / [`check_write_for`]'s
/// decision, never this file's. An unauthenticated caller is `None` rather than
/// an early `401`: on a public repository a download needs no credentials at
/// all, and only the gate knows that.
fn actor_id(headers: &HeaderMap, state: &AppState) -> Option<i64> {
    crate::api::auth::extract_user_id(headers, &state.jwt_secret)
}

/// The caller a signed URL would be *issued* to: the account, plus the session
/// generation the credentials it is asking with were minted under.
///
/// The generation is bound into the signature so that redeeming the URL can ask
/// whether that session is still current. It is read from the presented JWT
/// rather than from the database on purpose: the capability inherits the
/// standing of the session that asked for it, and that session has already been
/// through `session_standing_middleware` on this very request.
fn issuing_actor(headers: &HeaderMap, state: &AppState) -> Option<rg_core::lfs::service::LfsActor> {
    crate::api::auth::extract_user_credential(headers, &state.jwt_secret).map(
        |(user_id, credential)| rg_core::lfs::service::LfsActor::User {
            user_id,
            credential,
        },
    )
}

/// The actor a presented URL claims to have been issued to.
///
/// `actor` and `session` are one value split across two query parameters, and
/// both are covered by the HMAC — so a half-present pair is not a URL this
/// server ever minted, and saying so plainly beats letting it fall through to a
/// signature mismatch. A URL signed before the generation was folded in
/// (`plombir-git-lfs-v2`) lands here too, which is the intended end for it.
fn signed_actor(
    query: &LfsActionQuery,
) -> Result<Option<rg_core::lfs::service::LfsActor>, AppError> {
    use rg_core::lfs::service::{LfsActor, LfsCredential};

    let user = |user_id, credential| {
        Ok(Some(LfsActor::User {
            user_id,
            credential,
        }))
    };
    match (
        query.actor,
        query.session,
        query.pat,
        query.ssh_key,
        query.deploy_key,
    ) {
        (None, None, None, None, None) => Ok(None),
        (Some(user_id), Some(version), None, None, None) => {
            user(user_id, LfsCredential::Session { version })
        }
        (Some(user_id), None, Some(id), None, None) => user(user_id, LfsCredential::Token { id }),
        (Some(user_id), None, None, Some(id), None) => user(user_id, LfsCredential::SshKey { id }),
        (None, None, None, None, Some(key_id)) => Ok(Some(LfsActor::DeployKey { key_id })),
        // Two credentials at once is not a shape this server mints either, and
        // it is the one a caller would try in order to pick which question gets
        // asked about their URL.
        _ => Err(AppError::forbidden("incomplete LFS action URL signature")),
    }
}

/// LFS batch API: POST /repos/:owner/:name/lfs/objects/batch
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/lfs/objects/batch",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 201, description = "Created", body = serde_json::Value),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn batch(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Json(req): Json<rg_core::lfs::service::LfsBatchRequest>,
) -> impl IntoResponse {
    // The outer maintenance layer cannot inspect this operation without
    // buffering an untrusted body before the normal body limits and access
    // gates. It therefore admits this shared POST endpoint; once Axum has
    // parsed the small JSON request, reject the write half here while allowing
    // the download half to continue.
    if req.operation == "upload"
        && state
            .instance_settings
            .get(&state.db)
            .await
            .maintenance_mode
    {
        return crate::middleware::maintenance_response();
    }

    // LFS client sends Accept: application/vnd.git-lfs+json
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &repo).await {
            Ok(Some(r)) => r,
            Ok(None) => return AppError::not_found("repository not found").into_response(),
            Err(e) => return AppError::from(e).into_response(),
        };

    // Upload actions always require repository write access, including for
    // public repositories; a download requires whatever reading the repository
    // requires. Both answers come from the shared gate: an anonymous caller on
    // a private repository used to be turned away by the *credential* helper
    // rather than by the gate, which happened to produce the same 401 without
    // anything guaranteeing it would.
    //
    // A clone made from the SSH address arrives with the credential
    // `git-lfs-authenticate` minted instead of a session or a token; it is
    // asked the same questions, and the URLs below are issued to it.
    let actor = match ssh_grant_actor(&state, &headers, &repo_model, &req.operation).await {
        Some(Ok(actor)) => Some(actor),
        Some(Err(error)) => return error.into_response(),
        None => {
            let actor = issuing_actor(&headers, &state);
            let actor_id = actor.and_then(rg_core::lfs::service::LfsActor::user_id);
            let decision = if req.operation == "upload" {
                repo_access::check_write_for(&state, &repo_model, actor_id).await
            } else {
                repo_access::check_read_for(&state, &repo_model, actor_id).await
            };
            if let Err(error) = decision {
                return error.into_response();
            }
            actor
        }
    };

    let repo_id = repo_model.id;
    let lfs_root = rg_core::lfs::service::lfs_root(&state.repo_root, &owner, &repo);

    // Prefer the configured public URL so signed actions retain HTTPS and the
    // externally visible host when Plombir Git runs behind a reverse proxy.
    let base_url = state
        .external_url
        .as_deref()
        .map(|url| url.trim_end_matches('/').to_string())
        .or_else(|| {
            headers
                .get("host")
                .and_then(|value| value.to_str().ok())
                .map(|host| format!("http://{host}"))
        })
        .unwrap_or_else(|| "http://localhost:8080".to_string());

    match rg_core::lfs::service::batch(
        &state.db,
        repo_id,
        state.blob_storage.as_ref(),
        &lfs_root,
        &base_url,
        &owner,
        &repo,
        &req,
        state.jwt_secret.as_bytes(),
        actor,
    )
    .await
    {
        Ok(resp) => (StatusCode::OK, Json(serde_json::json!(resp))).into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Upload an LFS object: PUT /repos/:owner/:name/lfs/objects/:oid
/// Streams the request body directly to a temp file, then stream-compresses
/// it with zstd — never buffers the entire object in memory.
#[utoipa::path(
    put,
    path = "/repos/{owner}/{name}/lfs/objects/{oid}",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("oid" = String, Path, description = "oid"),
        LfsActionQuery,
    ),
    request_body(content = serde_json::Value),
    responses(
        (status = 200, description = "Updated", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 413, description = "Object above the 10 GiB LFS request-body ceiling", body = serde_json::Value),
    ),
)]
pub async fn upload_object(
    State(state): State<AppState>,
    Path((owner, repo, oid)): Path<(String, String, String)>,
    Query(query): Query<LfsActionQuery>,
    headers: HeaderMap,
    body: Body,
) -> impl IntoResponse {
    if !rg_core::lfs::service::is_valid_oid(&oid) {
        return AppError::bad_request("invalid LFS object identifier").into_response();
    }
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &repo).await {
            Ok(Some(r)) => r,
            Ok(None) => return AppError::not_found("repository not found").into_response(),
            Err(e) => return AppError::from(e).into_response(),
        };

    let signed = match authorize_signed_action(
        &state,
        &repo_model,
        &oid,
        rg_core::lfs::service::LfsActionKind::Upload,
        &query,
    )
    .await
    {
        Ok(signed) => signed,
        Err(error) => return error.into_response(),
    };
    if !signed {
        let actor_id = actor_id(&headers, &state);
        if let Err(error) = repo_access::check_write_for(&state, &repo_model, actor_id).await {
            return error.into_response();
        }
    }

    let repo_id = repo_model.id;
    let lfs_root = rg_core::lfs::service::lfs_root(&state.repo_root, &owner, &repo);

    // Stream body to temp file. The directory creation used to be discarded
    // with `let _ =`, so an unwritable LFS root surfaced later as a failure to
    // *open* the temp file — pointing the operator at the file instead of at
    // the directory that actually has to be fixed.
    let temp_path = lfs_root.join(rg_core::staging::lfs_object_spool_name(&oid));
    if let Some(parent) = temp_path.parent() {
        if let Err(error) = tokio::fs::create_dir_all(parent).await {
            return lfs_path_error("LFS object directory", parent, &error).into_response();
        }
    }

    match write_body_to_file(
        body,
        &temp_path,
        rg_core::lfs::service::LFS_OBJECT_MAX_BYTES,
    )
    .await
    {
        Ok(staged) => {
            // An LFS object id is a SHA-256 of its uncompressed bytes, not just
            // a well-formed name chosen by the client. Hashing happens in the
            // streaming write above, before this path is handed to the
            // compressor or can become visible through its stable blob key.
            if staged.sha256 != oid {
                discard_file_async("LFS staging file", &temp_path).await;
                return AppError::bad_request(format!(
                    "LFS object content digest does not match oid: expected {oid}, got {}",
                    staged.sha256
                ))
                .into_response();
            }

            // `batch` registers the size the client promised before it gives
            // out this upload URL. Do not turn a short/long body into a live
            // object whose row says something else. Direct authenticated PUTs
            // have no prior row and keep the historical behavior of recording
            // the size that actually arrived.
            match rg_db::ops::lfs_object_ops::find_by_repo_and_oid(&state.db, repo_id, &oid).await {
                Ok(Some(object)) if object.size != staged.written as i64 => {
                    discard_file_async("LFS staging file", &temp_path).await;
                    return AppError::bad_request(format!(
                        "LFS object size does not match batch declaration: expected {} bytes, got {}",
                        object.size, staged.written
                    ))
                    .into_response();
                }
                Ok(_) => {}
                Err(error) => return AppError::from(error).into_response(),
            }

            match rg_core::lfs::service::store_object_from_file(
                &state.db,
                repo_id,
                state.blob_storage.as_ref(),
                &owner,
                &repo,
                &oid,
                &temp_path,
                staged.written as i64,
            )
            .await
            {
                Ok(()) => StatusCode::OK.into_response(),
                Err(e) => AppError::from(e).into_response(),
            }
        }
        // `write_body_to_file` retires a partial `.tmp_<oid>` before returning
        // this error; keep the typed cause until this HTTP boundary classifies
        // a declared ceiling separately from an ordinary transport failure.
        Err(e) => lfs_body_error(e).into_response(),
    }
}

/// Download an LFS object: GET /repos/:owner/:name/lfs/objects/:oid
/// Streams the object, decompressing on the fly if compressed.
/// For compressed objects, uses spawn_blocking + channel for streaming
/// zstd decompression without blocking the async runtime.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/lfs/objects/{oid}",
    tag = "LFS",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("oid" = String, Path, description = "oid"),
        LfsActionQuery,
    ),
    responses(
        (status = 200, description = "Success", content_type = "application/octet-stream"),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn download_object(
    State(state): State<AppState>,
    Path((owner, repo, oid)): Path<(String, String, String)>,
    Query(query): Query<LfsActionQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if !rg_core::lfs::service::is_valid_oid(&oid) {
        return AppError::bad_request("invalid LFS object identifier").into_response();
    }
    // H-01: Auth check for private repos
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, &owner, &repo).await {
            Ok(Some(r)) => r,
            Ok(None) => return AppError::not_found("repository not found").into_response(),
            Err(e) => return AppError::from(e).into_response(),
        };

    if let Err(response) = authorize_lfs_download(&state, &repo_model, &oid, &query, &headers).await
    {
        return response;
    }

    let lfs_root = rg_core::lfs::service::lfs_root(&state.repo_root, &owner, &repo);

    match rg_core::lfs::service::read_object_source(
        state.blob_storage.as_ref(),
        &lfs_root,
        &owner,
        &repo,
        &oid,
    )
    .await
    {
        Ok(rg_core::lfs::service::LfsObjectSource::Local {
            path: file_path,
            compressed: is_compressed,
        }) => {
            if is_compressed {
                stream_compressed_lfs_object(file_path).await
            } else {
                stream_uncompressed_lfs_object(&file_path).await
            }
        }
        Ok(rg_core::lfs::service::LfsObjectSource::Bytes { data, compressed }) => {
            respond_with_lfs_bytes(data, compressed, state.git_idle_timeout_secs)
        }
        Err(error) => lfs_object_read_error(error).into_response(),
    }
}

/// Classify a failed object read: a read that *proved* the object is gone is a
/// `404`, a read that could not check is ours.
///
/// The whole `Err` used to be one `404` carrying `e.to_string()` as the body,
/// which conflated the two halves of [`read_object_source`]: an unreachable
/// blob store told `git lfs pull` the objects had been deleted — a verdict no
/// client retries, sending the investigation to the repository instead of the
/// storage — and, because a `404` body is not sanitized (H-05), handed over the
/// storage path from the error text on the way out.
///
/// The service now carries genuine absence as `rg_core::error::NotFound`, which
/// [`AppError::from`] already answers `404` to. A backend `NotFound` means the
/// same thing one layer down: the object vanished between the `exists` check
/// and the read. Everything else falls through to a `5xx` whose detail reaches
/// the operator log rather than the client.
fn lfs_object_read_error(error: anyhow::Error) -> AppError {
    if matches!(
        error.downcast_ref::<rg_core::blob_storage::BlobStorageError>(),
        Some(rg_core::blob_storage::BlobStorageError::NotFound(_))
    ) {
        return AppError::not_found("LFS object not found");
    }
    AppError::from(error)
}

/// Enforce download authorization: either the request carries a signed action
/// URL that still stands — signature, account and repository access, all three
/// re-checked by [`authorize_signed_action`] — or the repository's own read
/// gate decides on the caller's own credentials.
async fn authorize_lfs_download(
    state: &AppState,
    repo_model: &rg_db::entities::repository::Model,
    oid: &str,
    query: &LfsActionQuery,
    headers: &HeaderMap,
) -> Result<(), axum::response::Response> {
    let signed = match authorize_signed_action(
        state,
        repo_model,
        oid,
        rg_core::lfs::service::LfsActionKind::Download,
        query,
    )
    .await
    {
        Ok(signed) => signed,
        Err(error) => return Err(error.into_response()),
    };
    if !signed {
        let actor_id = actor_id(headers, state);
        if let Err(error) = repo_access::check_read_for(state, repo_model, actor_id).await {
            return Err(error.into_response());
        }
    }
    Ok(())
}

/// Stream a zstd-compressed LFS object, decompressing on a blocking thread and
/// piping decoded chunks through a channel into the response body.
///
/// The file is opened *before* the response head is built. Opening it inside
/// the blocking task meant the `200` was already on the wire, so the only way
/// left to report the failure was an aborted body: `git lfs pull` saw a
/// truncated transfer and the operator saw nothing at all — no path, no errno.
/// A failure that is still reportable is now a 500 naming the file; the errors
/// that genuinely can only happen mid-stream are logged with the path before
/// they go into the body channel.
async fn stream_compressed_lfs_object(file_path: std::path::PathBuf) -> axum::response::Response {
    let file = match tokio::fs::File::open(&file_path).await {
        Ok(file) => file.into_std().await,
        Err(error) => {
            return lfs_path_error("LFS object file", &file_path, &error).into_response();
        }
    };

    // Stream-decompress via channel: spawn_blocking reads zstd chunks → channel → response body
    let (tx, rx) = tokio::sync::mpsc::channel::<std::io::Result<axum::body::Bytes>>(8);

    tokio::task::spawn_blocking(move || {
        use std::io::Read;
        // A body-stream error is logged by nobody: the client sees a truncated
        // transfer and hyper drops the cause, so this is the only place the
        // file can still be named.
        let aborted = |error: &std::io::Error| {
            let message = rg_core::platform::fs::describe_path_error(
                "LFS object file",
                &file_path,
                error,
                LFS_STORAGE_HINT,
            );
            tracing::error!(error = %message, "LFS object stream aborted");
            std::io::Error::other(message)
        };

        let decoder = match zstd::stream::Decoder::new(file) {
            Ok(d) => d,
            Err(error) => {
                if tx.blocking_send(Err(aborted(&error))).is_err() {
                    // Client disconnected before the stream error could be delivered.
                }
                return;
            }
        };
        let mut reader = std::io::BufReader::with_capacity(64 * 1024, decoder);
        let mut buf = vec![0u8; 64 * 1024];
        loop {
            match reader.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    if tx
                        .blocking_send(Ok(axum::body::Bytes::from(buf[..n].to_vec())))
                        .is_err()
                    {
                        break;
                    }
                }
                Err(error) => {
                    if tx.blocking_send(Err(aborted(&error))).is_err() {
                        // Client disconnected before the stream error could be delivered.
                    }
                    break;
                }
            }
        }
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let frame_stream = futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
    let stream_body = http_body_util::StreamBody::new(frame_stream);
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/octet-stream")],
        Body::new(stream_body),
    )
        .into_response()
}

/// Stream an uncompressed LFS object file directly from disk.
///
/// `read_object_source` already confirmed the object exists, so a failure here
/// is the storage under the server misbehaving — a stale handle, an unreadable
/// bind-mount, a file yanked between the check and the open. It stays a 500,
/// but it now names the file instead of handing `git lfs pull` a bare
/// "failed to open LFS object file" with the path and the errno both dropped.
async fn stream_uncompressed_lfs_object(file_path: &std::path::Path) -> axum::response::Response {
    let file = match tokio::fs::File::open(file_path).await {
        Ok(file) => file,
        Err(error) => return lfs_path_error("LFS object file", file_path, &error).into_response(),
    };
    // Size off the open handle: the second, *blocking* `std::fs::metadata`
    // this used to do sat on the async runtime and re-resolved a path that
    // could already have changed under it.
    let measured = file.metadata().await.map(|metadata| metadata.len());
    match uncompressed_lfs_response(file_path, file, measured) {
        Ok(response) => response,
        Err(error) => error.into_response(),
    }
}

/// Turn an opened LFS object and the measurement of it into the download.
///
/// Split from the open so that measuring can fail on its own terms. The size
/// used to be `file.metadata().await.map(|m| m.len()).unwrap_or(0)`: an `fstat`
/// that failed on a live handle — a yanked bind-mount, a stale NFS descriptor —
/// collapsed into a plausible `0`, and the handler answered `200` with
/// `Content-Length: 0` while `ReaderStream` fed the socket the object's real
/// bytes. Neither half of that reaches the client as a failure: one honouring
/// the header truncates the object to nothing and caches it as complete, one
/// ignoring it gets framing that contradicts the head, and the operator sees a
/// `200` either way.
///
/// Measuring is therefore part of what has to succeed *before* any byte is
/// promised — the same ordering `stream_compressed_lfs_object` needed for its
/// open (`sol_108bb6f1e901`): once the response head is out, a failure has no
/// channel left. A failed measurement stays a `500` naming the path and
/// carrying the original `io::Error`, and the body is never started.
fn uncompressed_lfs_response(
    file_path: &std::path::Path,
    file: tokio::fs::File,
    measured: std::io::Result<u64>,
) -> Result<axum::response::Response, AppError> {
    // On this arm `file` is dropped unread: the stream over it is built below,
    // only once the length it will be served under is known.
    let size = measured.map_err(|error| lfs_path_error("LFS object file", file_path, &error))?;
    let stream = tokio_util::io::ReaderStream::new(file);
    let frame_stream = futures::StreamExt::map(stream, |item| item.map(http_body::Frame::data));
    let stream_body = http_body_util::StreamBody::new(frame_stream);
    Ok((
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
            (
                axum::http::header::CONTENT_LENGTH,
                size.to_string().as_str(),
            ),
        ],
        Body::new(stream_body),
    )
        .into_response())
}

/// Build the response for an in-memory LFS object, decompressing if needed.
///
/// This is the **remote (non-local) blob-backend** branch: `read_object_source`
/// hands back the whole object as a `Vec` (the local-path branch above streams
/// from disk instead). LFS objects can be very large, so the finished buffer is
/// served as a backpressure-sensitive, idle-guarded stream rather than a single
/// in-memory frame a slow/stalled client can pin until the kernel resets the
/// dead connection (card_444e03f1ca15). `Content-Length` lets clients spot an
/// idle-aborted short read.
fn respond_with_lfs_bytes(
    data: Vec<u8>,
    compressed: bool,
    idle_secs: u64,
) -> axum::response::Response {
    let body = if compressed {
        match zstd::stream::decode_all(std::io::Cursor::new(data)) {
            Ok(decoded) => decoded,
            Err(error) => return AppError::internal(error).into_response(),
        }
    } else {
        data
    };
    let len = body.len();
    (
        StatusCode::OK,
        [
            (axum::http::header::CONTENT_TYPE, "application/octet-stream"),
            (axum::http::header::CONTENT_LENGTH, len.to_string().as_str()),
        ],
        crate::http_stream::buffered_body_with_idle(body, idle_secs),
    )
        .into_response()
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// A verified staging upload, measured and hashed while it is written.
#[derive(Debug)]
struct StagedLfsUpload {
    written: usize,
    sha256: String,
}

#[derive(Debug, thiserror::Error)]
#[error("LFS upload exceeds the configured {max_bytes}-byte limit")]
struct LfsUploadTooLarge {
    max_bytes: usize,
}

fn lfs_body_error(error: anyhow::Error) -> AppError {
    if error.downcast_ref::<LfsUploadTooLarge>().is_some()
        || crate::body_limit::is_length_limit_error(error.as_ref())
    {
        AppError::payload_too_large("LFS upload exceeds the configured request-body limit")
    } else {
        AppError::from(error)
    }
}

/// Stream an Axum `Body` to a file, returning its size and SHA-256.
///
/// This is the write path of every `git lfs push`: the staging path is derived
/// from `repo_root` plus the object id, so a bare `?` on the io error hands the
/// client an errno and nothing else. Every failure names the file.
async fn write_body_to_file(
    body: Body,
    path: &std::path::Path,
    max_bytes: usize,
) -> anyhow::Result<StagedLfsUpload> {
    let staged = |error: &std::io::Error| {
        rg_core::platform::fs::path_error("LFS staging file", path, error, LFS_STORAGE_HINT)
    };

    let mut file = tokio::fs::File::create(path)
        .await
        .map_err(|error| staged(&error))?;

    let result = async {
        use futures::StreamExt;
        let mut written: usize = 0;
        let mut hasher = Sha256::new();
        let mut stream = body.into_data_stream();
        while let Some(chunk) = stream.next().await {
            // Preserve the typed `axum::Error` as the source. Formatting it
            // here loses the nested `LengthLimitError` before the handler can
            // distinguish a declared ceiling from an ordinary I/O failure.
            let data = chunk
                .map_err(anyhow::Error::new)
                .context("body stream error")?;
            written = written
                .checked_add(data.len())
                .filter(|size| *size <= max_bytes)
                .ok_or_else(|| anyhow::Error::new(LfsUploadTooLarge { max_bytes }))?;
            file.write_all(&data)
                .await
                .map_err(|error| staged(&error))?;
            hasher.update(&data);
        }

        // `tokio::fs::File` buffers: `write_all` returns once the bytes are queued
        // for the blocking pool, not once they are in the file, and dropping the
        // handle does not wait for that queue either. The caller hands this path
        // straight to `store_object_from_file`, which opens it with `std::fs` and
        // compresses whatever is there — so under load the object that reaches
        // blob storage is the upload minus however much had not landed yet, while
        // `written` (counted here, in memory) says the whole thing arrived.
        //
        // Nothing downstream would notice. The row records `written` as the
        // object's size, the upload answers `200`, and LFS never hashes the bytes
        // against the `oid` they are filed under, so a truncated object is
        // indistinguishable from a good one until somebody clones. The same
        // missing `flush` cost the OCI registry a three-day flake, where it was at
        // least loud (`sol_07eb75f8fb62`).
        file.flush().await.map_err(|error| staged(&error))?;

        Ok(StagedLfsUpload {
            written,
            sha256: hex::encode(hasher.finalize()),
        })
    }
    .await;

    drop(file);
    if result.is_err() {
        discard_file_async("LFS staging file", path).await;
    }
    result
}

#[cfg(test)]
mod staging_path_tests {
    use super::*;
    use axum::body::Bytes;

    /// Every `git lfs push` stages its object at
    /// `<owner>.lfs/<repo>/.tmp_<oid>`. A bare `?` on the io error left the
    /// client and the log with an errno against a path only the server computes.
    #[tokio::test]
    async fn staging_failure_names_the_file_and_the_remedy() {
        let temp = tempfile::tempdir().unwrap();
        // A regular file where `<owner>.lfs/` belongs fails the open
        // deterministically, independent of the uid the tests run as.
        let blocker = temp.path().join("owner.lfs");
        std::fs::write(&blocker, "not a directory").unwrap();
        let staged = blocker.join("repo").join(".tmp_abc");

        let error = write_body_to_file(
            Body::from("payload"),
            &staged,
            rg_core::lfs::service::LFS_OBJECT_MAX_BYTES,
        )
        .await
        .expect_err("staging must fail when the LFS root is not a directory");
        let rendered = format!("{error:#}");

        assert!(
            rendered.contains(&staged.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("LFS staging file"), "{rendered}");
        assert!(rendered.contains("<owner>.lfs/<repo>/"), "{rendered}");
    }

    #[tokio::test]
    async fn chunked_transport_overflow_is_413_and_removes_the_partial_spool() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("partial-lfs-upload");
        let body = Body::from_stream(futures::stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"123")),
            Ok::<_, std::io::Error>(Bytes::from_static(b"45")),
        ]));
        let error = write_body_to_file(body, &path, 4).await.unwrap_err();
        let response = lfs_body_error(error).into_response();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert!(!path.exists(), "a refused LFS upload left a spool behind");
    }

    #[tokio::test]
    async fn ordinary_body_failure_stays_500_and_removes_the_partial_spool() {
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("failed-lfs-upload");
        let body = Body::from_stream(futures::stream::iter([
            Ok(Bytes::from_static(b"prefix")),
            Err(std::io::Error::other("connection reset")),
        ]));

        let error = write_body_to_file(body, &path, rg_core::lfs::service::LFS_OBJECT_MAX_BYTES)
            .await
            .unwrap_err();
        let response = lfs_body_error(error).into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert!(!path.exists(), "a failed LFS upload left a spool behind");
    }

    /// The upload handler used to discard the result of creating this
    /// directory, so an unwritable LFS root was reported one step later against
    /// the temp *file* — sending the operator after the wrong path.
    #[test]
    fn directory_failure_names_the_directory_not_the_object() {
        let temp = tempfile::tempdir().unwrap();
        let blocker = temp.path().join("owner.lfs");
        std::fs::write(&blocker, "not a directory").unwrap();
        let directory = blocker.join("repo");
        let error = std::fs::create_dir_all(&directory).unwrap_err();

        let AppError::InternalError(rendered) =
            lfs_path_error("LFS object directory", &directory, &error)
        else {
            panic!("a filesystem failure on the LFS root must stay a 500");
        };

        assert!(
            rendered.contains(&directory.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("LFS object directory"), "{rendered}");
        assert!(rendered.contains("<owner>.lfs/<repo>/"), "{rendered}");
    }

    /// `read_object_source` confirms the object exists before handing back a
    /// local path, so an open that still fails is the storage misbehaving —
    /// a 500, not a 404, and it has to survive as a *status* rather than as a
    /// truncated body.
    #[tokio::test]
    async fn a_compressed_object_that_cannot_be_opened_is_a_500_not_a_broken_200() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("owner.lfs").join("repo").join("abc.zst");

        let response = stream_compressed_lfs_object(missing).await;

        // Opening inside the blocking task committed the `200` first, leaving
        // an aborted body as the only channel for the failure.
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[tokio::test]
    async fn an_uncompressed_object_that_cannot_be_opened_stays_a_500() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("owner.lfs").join("repo").join("abc");

        let response = stream_uncompressed_lfs_object(&missing).await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    }

    /// An LFS object that opened fine, so that the only thing left to fail is
    /// measuring it.
    async fn opened_lfs_object(temp: &tempfile::TempDir) -> (std::path::PathBuf, tokio::fs::File) {
        let path = temp.path().join("owner.lfs").join("repo").join("abc");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, LFS_OBJECT_PAYLOAD).unwrap();
        let file = tokio::fs::File::open(&path).await.unwrap();
        (path, file)
    }

    const LFS_OBJECT_PAYLOAD: &[u8] = b"lfs-object-payload";

    /// Opening the object and measuring it are two failures, not one. The size
    /// was read as `…map(|m| m.len()).unwrap_or(0)`, so an `fstat` that failed
    /// on the live handle was answered with a plausible zero and the operator
    /// never learnt which path or which errno.
    #[tokio::test]
    async fn a_metadata_failure_on_an_open_object_names_the_path_and_keeps_the_errno() {
        let temp = tempfile::tempdir().unwrap();
        let (path, file) = opened_lfs_object(&temp).await;

        let refusal = uncompressed_lfs_response(
            &path,
            file,
            Err(std::io::Error::other("stale NFS file handle")),
        )
        .expect_err("a measurement that failed must not produce a download response");
        let AppError::InternalError(rendered) = refusal else {
            panic!(
                "failing to measure an open LFS object is the storage's fault, not the client's"
            );
        };

        assert!(rendered.contains(&path.display().to_string()), "{rendered}");
        assert!(rendered.contains("LFS object file"), "{rendered}");
        assert!(rendered.contains("stale NFS file handle"), "{rendered}");
    }

    /// The status was only half of it: `Content-Length: 0` was promised beside a
    /// `ReaderStream` over the real file, so the head and the body disagreed
    /// about the same object. The refusal has to be built *instead of* the
    /// stream, never beside it.
    #[tokio::test]
    async fn a_metadata_failure_never_starts_the_object_body() {
        let temp = tempfile::tempdir().unwrap();
        let (path, file) = opened_lfs_object(&temp).await;

        let response = uncompressed_lfs_response(
            &path,
            file,
            Err(std::io::Error::other("stale NFS file handle")),
        )
        .unwrap_or_else(|error| error.into_response());

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_ne!(
            response
                .headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some("0"),
            "the refusal still promises an empty object body"
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert!(
            !String::from_utf8_lossy(&body)
                .contains(&String::from_utf8_lossy(LFS_OBJECT_PAYLOAD).into_owned()),
            "the object's bytes started leaving after the measurement had already failed"
        );
    }

    /// The other half of the contract: an object that opens and measures fine
    /// still streams whole, under a length matching it byte for byte.
    #[tokio::test]
    async fn an_uncompressed_object_streams_whole_under_its_exact_length() {
        let temp = tempfile::tempdir().unwrap();
        let (path, file) = opened_lfs_object(&temp).await;
        drop(file);

        let response = stream_uncompressed_lfs_object(&path).await;

        assert_eq!(response.status(), StatusCode::OK);
        let expected = LFS_OBJECT_PAYLOAD.len().to_string();
        assert_eq!(
            response
                .headers()
                .get(axum::http::header::CONTENT_LENGTH)
                .and_then(|value| value.to_str().ok()),
            Some(expected.as_str())
        );
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .unwrap();
        assert_eq!(body.as_ref(), LFS_OBJECT_PAYLOAD);
    }
}
