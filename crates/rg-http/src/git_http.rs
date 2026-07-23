//! Git Smart HTTP protocol endpoints (`/info/refs`, `git-upload-pack`,
//! `git-receive-pack`) plus post-push hooks (CI, webhooks, notifications) and
//! branch/tag protection enforcement.

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use sea_orm::DatabaseConnection;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::pat_auth::extract_actor_id;
use crate::{git_v2, ws, AppState};

/// Check repository access for git protocol.
///
/// - upload-pack (clone/fetch): can_read
/// - receive-pack (push): can_write
///
/// Returns Ok(()) if access is granted, or an error response.
///
/// When access is denied for an **anonymous** request (no valid credentials),
/// responds `401 Unauthorized` with a `WWW-Authenticate: Basic` challenge so
/// that `git` prompts for credentials (a username + PAT). An **authenticated**
/// actor that simply lacks permission gets `403 Forbidden`.
async fn check_git_access(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    actor_id: Option<i64>,
    require_write: bool,
) -> Result<(), (StatusCode, [(header::HeaderName, &'static str); 1], String)> {
    let access = if require_write {
        rg_core::repo::service::can_write(db, owner, repo_name, actor_id).await
    } else {
        rg_core::repo::service::can_read(db, owner, repo_name, actor_id).await
    };

    match access {
        Ok(true) => Ok(()),
        // Anonymous + denied → 401 with a Basic challenge so git prompts for
        // credentials. The String body carries the default text/plain type.
        Ok(false) if actor_id.is_none() => Err((
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"ForgeKeep\"")],
            "authentication required".to_string(),
        )),
        // Authenticated but lacking permission → 403.
        Ok(false) => Err((
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "text/plain")],
            "access denied".to_string(),
        )),
        Err(e) => Err((
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            format!("repository not found: {}", e),
        )),
    }
}

/// Git Smart HTTP `/info/refs` endpoint.
///
/// CRITICAL: Content-Type handling (pitfall #6)
///
/// The Git Smart HTTP protocol is VERY sensitive to Content-Type headers.
/// Incorrect Content-Type will cause `git` client to silently fail or
/// report "fatal: protocol error: bad line length character".
///
/// Correct Content-Types:
/// - info/refs response:
///   - upload-pack: `application/x-git-upload-pack-advertisement`
///   - receive-pack: `application/x-git-receive-pack-advertisement`
/// - request body (POST):
///   - upload-pack: `application/x-git-upload-pack-request`
///   - receive-pack: `application/x-git-receive-pack-request`
/// - response body (POST):
///   - upload-pack: `application/x-git-upload-pack-result`
///   - receive-pack: `application/x-git-receive-pack-result`
///
/// Common mistake: Using `text/plain` or wrong subtype will break git clients.
/// Always verify Content-Type matches the Git Smart HTTP spec exactly.
///
/// Strip `.git` suffix from repo name so both `owner/repo.git` and
/// `owner/repo` path formats resolve to the same bare repository.
fn strip_git_suffix(repo: &str) -> String {
    repo.strip_suffix(".git")
        .map(|s| s.to_string())
        .unwrap_or_else(|| repo.to_string())
}

pub(crate) async fn handle_info_refs(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> impl IntoResponse {
    // Strip .git suffix so both `owner/repo.git` and `owner/repo` work
    let repo = strip_git_suffix(&repo);
    let service = params.get("service").map(|s| s.as_str()).unwrap_or("");

    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            e.to_string(),
        );
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            e.to_string(),
        );
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "repository not found".to_string(),
        );
    }

    // Extract actor from auth header
    let actor_id = extract_actor_id(&state.db, &headers, &state.jwt_secret).await;
    let require_write = service == "git-receive-pack";

    // Check access
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, require_write).await {
        return resp;
    }

    // Check if client wants Protocol V2
    let wants_v2 = git_v2::wants_protocol_v2(&headers);

    match service {
        "git-upload-pack" | "git-receive-pack" => {
            if wants_v2 {
                // Protocol V2: send capability advertisement (refs sent via ls-refs command)
                return match build_v2_capability_advertisement() {
                    Ok(data) => {
                        let content_type = if service == "git-upload-pack" {
                            "application/x-git-upload-pack-advertisement"
                        } else {
                            "application/x-git-receive-pack-advertisement"
                        };
                        (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data)
                    }
                    Err(e) => (
                        StatusCode::INTERNAL_SERVER_ERROR,
                        [(header::CONTENT_TYPE, "text/plain")],
                        format!("error: {:#}", e),
                    ),
                };
            }

            // Protocol V1: send full ref advertisement
            let content_type = if service == "git-upload-pack" {
                "application/x-git-upload-pack-advertisement"
            } else {
                "application/x-git-receive-pack-advertisement"
            };

            match build_info_refs(&repo_path, service) {
                Ok(data) => (StatusCode::OK, [(header::CONTENT_TYPE, content_type)], data),
                Err(e) => (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain")],
                    format!("error: {:#}", e),
                ),
            }
        }
        _ => (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            "invalid or missing service parameter".to_string(),
        ),
    }
}

fn build_info_refs(repo_path: &std::path::Path, service: &str) -> Result<String> {
    let mut buf = String::new();
    // Pitfall #1: the "# service=" line must be wrapped in a pkt-line, not written raw
    let svc_line = format!("# service={}\n", service);
    buf.push_str(&format!("{:04x}", svc_line.len() + 4));
    buf.push_str(&svc_line);
    buf.push_str("0000");

    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    // Get all references (like git for-each-ref)
    let references = repo.references()?;
    let mut ref_list: Vec<(String, String)> = references
        .all()?
        .filter_map(|r| r.ok())
        .filter_map(|r| {
            let oid = r.target().try_id()?.to_owned();
            let name = String::from_utf8_lossy(r.name().as_bstr()).to_string();
            Some((oid.to_string(), name))
        })
        .collect();

    // Get HEAD SHA
    let head_sha = if let Ok(head) = repo.head() {
        head.try_into_referent() // Returns Option<Reference>
            .and_then(|r| r.target().try_id().map(|id| id.to_string()))
    } else {
        None
    };

    if let Some(sha) = &head_sha {
        ref_list.insert(0, (sha.clone(), "HEAD".to_string()));
    }

    let caps = if service == "git-upload-pack" {
        "multi_ack_detailed no-done side-band-64k thin-pack ofs-delta agent=forgekeep/0.1"
    } else {
        "report-status report-status-v2 side-band-64k agent=forgekeep/0.1"
    };

    if let Some((sha, refname)) = ref_list.first() {
        // First ref line carries capabilities after NUL separator
        // Format: <SHA> <refname>\0<capabilities>\n
        let payload = format!("{} {}\0{}\n", sha, refname, caps);
        let line = format!("{:04x}", payload.len() + 4);
        buf.push_str(&line);
        buf.push_str(&payload);
    } else {
        // Empty repository: send a dummy HEAD line with capabilities
        // Format: <null SHA> HEAD\0<capabilities>\n
        let null_sha = "0000000000000000000000000000000000000000";
        let payload = format!("{} HEAD\0{}\n", null_sha, caps);
        let line = format!("{:04x}", payload.len() + 4);
        buf.push_str(&line);
        buf.push_str(&payload);
    }

    for (sha, refname) in ref_list.iter().skip(1) {
        let payload = format!("{} {}\n", sha, refname);
        let line = format!("{:04x}", payload.len() + 4);
        buf.push_str(&line);
        buf.push_str(&payload);
    }

    buf.push_str("0000");
    Ok(buf)
}

/// Build Protocol V2 capability advertisement.
/// Format: version 2 + capabilities + flush
/// Manual pkt-line construction to avoid async in sync context.
fn build_v2_capability_advertisement() -> Result<String> {
    use std::io::Write;

    let mut buf = Vec::new();

    // Pitfall: Smart HTTP requires the "# service=" line to be wrapped in a pkt-line + flush
    let svc_line = "# service=git-upload-pack\n".to_string();
    let len = svc_line.len() + 4;
    write!(buf, "{:04x}", len)?;
    buf.extend_from_slice(svc_line.as_bytes());
    buf.extend_from_slice(b"0000");

    // Helper to write pkt-line data (pitfall: the pkt-line payload ends with \n,
    // length header = payload.len() + 4 (header) + 1 (\n); writeln! supplies the \n)
    let write_pkt = |buf: &mut Vec<u8>, text: &str| {
        let payload = text.as_bytes();
        let len = payload.len() + 4 + 1; // +4 for hex header, +1 for trailing \n
        writeln!(buf, "{:04x}{}", len, text)?;
        Ok::<(), std::io::Error>(())
    };

    // Protocol version line
    write_pkt(&mut buf, "version 2")?;
    for capability in rg_git::protocol::v2::ADVERTISED_CAPABILITIES {
        write_pkt(&mut buf, capability)?;
    }

    // Flush packet
    buf.extend_from_slice(b"0000");

    Ok(String::from_utf8(buf)?)
}

pub(crate) async fn handle_git_upload_pack(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    body: axum::body::Bytes,
) -> Response {
    // Strip .git suffix so both `owner/repo.git` and `owner/repo` work
    let repo = strip_git_suffix(&repo);
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(e.to_string()),
        )
            .into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(e.to_string()),
        )
            .into_response();
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
            Body::from("repository not found"),
        )
            .into_response();
    }

    // Check read access
    let actor_id = extract_actor_id(&state.db, &headers, &state.jwt_secret).await;
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, false).await {
        return (resp.0, resp.1, Body::from(resp.2)).into_response();
    }

    // Check if client wants Protocol V2
    let wants_v2 = git_v2::wants_protocol_v2(&headers);

    if wants_v2 {
        // Protocol V2: use V2 handler
        let (pipe_read, mut pipe_write) = tokio::io::duplex(body.len() + 1024);
        tokio::spawn(async move {
            let _ = pipe_write.write_all(&body).await;
        });

        let (buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
        // Spawn concurrent reader to prevent duplex deadlock when pack > 64KB
        let reader_task = tokio::spawn(async move {
            let mut buf_reader = buf_reader;
            let mut output = Vec::new();
            let _ = buf_reader.read_to_end(&mut output).await;
            output
        });

        match rg_git::protocol::v2::handle_v2_http(&repo_path, pipe_read, &mut buf_writer).await {
            Ok(()) => {
                let _ = buf_writer.flush().await;
                drop(buf_writer);
                let output = reader_task.await.unwrap_or_default();
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
                    Body::from(output),
                )
                    .into_response()
            }
            Err(e) => {
                drop(buf_writer);
                reader_task.abort();
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain")],
                    Body::from(format!("error: {:#}", e)),
                )
                    .into_response()
            }
        }
    } else {
        // Protocol V1: use V1 handler
        let (pipe_read, mut pipe_write) = tokio::io::duplex(body.len() + 1024);
        tokio::spawn(async move {
            let _ = pipe_write.write_all(&body).await;
        });

        let (buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
        // Spawn concurrent reader to prevent duplex deadlock when pack > 64KB
        let reader_task = tokio::spawn(async move {
            let mut buf_reader = buf_reader;
            let mut output = Vec::new();
            let _ = buf_reader.read_to_end(&mut output).await;
            output
        });

        match rg_git::protocol::upload_pack::handle_upload_pack_http(
            &repo_path,
            pipe_read,
            &mut buf_writer,
        )
        .await
        {
            Ok(()) => {
                let _ = buf_writer.flush().await;
                drop(buf_writer);
                let output = reader_task.await.unwrap_or_default();
                (
                    StatusCode::OK,
                    [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
                    Body::from(output),
                )
                    .into_response()
            }
            Err(e) => {
                drop(buf_writer);
                reader_task.abort();
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain")],
                    Body::from(format!("error: {:#}", e)),
                )
                    .into_response()
            }
        }
    }
}

pub(crate) async fn handle_git_receive_pack(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // Strip .git suffix so both `owner/repo.git` and `owner/repo` work
    let repo = strip_git_suffix(&repo);
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(e.to_string()),
        );
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(e.to_string()),
        );
    }

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            [(
                header::CONTENT_TYPE,
                "application/x-git-receive-pack-result",
            )],
            Body::from("repository not found"),
        );
    }

    // Check write access
    let actor_id = extract_actor_id(&state.db, &headers, &state.jwt_secret).await;
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, true).await {
        return (resp.0, resp.1, Body::from(resp.2));
    }

    let repo_model = match find_repo_by_name(&state.db, &owner, &repo).await {
        Ok(Some(repo_model)) => repo_model,
        Ok(None) => {
            return (
                StatusCode::NOT_FOUND,
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from("repository not found"),
            );
        }
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from(format!("failed to load repository: {:#}", e)),
            );
        }
    };
    let protection_rules =
        match rg_db::ops::protected_branch_ops::list_by_repo(&state.db, repo_model.id).await {
            Ok(rules) => rules,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(
                        header::CONTENT_TYPE,
                        "application/x-git-receive-pack-result",
                    )],
                    Body::from(format!("failed to load branch protections: {:#}", e)),
                );
            }
        };
    let tag_protection_rules =
        match rg_db::ops::protected_tag_ops::list_by_repo(&state.db, repo_model.id).await {
            Ok(rules) => rules,
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(
                        header::CONTENT_TYPE,
                        "application/x-git-receive-pack-result",
                    )],
                    Body::from(format!("failed to load tag protections: {:#}", e)),
                );
            }
        };

    let (pipe_read, mut pipe_write) = tokio::io::duplex(body.len() + 1024);
    tokio::spawn(async move {
        let _ = pipe_write.write_all(&body).await;
    });

    let (buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
    // Spawn concurrent reader to prevent duplex deadlock when response > 64KB
    let reader_task = tokio::spawn(async move {
        let mut buf_reader = buf_reader;
        let mut output = Vec::new();
        let _ = buf_reader.read_to_end(&mut output).await;
        output
    });

    let require_signed_refs = signed_commit_required_refs(&protection_rules);
    let mut rejected_refs = branch_protection_rejected_refs(protection_rules, actor_id);
    rejected_refs.extend(tag_protection_rejected_refs(tag_protection_rules, actor_id));
    match rg_git::protocol::receive_pack::handle_receive_pack_http_with_rejections(
        &repo_path,
        pipe_read,
        &mut buf_writer,
        rejected_refs,
        require_signed_refs,
    )
    .await
    {
        Ok(ref_updates) => {
            let _ = buf_writer.flush().await;
            drop(buf_writer);
            let output = reader_task.await.unwrap_or_default();

            // ── Post-push hooks: trigger CI + Webhook ───────────────
            let db = state.db.clone();
            let repo_path_clone = repo_path.clone();
            let repo_root = state.repo_root.clone();
            let owner_clone = owner.clone();
            let repo_clone = repo.clone();
            let docker_enabled = state.docker_enabled;
            let external_runners = state.external_runners;
            let allow_host_runner = state.allow_host_runner;
            let jwt_secret = state.jwt_secret.clone();
            let hub = state.notification_hub.clone();
            let smtp = state.smtp_config.clone();
            let ci_engine = state.ci_engine.clone();
            let external_url = state.external_url.clone();

            tokio::spawn(async move {
                post_push_hooks(
                    &PostPushParams {
                        db: &db,
                        repo_path: &repo_path_clone,
                        repo_root: &repo_root,
                        owner: &owner_clone,
                        repo_name: &repo_clone,
                        docker_enabled,
                        external_runners,
                        allow_host_runner,
                        jwt_secret: &jwt_secret,
                        notification_hub: &hub,
                        smtp_config: &smtp,
                        ci_engine: &*ci_engine,
                        external_url: external_url.as_deref(),
                    },
                    &ref_updates,
                )
                .await;
            });

            (
                StatusCode::OK,
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from(output),
            )
        }
        Err(e) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(format!("error: {:#}", e)),
        ),
    }
}

/// Parameters for post-push hooks.
struct PostPushParams<'a> {
    db: &'a DatabaseConnection,
    repo_path: &'a std::path::Path,
    repo_root: &'a std::path::Path,
    owner: &'a str,
    repo_name: &'a str,
    docker_enabled: bool,
    external_runners: bool,
    allow_host_runner: bool,
    jwt_secret: &'a str,
    notification_hub: &'a ws::NotificationHub,
    smtp_config: &'a Option<rg_core::email::SmtpConfig>,
    ci_engine: &'a dyn rg_core::ci::CiTrigger,
    external_url: Option<&'a str>,
}

fn branch_protection_rejected_refs(
    protections: Vec<rg_db::entities::protected_branch::Model>,
    actor_id: Option<i64>,
) -> Vec<(String, String)> {
    protections
        .into_iter()
        .filter_map(|protection| {
            if direct_push_allowed_by_rule(&protection, actor_id) {
                return None;
            }

            let message = if protection.require_pr {
                format!(
                    "push to protected branch '{}' is not allowed; open a pull request instead",
                    protection.branch_name
                )
            } else if !protection.allow_force_push {
                format!(
                    "force push to protected branch '{}' is not allowed",
                    protection.branch_name
                )
            } else {
                return None;
            };

            Some((format!("refs/heads/{}", protection.branch_name), message))
        })
        .collect()
}

fn direct_push_allowed_by_rule(
    protection: &rg_db::entities::protected_branch::Model,
    actor_id: Option<i64>,
) -> bool {
    if let Some(uid) = actor_id {
        if let Some(allowed_json) = &protection.allowed_push_user_ids {
            if let Ok(allowed_ids) = serde_json::from_str::<Vec<i64>>(allowed_json) {
                if allowed_ids.contains(&uid) {
                    return true;
                }
            }
        }
    }

    false
}

fn signed_commit_required_refs(
    protections: &[rg_db::entities::protected_branch::Model],
) -> Vec<String> {
    protections
        .iter()
        .filter(|rule| rule.require_signed_commits)
        .map(|rule| format!("refs/heads/{}", rule.branch_name))
        .collect()
}

fn tag_protection_rejected_refs(
    protections: Vec<rg_db::entities::protected_tag::Model>,
    actor_id: Option<i64>,
) -> Vec<(String, String)> {
    protections
        .into_iter()
        .filter_map(|protection| {
            let allowed = actor_id.is_some_and(|uid| {
                protection
                    .allowed_user_ids
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<Vec<i64>>(json).ok())
                    .is_some_and(|ids| ids.contains(&uid))
            });
            (!allowed).then(|| {
                (
                    format!("refs/tags/{}", protection.pattern),
                    format!(
                        "creation or update of protected tag pattern '{}' is not allowed",
                        protection.pattern
                    ),
                )
            })
        })
        .collect()
}

/// Section 0 of the post-push hook (branch updates only): refresh open-PR head
/// SHAs, run the auto-merge / merge-queue evaluations for the new commit, and
/// emit the protected-branch acceptance audit log.
async fn post_push_branch_maintenance(
    params: &PostPushParams<'_>,
    repo_id: i64,
    branch_name: &str,
    update: &rg_git::protocol::receive_pack::RefUpdate,
) {
    if !update.new_sha.chars().all(|character| character == '0') {
        match rg_db::ops::pull_request_ops::update_open_head_sha(
            params.db,
            repo_id,
            branch_name,
            &update.new_sha,
        )
        .await
        {
            Ok(_) => {
                if let Err(error) = rg_core::pull_request::try_auto_merges_for_head_commit(
                    params.db,
                    params.repo_root,
                    repo_id,
                    &update.new_sha,
                )
                .await
                {
                    tracing::warn!(%error, "auto-merge evaluation after push failed");
                }
                if let Err(error) =
                    rg_core::pull_request::merge_queue::process_for_head_commit_with_ci(
                        params.db,
                        params.repo_root,
                        repo_id,
                        &update.new_sha,
                        &rg_core::pull_request::merge_queue::MergeQueueCi {
                            trigger: params.ci_engine,
                            docker_enabled: params.docker_enabled,
                            external_runners: params.external_runners,
                            allow_host_runner: params.allow_host_runner,
                            jwt_secret: Some(params.jwt_secret),
                            external_url: params.external_url,
                        },
                    )
                    .await
                {
                    tracing::warn!(%error, "merge queue evaluation after push failed");
                }
            }
            Err(error) => {
                tracing::warn!(%error, "failed to refresh PR head SHA after push")
            }
        }
    }
    match rg_db::ops::protected_branch_ops::find_by_repo_and_branch(params.db, repo_id, branch_name)
        .await
    {
        Ok(Some(_protection)) => {
            tracing::info!(
                branch = %branch_name,
                "Post-push: protected branch update accepted by pre-receive rules"
            );
        }
        Err(e) => {
            tracing::warn!(error = %e, "Failed to check branch protection");
        }
        _ => {}
    }
}

/// Section 1 of the post-push hook: trigger a CI pipeline when a
/// `.forgekeep-ci.yml` is present at the pushed commit, then fan out the
/// real-time owner notification and the optional SMTP email.
async fn trigger_ci_for_push(
    params: &PostPushParams<'_>,
    repo_id: i64,
    repo_owner_id: i64,
    update: &rg_git::protocol::receive_pack::RefUpdate,
) {
    if !params.ci_engine.has_ci_config(params.repo_path, &update.new_sha) {
        return;
    }
    let pipeline_id = match params
        .ci_engine
        .trigger_pipeline(rg_core::ci::TriggerPipelineParams {
            db: params.db,
            repo_path: params.repo_path,
            repo_id,
            commit_sha: &update.new_sha,
            ref_name: &update.refname,
            trigger_type: "push",
            triggered_by: None,
            docker_enabled: params.docker_enabled,
            external_runners: params.external_runners,
            allow_host_runner: params.allow_host_runner,
            jwt_secret: Some(params.jwt_secret),
            external_url: params.external_url,
        })
        .await
    {
        Ok(pipeline_id) => pipeline_id,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to trigger CI pipeline");
            return;
        }
    };

    tracing::info!(pipeline_id, "CI pipeline triggered");

    // Push real-time notification to repo owner
    ws::push_notification(
        params.notification_hub,
        repo_owner_id,
        "ci_triggered",
        serde_json::json!({
            "pipeline_id": pipeline_id,
            "repo": format!("{}/{}", params.owner, params.repo_name),
            "ref": update.refname,
            "commit": update.new_sha,
        }),
    );

    // Send email notification if SMTP is configured
    if let Some(smtp) = params.smtp_config {
        if let Ok(Some(owner_user)) =
            rg_db::ops::user_ops::find_by_id(params.db, repo_owner_id).await
        {
            let subject = format!(
                "[ForgeKeep] CI pipeline #{} triggered for {}/{}",
                pipeline_id, params.owner, params.repo_name
            );
            let body = format!(
                "A CI pipeline has been triggered for repository {}/{} on branch {}.<br/><br/>Commit: {}<br/>Pipeline ID: {}",
                params.owner, params.repo_name, update.refname, update.new_sha, pipeline_id
            );
            if let Err(e) = rg_core::email::send_html_notification(
                smtp,
                &owner_user.email,
                &subject,
                &body,
                None,
            )
            .await
            {
                tracing::warn!(error = %e, "Failed to send CI notification email");
            }
        }
    }
}

/// Sections 2–3 of the post-push hook: fire the generic `push` webhook, the
/// branch/tag create/delete webhooks, and the real-time push notification.
async fn trigger_push_webhooks(
    params: &PostPushParams<'_>,
    repo_id: i64,
    repo_owner_id: i64,
    update: &rg_git::protocol::receive_pack::RefUpdate,
) {
    // 2. Trigger push webhook
    let payload = serde_json::json!({
        "ref": update.refname,
        "before": update.old_sha,
        "after": update.new_sha,
        "repository": {
            "owner": params.owner,
            "name": params.repo_name,
        },
    });

    if let Err(e) =
        rg_core::webhook::service::trigger_event(params.db, repo_id, "push", &payload).await
    {
        tracing::warn!(error = %e, "Failed to trigger push webhook");
    }

    // 3. Trigger branch/tag-specific webhooks
    if let Some(branch_name) = update.refname.strip_prefix("refs/heads/") {
        if update.old_sha.is_empty()
            || update.old_sha == "0000000000000000000000000000000000000000"
        {
            // New branch created
            if let Err(e) =
                rg_core::webhook::service::trigger_branch_created(params.db, repo_id, branch_name)
                    .await
            {
                tracing::warn!(
                    "Failed to trigger branch.created webhook for {}: {e}",
                    branch_name
                );
            }
        } else if update.new_sha.is_empty()
            || update.new_sha == "0000000000000000000000000000000000000000"
        {
            // Branch deleted
            if let Err(e) =
                rg_core::webhook::service::trigger_branch_deleted(params.db, repo_id, branch_name)
                    .await
            {
                tracing::warn!(
                    "Failed to trigger branch.deleted webhook for {}: {e}",
                    branch_name
                );
            }
        }
    } else if let Some(tag_name) = update.refname.strip_prefix("refs/tags/") {
        if update.old_sha.is_empty()
            || update.old_sha == "0000000000000000000000000000000000000000"
        {
            // New tag created
            if let Err(e) =
                rg_core::webhook::service::trigger_tag_created(params.db, repo_id, tag_name).await
            {
                tracing::warn!(
                    "Failed to trigger tag.created webhook for {}: {e}",
                    tag_name
                );
            }
        } else if update.new_sha.is_empty()
            || update.new_sha == "0000000000000000000000000000000000000000"
        {
            // Tag deleted
            if let Err(e) =
                rg_core::webhook::service::trigger_tag_deleted(params.db, repo_id, tag_name).await
            {
                tracing::warn!(
                    "Failed to trigger tag.deleted webhook for {}: {e}",
                    tag_name
                );
            }
        }
    }

    // Push real-time notification for push event
    ws::push_notification(
        params.notification_hub,
        repo_owner_id,
        "push",
        serde_json::json!({
            "repo": format!("{}/{}", params.owner, params.repo_name),
            "ref": update.refname,
            "commit": update.new_sha,
        }),
    );
}

/// Post-push hook: trigger CI pipeline and webhook for push events.
async fn post_push_hooks(
    params: &PostPushParams<'_>,
    ref_updates: &[rg_git::protocol::receive_pack::RefUpdate],
) {
    // Find repo_id from DB
    let repo_model = find_repo_by_name(params.db, params.owner, params.repo_name).await;

    let (repo_id, repo_owner_id) = match repo_model {
        Ok(Some(r)) => (r.id, r.owner_id),
        _ => {
            tracing::warn!(owner = %params.owner, repo = %params.repo_name, "Post-push: repo not found in DB, skipping hooks");
            return;
        }
    };

    for update in ref_updates {
        if update.status != "ok" {
            continue;
        }

        tracing::info!(
            refname = %update.refname,
            new_sha = %update.new_sha,
            "Post-push: triggering hooks"
        );

        // 0. PR head-SHA refresh + auto-merge/merge-queue + protected-branch audit
        if let Some(branch_name) = update.refname.strip_prefix("refs/heads/") {
            post_push_branch_maintenance(params, repo_id, branch_name, update).await;
        }

        // 1. Trigger CI pipeline if .forgekeep-ci.yml exists
        trigger_ci_for_push(params, repo_id, repo_owner_id, update).await;

        // 2-3. Push + branch/tag webhooks and the real-time notification
        trigger_push_webhooks(params, repo_id, repo_owner_id, update).await;
    }
}

/// Find a repository by owner name (user or org) and repo name (DB lookup).
async fn find_repo_by_name(
    db: &DatabaseConnection,
    owner: &str,
    name: &str,
) -> anyhow::Result<Option<rg_db::entities::repository::Model>> {
    rg_core::repo::service::find_repo_by_owner_name(db, owner, name).await
}
