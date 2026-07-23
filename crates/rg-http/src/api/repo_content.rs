//! REST API handlers for repository content browsing (tree, blob, history).

use anyhow::Context;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::Json;
use chrono;
use serde::{Deserialize, Serialize};

use crate::api::auth::{extract_bearer_claims, extract_ci_job_claims, extract_user_id};
use crate::error::AppError;
use crate::AppState;

// ── Request / Response types ──────────────────────────────────────────

#[derive(Deserialize)]
pub struct TreeQuery {
    /// Git ref (branch, tag, commit SHA). Default: HEAD
    #[serde(default)]
    pub r#ref: Option<String>,
    /// Sub-path within the tree. Default: root
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Deserialize)]
pub struct BlobQuery {
    #[serde(default)]
    pub r#ref: Option<String>,
}

#[derive(Deserialize)]
pub struct LogQuery {
    #[serde(default)]
    pub r#ref: Option<String>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub limit: Option<i64>,
}

/// Request body for creating/updating a file.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct CreateOrUpdateFileRequest {
    /// Branch name (default: repo's default branch)
    #[serde(default)]
    pub branch: Option<String>,
    /// File content (UTF-8 string, not base64)
    pub content: String,
    /// Commit message
    pub message: String,
    /// Blob SHA of the file being updated (required for updates, omit for creates)
    #[serde(default)]
    pub sha: Option<String>,
}

/// Query parameters for deleting a file.
#[derive(Deserialize, utoipa::ToSchema)]
pub struct DeleteFileQuery {
    /// Branch name (default: repo's default branch)
    #[serde(default)]
    pub branch: Option<String>,
    /// Commit message
    pub message: String,
    /// Blob SHA of the file (required to prevent accidental deletes)
    pub sha: String,
}

/// Response for file creation/update/deletion.
#[derive(Serialize, Deserialize, utoipa::ToSchema)]
pub struct FileOperationResponse {
    pub success: bool,
    pub file_path: String,
    pub commit_sha: String,
    pub message: String,
}

#[derive(Serialize)]
pub struct TreeEntry {
    pub name: String,
    pub path: String,
    pub kind: String, // "tree" | "blob"
    pub size: Option<i64>,
    pub sha: Option<String>,
}

#[derive(Serialize)]
pub struct BlobContent {
    pub path: String,
    pub sha: String,
    pub size: i64,
    pub content: String,
    pub encoding: String, // "utf-8" | "base64"
    pub is_binary: bool,
}

#[derive(Serialize)]
pub struct CommitEntry {
    pub sha: String,
    #[serde(rename = "author")]
    pub author_name: String,
    #[serde(skip_serializing)]
    pub author_email: String,
    #[serde(rename = "date")]
    pub author_date: String,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gpg_signature: Option<GpgSignature>,
}

/// GPG signature information for a commit.
#[derive(Serialize)]
pub struct GpgSignature {
    pub verified: bool,
    pub signer_key: Option<String>,
    pub signer_name: Option<String>,
    pub signer_email: Option<String>,
    pub status: String,
}

// ── Handlers ──────────────────────────────────────────────────────────

/// Resolve a repo by owner/name and enforce read access.
/// Returns the repo model. Public repos are always accessible;
/// private repos require a valid JWT and the user must have read permission.
async fn resolve_and_check_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    let repo_model = rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, repo)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))?;

    let actor_id = extract_user_id(headers, &state.jwt_secret);
    match rg_core::repo::service::can_read_repo(&state.db, &repo_model, actor_id).await {
        Ok(true) => {}
        Ok(false)
            if actor_id.is_none()
                && extract_ci_job_claims(
                    headers,
                    &state.jwt_secret,
                    repo_model.id,
                    "repo:read",
                )
                .is_some() => {}
        Ok(false) if repo_model.is_private && actor_id.is_none() => {
            return Err(AppError::unauthorized("authentication required"));
        }
        Ok(false) => {
            return Err(AppError::forbidden("access denied"));
        }
        Err(e) => return Err(AppError::internal(e)),
    }

    Ok(repo_model)
}

// ── Individual handlers ─────────────────────────────────────────────────

/// List tree entries (directory listing) for a repo.
/// GET /api/v1/repos/:owner/:name/tree
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/tree",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_tree(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Query(params): Query<TreeQuery>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos
    let _repo = match resolve_and_check_access(&state, &headers, &owner, &repo).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if !repo_path.exists() {
        return AppError::not_found("repository not found").into_response();
    }

    let git_ref = params.r#ref.unwrap_or_else(|| "HEAD".to_string());
    let sub_path = params.path.unwrap_or_default();

    let result = list_tree_entries(&repo_path, &git_ref, &sub_path);

    match result {
        Ok(entries) => (
            StatusCode::OK,
            Json(serde_json::json!({ "entries": entries })),
        )
            .into_response(),
        Err(e) => {
            // A freshly-created repo with no commits has an unborn HEAD, which
            // can't be resolved to a tree. That's not an error — return an
            // empty tree so the UI can render the empty-repo state.
            if is_empty_repo(&repo_path) {
                return (StatusCode::OK, Json(serde_json::json!({ "entries": [] })))
                    .into_response();
            }
            tracing::error!(%e, "list_tree failed");
            AppError::internal(e).into_response()
        }
    }
}

/// Returns true if the repository has no commits yet (unborn HEAD), e.g. a
/// repo that was just created but never pushed to.
fn is_empty_repo(repo_path: &std::path::Path) -> bool {
    match gix::open(repo_path) {
        Ok(repo) => match repo.head() {
            // `Head::id()` is None when HEAD points at a branch that doesn't
            // exist yet (no commits).
            Ok(head) => head.id().is_none(),
            Err(_) => true,
        },
        Err(_) => false,
    }
}

/// Get blob (file) content.
/// GET /api/v1/repos/:owner/:name/blob/:path
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/blob/{*path}",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_blob(
    State(state): State<AppState>,
    Path((owner, repo, path)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(params): Query<BlobQuery>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos
    let _repo = match resolve_and_check_access(&state, &headers, &owner, &repo).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if !repo_path.exists() {
        return AppError::not_found("repository not found").into_response();
    }

    let git_ref = params.r#ref.unwrap_or_else(|| "HEAD".to_string());

    match get_blob_content(&repo_path, &git_ref, &path) {
        Ok(blob) => (StatusCode::OK, Json(blob)).into_response(),
        Err(e) => AppError::not_found(e).into_response(),
    }
}

/// Get commit log for a repo or a specific file.
/// GET /api/v1/repos/:owner/:name/log
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/log",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("ref" = Option<String>, Query, description = "Git ref (branch/tag/sha, default: HEAD)"),
        ("path" = Option<String>, Query, description = "File path filter"),
        ("limit" = Option<i64>, Query, description = "Max number of commits"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_log(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    Query(params): Query<LogQuery>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos
    let _repo = match resolve_and_check_access(&state, &headers, &owner, &repo).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if !repo_path.exists() {
        return AppError::not_found("repository not found").into_response();
    }

    let git_ref = params.r#ref.clone().unwrap_or_else(|| "HEAD".to_string());
    let limit = params.limit.unwrap_or(50).min(100);
    let file_path = params.path.unwrap_or_default();

    match get_commit_log(&repo_path, &git_ref, &file_path, limit) {
        Ok(log) => (StatusCode::OK, Json(serde_json::json!({ "commits": log }))).into_response(),
        Err(e) => {
            tracing::error!(%e, "get_log failed");
            AppError::internal(e).into_response()
        }
    }
}

/// List branches.
/// GET /api/v1/repos/:owner/:name/branches
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/branches",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_branches(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos
    let _repo = match resolve_and_check_access(&state, &headers, &owner, &repo).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if !repo_path.exists() {
        return AppError::not_found("repository not found").into_response();
    }

    match list_branch_names(&repo_path) {
        Ok(branches) => (StatusCode::OK, Json(branches)).into_response(),
        Err(e) => {
            tracing::error!(%e, "list_branches failed");
            AppError::internal(e).into_response()
        }
    }
}

/// List tags.
/// GET /api/v1/repos/:owner/:name/tags
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/tags",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn list_tags(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos
    let _repo = match resolve_and_check_access(&state, &headers, &owner, &repo).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if !repo_path.exists() {
        return AppError::not_found("repository not found").into_response();
    }

    match list_tag_names(&repo_path) {
        Ok(tags) => (StatusCode::OK, Json(tags)).into_response(),
        Err(e) => {
            tracing::error!(%e, "list_tags failed");
            AppError::internal(e).into_response()
        }
    }
}

// ── Git CLI helpers ───────────────────────────────────────────────────

fn list_tree_entries(
    repo_path: &std::path::Path,
    git_ref: &str,
    sub_path: &str,
) -> anyhow::Result<Vec<TreeEntry>> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    // Resolve ref to commit
    let commit_id = repo
        .rev_parse_single(git_ref)
        .map_err(|e| anyhow::anyhow!("failed to resolve ref '{}': {}", git_ref, e))?;

    let commit = repo
        .find_commit(commit_id)
        .map_err(|e| anyhow::anyhow!("failed to find commit: {}", e))?;

    let decoded = commit
        .decode()
        .map_err(|e| anyhow::anyhow!("failed to decode commit: {}", e))?;

    let tree_oid = decoded.tree();
    let mut tree = repo
        .find_tree(tree_oid)
        .map_err(|e| anyhow::anyhow!("failed to get tree: {}", e))?;

    // Traverse into sub_path if specified
    if !sub_path.is_empty() {
        for component in sub_path.split('/') {
            let entry = tree
                .iter()
                .filter_map(|e| e.ok())
                .find(|e| e.filename() == component);
            let entry = entry.ok_or_else(|| anyhow::anyhow!("path not found: {}", sub_path))?;
            tree = repo
                .find_tree(entry.oid())
                .map_err(|e| anyhow::anyhow!("failed to find sub-tree: {}", e))?;
        }
    }

    let mut entries = Vec::new();
    for entry in tree.iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(_) => continue,
        };
        let oid = entry.oid();
        let name = entry.filename().to_string();
        let kind = if entry.mode().is_tree() {
            "tree".to_string()
        } else {
            "blob".to_string()
        };

        let size = if kind == "blob" {
            get_blob_size(repo_path, &oid.to_string()).ok()
        } else {
            None
        };

        let full_path = if sub_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", sub_path, name)
        };

        entries.push(TreeEntry {
            name,
            path: full_path,
            kind,
            size,
            sha: Some(oid.to_string()),
        });
    }

    Ok(entries)
}

fn get_blob_content(
    repo_path: &std::path::Path,
    git_ref: &str,
    path: &str,
) -> anyhow::Result<BlobContent> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let target = format!("{}:{}", git_ref, path);

    // Resolve ref:path to object ID
    let object_id = repo
        .rev_parse_single(target.as_str())
        .map_err(|e| anyhow::anyhow!("path '{}' not found at ref '{}': {}", path, git_ref, e))?;

    // Find and decode the blob
    let object = repo
        .find_object(object_id)
        .map_err(|e| anyhow::anyhow!("failed to find object: {}", e))?;

    let blob = object
        .try_into_blob()
        .map_err(|e| anyhow::anyhow!("path '{}' is not a file: {}", path, e))?;

    let data = blob.data.as_slice();
    let size = data.len() as i64;

    // Check if binary by looking for null bytes
    let is_binary = data.contains(&0);

    let (content, encoding) = if is_binary {
        use std::fmt::Write;
        let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
        // Simple base64 encoding
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let chunks = data.chunks(3);
        for chunk in chunks {
            let b0 = chunk[0] as u32;
            let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
            let triple = (b0 << 16) | (b1 << 8) | b2;
            let _ = write!(s, "{}", ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
            let _ = write!(s, "{}", ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                let _ = write!(s, "{}", ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
            } else {
                let _ = write!(s, "=");
            }
            if chunk.len() > 2 {
                let _ = write!(s, "{}", ALPHABET[(triple & 0x3F) as usize] as char);
            } else {
                let _ = write!(s, "=");
            }
        }
        (s, "base64".to_string())
    } else {
        (
            String::from_utf8_lossy(data).to_string(),
            "utf-8".to_string(),
        )
    };

    Ok(BlobContent {
        path: path.to_string(),
        sha: object_id.to_string(),
        size,
        content,
        encoding,
        is_binary,
    })
}

fn get_blob_size(repo_path: &std::path::Path, sha: &str) -> anyhow::Result<i64> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let oid = gix::ObjectId::from_hex(sha.as_bytes())
        .map_err(|e| anyhow::anyhow!("invalid SHA: {}", e))?;

    let object = repo
        .find_object(oid)
        .map_err(|e| anyhow::anyhow!("object not found: {}", e))?;

    let blob = object
        .try_into_blob()
        .map_err(|e| anyhow::anyhow!("not a blob: {}", e))?;

    Ok(blob.data.len() as i64)
}

fn get_commit_log(
    repo_path: &std::path::Path,
    git_ref: &str,
    _path: &str,
    limit: i64,
) -> anyhow::Result<Vec<CommitEntry>> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let mut entries = Vec::new();

    // Use rev_walk to traverse commit history
    let head_id = match repo.rev_parse_single(git_ref) {
        Ok(id) => id,
        Err(_) => return Ok(entries), // No commits yet
    };

    let walk = repo.rev_walk([head_id]);

    let mut count = 0;
    // Call all() to get the iterator
    if let Ok(walk_iter) = walk.all() {
        for info in walk_iter {
            if count >= limit {
                break;
            }

            let info = match info {
                Ok(i) => i,
                Err(_) => continue,
            };

            let commit_id = info.id;

            let object = match repo.find_object(commit_id) {
                Ok(obj) => obj,
                Err(_) => continue,
            };

            let commit = match object.try_into_commit() {
                Ok(c) => c,
                Err(_) => continue,
            };

            // Get commit message
            let message = commit.message_raw().unwrap_or_default().to_string();
            let first_line = message.lines().next().unwrap_or("").to_string();

            // Get author info
            let author = commit.author().unwrap_or_default();
            let author_name = String::from_utf8_lossy(author.name).to_string();
            let author_email = String::from_utf8_lossy(author.email).to_string();
            // Parse author time from the signature string (format: "timestamp offset")
            // e.g., "1700000000 +0000"
            let timestamp = author
                .time
                .split_whitespace()
                .next()
                .unwrap_or("0")
                .parse::<i64>()
                .unwrap_or(0);
            let author_date = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
                .map(|dt| dt.to_rfc3339())
                .unwrap_or_default();

            entries.push(CommitEntry {
                sha: commit_id.to_string(),
                author_name,
                author_email,
                author_date,
                message: first_line,
                gpg_signature: None,
            });

            count += 1;
        }
    }

    Ok(entries)
}

fn list_branch_names(repo_path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let references = repo.references()?;
    let branches: Vec<String> = references
        .all()?
        .filter_map(|r| r.ok())
        .filter_map(|r| {
            let name = r.name().as_bstr();
            // Filter to only local branches (refs/heads/)
            if name.starts_with(b"refs/heads/") {
                let stripped = &name["refs/heads/".len()..];
                Some(String::from_utf8_lossy(stripped).to_string())
            } else {
                None
            }
        })
        .collect();

    Ok(branches)
}

fn list_tag_names(repo_path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let references = repo.references()?;
    let tags: Vec<String> = references
        .all()?
        .filter_map(|r| r.ok())
        .filter_map(|r| {
            let name = r.name().as_bstr();
            // Filter to only tags (refs/tags/)
            if name.starts_with(b"refs/tags/") {
                let stripped = &name["refs/tags/".len()..];
                Some(String::from_utf8_lossy(stripped).to_string())
            } else {
                None
            }
        })
        .collect();

    Ok(tags)
}

/// GET /api/v1/repos/:owner/:name/commits/:sha/signature
/// Get GPG signature verification status for a commit.
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/commits/{sha}/signature",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
        ("sha" = String, Path, description = "sha"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
    ),
)]
pub async fn get_commit_signature(
    State(state): State<AppState>,
    Path((owner, repo, sha)): Path<(String, String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos
    let _repo = match resolve_and_check_access(&state, &headers, &owner, &repo).await {
        Ok(r) => r,
        Err(e) => return e.into_response(),
    };

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if !repo_path.exists() {
        return AppError::not_found("repository not found").into_response();
    }

    // Validate SHA format
    if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return AppError::bad_request("invalid commit SHA format").into_response();
    }

    match verify_commit_signature(&repo_path, &sha) {
        Ok(sig) => (StatusCode::OK, Json(sig)).into_response(),
        Err(e) => AppError::not_found(e).into_response(),
    }
}

/// Verify a commit's GPG signature using `git log --show-signature`.
fn verify_commit_signature(repo_path: &std::path::Path, sha: &str) -> anyhow::Result<GpgSignature> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    // Resolve the commit SHA using gix
    let commit_id = match repo.rev_parse_single(sha) {
        Ok(id) => id,
        Err(_) => anyhow::bail!("commit {} not found", sha),
    };

    let full_sha = commit_id.to_string();

    // Read commit object to check for gpgsig header via gix extra_headers()
    let commit_object = repo.find_object(commit_id)?;
    let commit = commit_object
        .try_into_commit()
        .map_err(|_| anyhow::anyhow!("not a commit object"))?;

    // Use gix decode() + extra_headers() to check for gpgsig (replaces git cat-file commit)
    let has_gpgsig = commit.decode()?.extra_headers().find("gpgsig").is_some();

    if !has_gpgsig {
        return Ok(GpgSignature {
            verified: false,
            signer_key: None,
            signer_name: None,
            signer_email: None,
            status: "no_signature".to_string(),
        });
    }

    // TODO(gix): Verify the signature using git CLI — gix doesn't support cryptographic verification (Phase 3)
    // When gix ships built-in GPG verification (or sequoia-openpgp is introduced), replace this block.
    let git_gateway = rg_git::cli_gateway::global_gateway()
        .as_ref()
        .map_err(|e| anyhow::anyhow!("{}", e))?;

    let verify_output = git_gateway.run(
        &["log", "--format=%G?%n%GK%n%GN%n%GE", "-1", &full_sha],
        Some(repo_path),
    )?;
    if !verify_output.success() {
        return Ok(GpgSignature {
            verified: false,
            signer_key: None,
            signer_name: None,
            signer_email: None,
            status: "verification_failed".to_string(),
        });
    }
    let verify_text = verify_output.stdout_str();
    let lines: Vec<&str> = verify_text.lines().collect();

    let status_code: &str = lines.first().map(|l: &&str| l.trim()).unwrap_or("N");
    let signer_key = lines
        .get(1)
        .map(|l: &&str| l.trim().to_string())
        .filter(|s| !s.is_empty());
    let signer_name = lines
        .get(2)
        .map(|l: &&str| l.trim().to_string())
        .filter(|s| !s.is_empty());
    let signer_email = lines
        .get(3)
        .map(|l: &&str| l.trim().to_string())
        .filter(|s| !s.is_empty());

    let (verified, status): (bool, String) = match status_code {
        "G" => (true, "valid".to_string()),
        "E" => (false, "expired".to_string()),
        "X" => (false, "expired_key".to_string()),
        "Y" => (false, "expired_key".to_string()),
        "R" => (false, "revoked_key".to_string()),
        "B" => (false, "bad_signature".to_string()),
        "U" => (false, "untrusted".to_string()),
        "N" => (false, "no_signature".to_string()),
        _ => (false, format!("unknown_{}", status_code)),
    };

    Ok(GpgSignature {
        verified,
        signer_key,
        signer_name,
        signer_email,
        status,
    })
}

// ── Write access helper ─────────────────────────────────────────────
/// Resolve a repo by owner/name and enforce write access.
/// Returns the repo model. User must have write permission (owner, collaborator with write/admin, or org member with write).
async fn resolve_and_check_write_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
) -> Result<
    (
        rg_db::entities::repository::Model,
        rg_db::entities::user::Model,
    ),
    AppError,
> {
    let claims = extract_bearer_claims(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;

    let user_id = claims
        .sub
        .parse::<i64>()
        .map_err(|_| AppError::unauthorized("invalid token subject".to_string()))?;

    if user_id <= 0 {
        return Err(AppError::unauthorized("invalid token"));
    }

    let repo_model = rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, repo)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::not_found("repository not found"))?;

    if !rg_core::repo::service::can_write_repo(&state.db, &repo_model, Some(user_id))
        .await
        .unwrap_or(false)
    {
        return Err(AppError::forbidden("write access denied"));
    }

    let user = rg_db::ops::user_ops::find_by_id(&state.db, user_id)
        .await
        .map_err(AppError::internal)?
        .ok_or_else(|| AppError::unauthorized("invalid token"))?;

    Ok((repo_model, user))
}

// ── File creation/update/delete handlers ──────────────────────────

/// Create or update a file in a repository.
/// POST /api/v1/repos/:owner/:name/contents/:path
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/contents/{*path}",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    request_body = CreateOrUpdateFileRequest,
    responses(
        (status = 200, description = "Success", body = FileOperationResponse),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
        (status = 409, description = "Conflict (SHA mismatch)", body = serde_json::Value),
    ),
)]
pub async fn create_or_update_file(
    State(state): State<AppState>,
    Path((owner, repo, path)): Path<(String, String, String)>,
    headers: HeaderMap,
    Json(req): Json<CreateOrUpdateFileRequest>,
) -> impl IntoResponse {
    // Validate owner/repo
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // Check write access
    let (repo_model, user) =
        match resolve_and_check_write_access(&state, &headers, &owner, &repo).await {
            Ok(r) => r,
            Err(e) => return e.into_response(),
        };

    let branch = req.branch.unwrap_or(repo_model.default_branch.clone());

    // Call business logic
    match rg_core::repo::service::create_or_update_file(
        &state.db,
        repo_model.id,
        &owner,
        &repo,
        &path,
        &req.content,
        &req.message,
        &branch,
        req.sha.as_deref(),
        &user.username,
        &user.email,
        &state.repo_root,
    )
    .await
    {
        Ok(_) => {
            // Get the new commit SHA
            let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
            let new_sha = get_latest_commit_sha(&repo_path, &branch).unwrap_or_default();

            (
                StatusCode::OK,
                Json(FileOperationResponse {
                    success: true,
                    file_path: path,
                    commit_sha: new_sha,
                    message: "File created/updated successfully".to_string(),
                }),
            )
                .into_response()
        }
        Err(e) => {
            // Check if it's a SHA mismatch (conflict)
            if e.to_string().contains("SHA mismatch") {
                AppError::conflict(e.to_string()).into_response()
            } else {
                AppError::bad_request(e.to_string()).into_response()
            }
        }
    }
}

/// Delete a file from a repository.
/// DELETE /api/v1/repos/:owner/:name/contents/:path
#[utoipa::path(
    delete,
    path = "/repos/{owner}/{name}/contents/{*path}",
    tag = "Repository Content",
    params(
        ("owner" = String, Path, description = "owner"),
        ("name" = String, Path, description = "name"),
    ),
    responses(
        (status = 200, description = "Success", body = FileOperationResponse),
        (status = 400, description = "Bad request", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 403, description = "Forbidden", body = serde_json::Value),
        (status = 404, description = "Not found", body = serde_json::Value),
        (status = 409, description = "Conflict (SHA mismatch)", body = serde_json::Value),
    ),
)]
pub async fn delete_file(
    State(state): State<AppState>,
    Path((owner, repo, path)): Path<(String, String, String)>,
    headers: HeaderMap,
    Query(params): Query<DeleteFileQuery>,
) -> impl IntoResponse {
    // Validate owner/repo
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // Check write access
    let (repo_model, user) =
        match resolve_and_check_write_access(&state, &headers, &owner, &repo).await {
            Ok(r) => r,
            Err(e) => return e.into_response(),
        };

    let branch = params.branch.unwrap_or(repo_model.default_branch.clone());

    // Call business logic
    match rg_core::repo::service::delete_file(
        &state.db,
        repo_model.id,
        &owner,
        &repo,
        &path,
        &params.message,
        &branch,
        &params.sha,
        &user.username,
        &user.email,
        &state.repo_root,
    )
    .await
    {
        Ok(_) => {
            // Get the new commit SHA
            let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
            let new_sha = get_latest_commit_sha(&repo_path, &branch).unwrap_or_default();

            (
                StatusCode::OK,
                Json(FileOperationResponse {
                    success: true,
                    file_path: path,
                    commit_sha: new_sha,
                    message: "File deleted successfully".to_string(),
                }),
            )
                .into_response()
        }
        Err(e) => {
            // Check if it's a SHA mismatch (conflict)
            if e.to_string().contains("SHA mismatch") {
                AppError::conflict(e.to_string()).into_response()
            } else {
                AppError::bad_request(e.to_string()).into_response()
            }
        }
    }
}

/// Get the latest commit SHA on a branch.
fn get_latest_commit_sha(repo_path: &std::path::Path, branch: &str) -> anyhow::Result<String> {
    let repo = gix::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let reference = format!("refs/heads/{}", branch);
    let oid = repo
        .rev_parse_single(reference.as_str())
        .map_err(|e| anyhow::anyhow!("failed to resolve branch '{}': {}", branch, e))?;

    Ok(oid.to_string())
}
