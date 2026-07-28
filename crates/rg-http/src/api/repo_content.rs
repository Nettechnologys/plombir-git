//! REST API handlers for repository content browsing (tree, blob, history).

use anyhow::Context;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use chrono;
use serde::{Deserialize, Serialize};

use crate::api::repo_access::{CiRead, RepoContents, RepoWrite};
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
    pub encoding: String, // "utf-8" | "base64" | "none"
    pub is_binary: bool,
    /// True when the blob is larger than `MAX_BLOB_API_BYTES` and was therefore
    /// NOT read into memory or encoded: `content` is empty and `encoding` is
    /// "none" (only `size`/`sha` are meaningful). Clients should fetch the file
    /// another way (clone / archive) instead of the JSON blob API. Guards
    /// against a memory-amplification DoS where a huge committed file would be
    /// base64-inflated (×4/3) and JSON-escaped into a single in-memory frame.
    pub too_large: bool,
}

/// Upper bound (bytes) on a blob the JSON blob API will inline. A file larger
/// than this is reported with `too_large: true` and an empty body instead of
/// being loaded + base64/UTF-8 encoded into one in-memory JSON frame — mirroring
/// how GitHub's Contents API refuses to inline large files. 5 MiB sits well
/// above normal source files while bounding worst-case per-request memory
/// (5 MiB raw → ~6.7 MiB base64 → JSON escaping).
pub const MAX_BLOB_API_BYTES: u64 = 5 * 1024 * 1024;

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
    Query(params): Query<TreeQuery>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
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
            // Only the typed outcomes of `list_tree_entries` become 4xx; a git
            // layer that failed is a 5xx, and `From<anyhow::Error>` logs the
            // full context chain for operators before sanitizing the body. The
            // unconditional `tracing::error!("list_tree failed")` that used to
            // sit here logged a mistyped `?ref=` at error level on every miss.
            AppError::from(e).into_response()
        }
    }
}

/// Returns true if the repository has no commits yet (unborn HEAD), e.g. a
/// repo that was just created but never pushed to.
///
/// Only an *answerable* "no commits" question returns true: if the repository
/// can't be opened or `HEAD` can't be read at all, the state is unknown, not
/// empty — we log why and return false so the caller surfaces the real error
/// instead of rendering a healthy-looking empty repo (card_6f2a9ab1e623).
fn is_empty_repo(repo_path: &std::path::Path) -> bool {
    let repo = match gix::open(repo_path) {
        Ok(repo) => repo,
        Err(e) => {
            tracing::warn!(
                repo = %repo_path.display(),
                error = %format!("{e:#}"),
                "cannot open repository to check for unborn HEAD"
            );
            return false;
        }
    };
    match repo.head() {
        // `Head::id()` is None when HEAD points at a branch that doesn't
        // exist yet (no commits).
        Ok(head) => head.id().is_none(),
        Err(e) => {
            tracing::warn!(
                repo = %repo_path.display(),
                error = %format!("{e:#}"),
                "cannot read HEAD — treating repository as non-empty so the real error surfaces"
            );
            false
        }
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
    Query(params): Query<BlobQuery>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
    }

    let git_ref = params.r#ref.unwrap_or_else(|| "HEAD".to_string());

    match get_blob_content(&repo_path, &git_ref, &path) {
        Ok(blob) => (StatusCode::OK, Json(blob)).into_response(),
        // Only the two typed outcomes of `get_blob_content` become 4xx; a git
        // layer that failed is a 5xx, and `From<anyhow::Error>` logs the full
        // context chain for operators before sanitizing the body.
        Err(e) => AppError::from(e).into_response(),
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
    Query(params): Query<LogQuery>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
    }

    let git_ref = params.r#ref.clone().unwrap_or_else(|| "HEAD".to_string());
    let limit = params.limit.unwrap_or(50).min(100);
    let file_path = params.path.unwrap_or_default();

    match get_commit_log(&repo_path, &git_ref, &file_path, limit) {
        Ok(log) => (StatusCode::OK, Json(serde_json::json!({ "commits": log }))).into_response(),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "get_log failed");
            AppError::from(e).into_response()
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
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
    }

    match list_branch_names(&repo_path) {
        Ok(branches) => (StatusCode::OK, Json(branches)).into_response(),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "list_branches failed");
            AppError::from(e).into_response()
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
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
    }

    match list_tag_names(&repo_path) {
        Ok(tags) => (StatusCode::OK, Json(tags)).into_response(),
        Err(e) => {
            tracing::error!(error = %format!("{e:#}"), "list_tags failed");
            AppError::from(e).into_response()
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
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

    // Exactly two outcomes below belong to the client: the ref does not
    // resolve, and the sub-path is not in that ref's tree. They carry
    // `rg_core::error::NotFound` so the HTTP layer can name them; everything
    // else here — a repository that will not open, a commit that will not
    // decode, a tree object that is missing — is ours and stays a 5xx. While
    // no outcome was typed, *all* of them collapsed into one 500, so a browser
    // asking for a deleted branch was reported as a server fault and logged as
    // one on every miss (card_784afeaf9603, the mirror of card_aa048c2956b1
    // one endpoint over).
    let commit_id = repo.rev_parse_single(git_ref).map_err(|e| {
        anyhow::Error::new(rg_core::error::NotFound::new("ref")).context(format!(
            "resolving ref '{}' in {:?}: {}",
            git_ref, repo_path, e
        ))
    })?;

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
            let entry = entry.ok_or_else(|| {
                anyhow::Error::new(rg_core::error::NotFound::new("path")).context(format!(
                    "sub-path '{}' has no component '{}' in {:?} at '{}'",
                    sub_path, component, repo_path, git_ref
                ))
            })?;
            if !entry.mode().is_tree() {
                // The path resolves, it just is not a directory — the mirror of
                // `get_blob_content`'s "path is not a file", and the fixed text
                // carries no request data (H-05). Left to `find_tree` below it
                // came out as a 500, because "the client pointed at a blob" and
                // "the object store lost a tree" are the same untyped error
                // once they are both `anyhow!("failed to find sub-tree")`.
                return Err(rg_core::error::invalid_request("path is not a directory"));
            }
            tree = repo
                .find_tree(entry.oid())
                .map_err(|e| anyhow::anyhow!("failed to find sub-tree: {}", e))?;
        }
    }

    let mut entries = Vec::new();
    for entry in tree.iter() {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                // Skipping silently would drop the entry from the listing and
                // make a corrupt tree look like a short directory.
                tracing::warn!(
                    repo = %repo_path.display(),
                    git_ref = %git_ref,
                    path = %sub_path,
                    error = %format!("{e:#}"),
                    "skipping unreadable tree entry"
                );
                continue;
            }
        };
        let oid = entry.oid();
        let name = entry.filename().to_string();
        let kind = if entry.mode().is_tree() {
            "tree".to_string()
        } else {
            "blob".to_string()
        };

        let full_path = if sub_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", sub_path, name)
        };

        let size = if kind == "blob" {
            // A missing size stays non-fatal — one unreadable object must not
            // fail the whole directory listing — but `.ok()` on its own made
            // the entry look like a file whose size simply was not recorded,
            // with nothing anywhere saying why (card_6f2a9ab1e623).
            match get_blob_size(repo_path, &oid.to_string()) {
                Ok(size) => Some(size),
                Err(e) => {
                    tracing::warn!(
                        repo = %repo_path.display(),
                        git_ref = %git_ref,
                        path = %full_path,
                        error = %format!("{e:#}"),
                        "cannot read blob size — listing the entry without one"
                    );
                    None
                }
            }
        } else {
            None
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
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

    let target = format!("{}:{}", git_ref, path);

    // Exactly two outcomes below belong to the client: the `ref:path` pair does
    // not resolve, and it resolves to something that is not a file. They carry
    // `rg_core::error::NotFound` / `InvalidRequest` so the HTTP layer can name
    // them; everything else in this function — a repository that will not open,
    // an unreadable object header, an object whose header lied — is ours and
    // stays a 5xx. The whole function used to be flattened into one
    // `AppError::not_found(e)` at the call site, which reported a broken
    // repository as an absent file *and* put `repo_path` in the response body
    // (404s are not sanitized by `IntoResponse`, H-05).
    let object_id = repo.rev_parse_single(target.as_str()).map_err(|e| {
        anyhow::Error::new(rg_core::error::NotFound::new("file"))
            .context(format!("resolving '{}' in {:?}: {}", target, repo_path, e))
    })?;

    // Inspect the object header WITHOUT decoding the blob into memory, so an
    // oversized file is rejected before we ever buffer + base64-inflate it.
    let header = repo
        .find_header(object_id)
        .map_err(|e| anyhow::anyhow!("failed to read object header: {}", e))?;
    if header.kind() != gix::object::Kind::Blob {
        // The path exists, it just is not a blob — "not found" would be a lie,
        // and the fixed text carries no request data (H-05).
        return Err(rg_core::error::invalid_request("path is not a file"));
    }
    let blob_size = header.size();
    if blob_size > MAX_BLOB_API_BYTES {
        // Memory-amplification guard: return metadata only, never load/encode.
        return Ok(BlobContent {
            path: path.to_string(),
            sha: object_id.to_string(),
            size: blob_size as i64,
            content: String::new(),
            encoding: "none".to_string(),
            is_binary: false,
            too_large: true,
        });
    }

    // Under the cap: safe to load and encode the full blob.
    let object = repo
        .find_object(object_id)
        .map_err(|e| anyhow::anyhow!("failed to find object: {}", e))?;

    // Not a client outcome: the header above already said this object is a
    // blob, so failing here means the object store contradicts itself.
    let blob = object.try_into_blob().map_err(|e| {
        anyhow::anyhow!(
            "object header claimed a blob but the object is not one: {}",
            e
        )
    })?;

    let data = blob.data.as_slice();
    let size = data.len() as i64;

    // Check if binary by looking for null bytes
    let is_binary = data.contains(&0);

    let (content, encoding) = if is_binary {
        let mut s = String::with_capacity(data.len() * 4 / 3 + 4);
        // Simple base64 encoding
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let chunks = data.chunks(3);
        for chunk in chunks {
            let b0 = chunk[0] as u32;
            let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
            let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
            let triple = (b0 << 16) | (b1 << 8) | b2;
            s.push(ALPHABET[((triple >> 18) & 0x3F) as usize] as char);
            s.push(ALPHABET[((triple >> 12) & 0x3F) as usize] as char);
            if chunk.len() > 1 {
                s.push(ALPHABET[((triple >> 6) & 0x3F) as usize] as char);
            } else {
                s.push('=');
            }
            if chunk.len() > 2 {
                s.push(ALPHABET[(triple & 0x3F) as usize] as char);
            } else {
                s.push('=');
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
        too_large: false,
    })
}

fn get_blob_size(repo_path: &std::path::Path, sha: &str) -> anyhow::Result<i64> {
    let repo = gix::open(repo_path)
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

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
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

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
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

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
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

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
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    // H-01: Auth check for private repos

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
    }

    // Validate SHA format
    if sha.len() < 7 || !sha.chars().all(|c| c.is_ascii_hexdigit()) {
        return AppError::bad_request("invalid commit SHA format").into_response();
    }

    match verify_commit_signature(&repo_path, &sha) {
        Ok(sig) => (StatusCode::OK, Json(sig)).into_response(),
        // See `get_blob`: an unopenable repository is not a missing commit.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// Verify a commit's GPG signature using `git log --show-signature`.
fn verify_commit_signature(repo_path: &std::path::Path, sha: &str) -> anyhow::Result<GpgSignature> {
    let repo = gix::open(repo_path)
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

    // The same two-client-outcomes split as `get_blob_content`: the SHA does
    // not resolve, or it resolves to something that is not a commit. A repo
    // that would not open never reaches here, and must not be reported as a
    // missing commit.
    let commit_id = match repo.rev_parse_single(sha) {
        Ok(id) => id,
        Err(e) => {
            return Err(
                anyhow::Error::new(rg_core::error::NotFound::new("commit")).context(format!(
                    "resolving commit '{}' in {:?}: {}",
                    sha, repo_path, e
                )),
            )
        }
    };

    let full_sha = commit_id.to_string();

    // Read commit object to check for gpgsig header via gix extra_headers()
    let commit_object = repo.find_object(commit_id)?;
    let commit = commit_object
        .try_into_commit()
        .map_err(|_| rg_core::error::invalid_request("object is not a commit"))?;

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

// ── Commit author ───────────────────────────────────────────────────
/// The account behind an already-authorized write, for the commit it will
/// author.
///
/// Write access itself is the `RepoWrite` extractor's business — these handlers
/// used to call `repo_access::require_write` here instead, which is the gate as
/// a convention again: nothing but the author's memory made the call happen.
async fn commit_author(
    state: &AppState,
    actor_id: i64,
) -> Result<rg_db::entities::user::Model, AppError> {
    rg_db::ops::user_ops::find_by_id(&state.db, actor_id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::unauthorized("invalid token"))
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
    RepoWrite {
        repo: repo_model,
        actor_id,
    }: RepoWrite,
    Json(req): Json<CreateOrUpdateFileRequest>,
) -> impl IntoResponse {
    // Validate owner/repo
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    let user = match commit_author(&state, actor_id).await {
        Ok(user) => user,
        Err(e) => return e.into_response(),
    };

    let branch = req.branch.unwrap_or(repo_model.default_branch.clone());
    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    // Read *before* the write: the hooks below need the ref's previous value,
    // and once the commit lands it is gone. A branch that does not exist yet
    // (the editor may create one) legitimately resolves to nothing — that is
    // the all-zero SHA a push sends for a created ref, which is what makes the
    // `branch.created` webhook fire instead of a plain `push`.
    let old_sha = previous_branch_sha(&repo_path, &branch);

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
            let new_sha = latest_commit_sha_or_log(&repo_path, &branch);

            spawn_post_push_hooks_for_edit(
                &state, repo_path, &owner, &repo, &branch, &old_sha, &new_sha, user.id,
            );

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
        // The service now carries its own outcome: a lost race is a typed
        // `Conflict`, an absent file a `NotFound`, a rejected path an
        // `InvalidRequest`. Matching on the message text instead made the `409`
        // hostage to the exact wording, and handed every other failure — an
        // unwritable `repo_root`, a git clone that died, a push that was
        // rejected — to the client as a `400` it would never retry.
        Err(e) => AppError::from(e).into_response(),
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
    RepoWrite {
        repo: repo_model,
        actor_id,
    }: RepoWrite,
    Query(params): Query<DeleteFileQuery>,
) -> impl IntoResponse {
    // Validate owner/repo
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    let user = match commit_author(&state, actor_id).await {
        Ok(user) => user,
        Err(e) => return e.into_response(),
    };

    let branch = params.branch.unwrap_or(repo_model.default_branch.clone());
    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    // Same as `create_or_update_file`: the ref's old value has to be read
    // before the commit that replaces it.
    let old_sha = previous_branch_sha(&repo_path, &branch);

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
            let new_sha = latest_commit_sha_or_log(&repo_path, &branch);

            spawn_post_push_hooks_for_edit(
                &state, repo_path, &owner, &repo, &branch, &old_sha, &new_sha, user.id,
            );

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
        // Same split as `create_or_update_file` above.
        Err(e) => AppError::from(e).into_response(),
    }
}

/// The all-zero object id git uses on the wire for "this ref had no value".
const ZERO_SHA: &str = "0000000000000000000000000000000000000000";

/// The branch's SHA *before* an edit, in the form a push reports it: the
/// all-zero id when the branch does not exist yet.
///
/// An unresolvable branch is the normal case for an editor that creates one, so
/// this is not an error path — but it is not silent either, since the same
/// failure also covers an unreadable repository, and the hooks downstream would
/// then mistake an existing branch for a newly created one.
fn previous_branch_sha(repo_path: &std::path::Path, branch: &str) -> String {
    match get_latest_commit_sha(repo_path, branch) {
        Ok(sha) => sha,
        Err(e) => {
            tracing::debug!(
                repo = %repo_path.display(),
                branch = %branch,
                error = %format!("{e:#}"),
                "no pre-edit SHA for branch — treating the edit as a branch creation"
            );
            ZERO_SHA.to_string()
        }
    }
}

/// Hand a web-editor commit to the same post-push automation a `git push` gets.
///
/// The editor writes a real commit onto a real branch (`rg_core::repo::service`
/// clones, commits and pushes into the bare repo), so everything a push
/// triggers is owed here too: CI, the `push` / `branch.*` webhooks, the
/// open-PR head-SHA refresh that auto-merge and the merge queue read, and the
/// watch fan-out. None of it ran until card_13202be354ac.
///
/// Skipped when the read-back of the new SHA failed or the branch did not
/// actually move. That guard is load-bearing, not defensive noise: an empty
/// `new_sha` reaching the hooks reads as a *deleted* ref and would fire
/// `branch.deleted` for a branch that is alive and well.
#[allow(clippy::too_many_arguments)]
fn spawn_post_push_hooks_for_edit(
    state: &AppState,
    repo_path: std::path::PathBuf,
    owner: &str,
    repo: &str,
    branch: &str,
    old_sha: &str,
    new_sha: &str,
    pusher_id: i64,
) {
    if new_sha.is_empty() || new_sha == ZERO_SHA || new_sha == old_sha {
        tracing::warn!(
            repo = %format!("{owner}/{repo}"),
            branch = %branch,
            old_sha = %old_sha,
            new_sha = %new_sha,
            "web-editor commit landed but its new head SHA is unusable — \
             skipping post-push hooks rather than reporting a bogus ref update"
        );
        return;
    }

    state.spawn_post_push_hooks(
        repo_path,
        owner.to_string(),
        repo.to_string(),
        Some(pusher_id),
        vec![rg_git::protocol::receive_pack::RefUpdate {
            old_sha: old_sha.to_string(),
            new_sha: new_sha.to_string(),
            refname: format!("refs/heads/{branch}"),
            status: "ok".to_string(),
            message: String::new(),
        }],
    );
}

/// Best-effort read-back of the commit SHA for the write endpoints. The commit
/// has already landed, so a failure here must not fail the request — but it
/// must not be silent either: `.unwrap_or_default()` used to hand the client an
/// empty `commit_sha` with nothing in the log (card_6f2a9ab1e623).
fn latest_commit_sha_or_log(repo_path: &std::path::Path, branch: &str) -> String {
    match get_latest_commit_sha(repo_path, branch) {
        Ok(sha) => sha,
        Err(e) => {
            tracing::warn!(
                repo = %repo_path.display(),
                branch = %branch,
                error = %format!("{e:#}"),
                "commit landed but reading back its SHA failed — responding with an empty commit_sha"
            );
            String::new()
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

#[cfg(test)]
mod tests {
    use super::is_empty_repo;

    /// The ordinary positive case the empty-tree response exists for: a repo
    /// that was created but never pushed to.
    #[test]
    fn a_repository_without_commits_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("fresh.git");
        gix::init_bare(&repo_path).expect("a bare repo must initialise");

        assert!(
            is_empty_repo(&repo_path),
            "an unborn HEAD is the one state that legitimately means `no commits yet`"
        );
    }

    /// The bug (card_6f2a9ab1e623): an unreadable `HEAD` used to answer
    /// "repository is empty", so `list_tree` returned `200 {entries: []}` and
    /// the real error never reached the client or the log. Unknown is not
    /// empty.
    #[test]
    fn an_unreadable_head_is_not_reported_as_an_empty_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken.git");
        gix::init_bare(&repo_path).expect("a bare repo must initialise");
        std::fs::write(repo_path.join("HEAD"), "not a ref at all\n").unwrap();

        // The fixture must exercise the `repo.head()` error branch specifically
        // — if `gix::open` itself rejected it we would be testing the other,
        // already-correct arm.
        let repo = gix::open(&repo_path).expect("a malformed HEAD must still open the repository");
        assert!(
            repo.head().is_err(),
            "fixture must produce a HEAD that cannot be read"
        );

        assert!(
            !is_empty_repo(&repo_path),
            "a HEAD that cannot be read is an unknown state, not an empty repository"
        );
    }

    /// A repository that cannot be opened at all is likewise unknown, not
    /// empty — this arm was already correct and must stay that way.
    #[test]
    fn a_path_that_is_not_a_repository_is_not_reported_as_empty() {
        let dir = tempfile::tempdir().unwrap();

        assert!(!is_empty_repo(&dir.path().join("nothing-here.git")));
    }
}
