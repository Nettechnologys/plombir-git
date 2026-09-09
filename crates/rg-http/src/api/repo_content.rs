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

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
pub struct TreeQuery {
    /// Git ref (branch, tag, commit SHA). Default: HEAD
    #[serde(default)]
    pub r#ref: Option<String>,
    /// Sub-path within the tree. Default: root
    #[serde(default)]
    pub path: Option<String>,
}

#[derive(Deserialize, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
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

/// One entry of `GET /repos/{owner}/{name}/branches`.
///
/// The list used to be bare `Vec<String>`, which left the browser unable to say
/// which branch is the default one: the page could only guess from a second
/// request for the repository row. `is_default` is read from the repository's
/// symbolic `HEAD`, i.e. from Git itself rather than from the database mirror of
/// it, so the marker matches what a `git clone` would check out.
#[derive(Debug, Serialize)]
pub struct BranchRef {
    pub name: String,
    pub is_default: bool,
}

const DEFAULT_COMMIT_LOG_LIMIT: i64 = 50;
const MAX_COMMIT_LOG_LIMIT: i64 = 100;

fn commit_log_limit(limit: Option<i64>) -> Result<usize, AppError> {
    let limit = limit.unwrap_or(DEFAULT_COMMIT_LOG_LIMIT);
    if limit <= 0 {
        return Err(AppError::bad_request("limit must be greater than zero"));
    }

    // The validated value is positive and capped well below usize::MAX on
    // every supported target, so the conversion is lossless.
    Ok(limit.min(MAX_COMMIT_LOG_LIMIT) as usize)
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
#[derive(Deserialize, utoipa::ToSchema, utoipa::IntoParams)]
#[into_params(parameter_in = Query)]
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

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TreeEntryKind {
    Tree,
    Blob,
    Commit,
}

impl TreeEntryKind {
    fn from_mode(mode: gix::object::tree::EntryMode) -> Self {
        use gix::object::tree::EntryKind;

        match mode.kind() {
            EntryKind::Tree => Self::Tree,
            EntryKind::Commit => Self::Commit,
            // A symlink is backed by a blob too: the contents API reads its
            // target text from that blob, so it deliberately keeps the same
            // client-facing kind as regular and executable files.
            EntryKind::Blob | EntryKind::BlobExecutable | EntryKind::Link => Self::Blob,
        }
    }

    fn is_tree(self) -> bool {
        self == Self::Tree
    }

    fn is_blob(self) -> bool {
        self == Self::Blob
    }
}

#[derive(Debug, Serialize)]
pub struct TreeEntry {
    pub name: String,
    pub path: String,
    pub kind: TreeEntryKind,
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
pub const MAX_BLOB_API_BYTES: u64 = rg_core::repo::service::MAX_BLOB_API_BYTES;

/// Transport envelope for the JSON file-edit request. A JSON string can use
/// six wire bytes for one decoded control byte (`\u00xx`); the small allowance
/// covers the branch, commit message, SHA and object syntax without making any
/// of those fields unbounded.
pub(crate) const CONTENT_EDIT_JSON_MAX_BYTES: usize = MAX_BLOB_API_BYTES as usize * 6 + 64 * 1024;

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

/// What a commit signature check concluded.
///
/// `git log --format=%G?` answers two different questions with one alphabet:
/// whether the signature holds, and whether it could be checked at all. `E`
/// means Git could not check it — in practice, this instance holds no public
/// key for the signer, which on an instance where contributor keys were never
/// imported is the answer for *every* signed commit. That is not a claim about
/// the commit, and a boolean has no room to say so (card_61b29791d099). The
/// push path already keeps the two apart in
/// [`rg_git::protocol::receive_pack::unsigned_commit_for_required_signature`];
/// this is the same distinction on the read path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SignatureVerdict {
    /// Git checked the signature and it holds.
    Valid,
    /// Git checked the signature and rejected it: it does not match, or the key
    /// behind it is revoked or expired. Evidence against the commit.
    Invalid,
    /// Git reached no verdict — the signer's key is unavailable here, or its
    /// trust is unknown. Says nothing about the commit either way.
    Undeterminable,
    /// The commit carries no signature header: nothing was checked because
    /// there is nothing to check.
    Unsigned,
}

/// GPG signature information for a commit.
#[derive(Serialize)]
pub struct GpgSignature {
    /// The verdict itself. Deliberately not a `verified: bool`: a consumer
    /// reading a boolean has to fold "could not check" into one of the two
    /// answers, and the one it lands in is the accusation.
    pub verdict: SignatureVerdict,
    pub signer_key: Option<String>,
    pub signer_name: Option<String>,
    pub signer_email: Option<String>,
    /// Detail behind the verdict, for the badge tooltip: which `%G?` code Git
    /// answered with, in words.
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
        TreeQuery,
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 409, description = "HEAD points at a branch that does not exist while other branches do", body = serde_json::Value),
    ),
)]
pub async fn list_tree(
    State(state): State<AppState>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
    Path((owner, repo)): Path<(String, String)>,
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
        // A malformed ref is the caller's mistake whatever the repository
        // holds. Drawing the empty-repository 200 below for it would hide a
        // typed 400 behind a plausible-looking empty tree.
        Err(e) if e.downcast_ref::<rg_core::error::InvalidRequest>().is_some() => {
            AppError::from(e).into_response()
        }
        Err(e) => match classify_repo_emptiness(&repo_path) {
            // A freshly-created repo with no commits has an unborn HEAD, which
            // can't be resolved to a tree. That's not an error — return an
            // empty tree so the UI can render the empty-repo state.
            RepoEmptiness::Empty => {
                (StatusCode::OK, Json(serde_json::json!({ "entries": [] }))).into_response()
            }
            // A repository whose HEAD lost its branch has a history to show and
            // no way to name it. Drawing the empty state here is what hid a
            // full repository behind "push an existing repository".
            RepoEmptiness::HeadWithoutBranch { head, branches } => {
                head_without_branch_error(&head, &branches).into_response()
            }
            // Only the typed outcomes of `list_tree_entries` become 4xx; a git
            // layer that failed is a 5xx, and `From<anyhow::Error>` logs the
            // full context chain for operators before sanitizing the body. The
            // unconditional `tracing::error!("list_tree failed")` that used to
            // sit here logged a mistyped `?ref=` at error level on every miss.
            RepoEmptiness::NotEmpty => AppError::from(e).into_response(),
        },
    }
}

/// What the read side found when it asked "does this repository have any
/// commits?".
///
/// Two of these outcomes are indistinguishable through a `bool`, and
/// collapsing them is exactly how a repository with a full history got drawn
/// as "Quick setup — push an existing repository" (card_9e11f76dddd1).
pub(crate) enum RepoEmptiness {
    /// Unborn `HEAD` and no `refs/heads/*` at all: created, never pushed to.
    /// The one state that legitimately renders as an empty repository.
    Empty,
    /// Unborn `HEAD`, but branches exist — `HEAD` names a branch nobody
    /// created. That is not emptiness but a desync, and only naming both sides
    /// makes it diagnosable from the response instead of from an ssh session.
    HeadWithoutBranch { head: String, branches: Vec<String> },
    /// Commits are reachable, or the state could not be determined at all.
    /// Both mean the same thing to a caller: do not dress this up as empty,
    /// let the original error through (card_6f2a9ab1e623).
    NotEmpty,
}

/// How many branch names the desync error is willing to spell out. The list is
/// a diagnosis, not a listing — `GET /repos/{owner}/{name}/branches` is where
/// the full set lives.
const DESYNC_BRANCH_SAMPLE: usize = 10;

/// `HEAD` points at a branch that does not exist while other branches do.
///
/// A `409`, not a `5xx`: the request is well-formed, the server is healthy, and
/// the repository owner can fix it by pointing the default branch at a branch
/// that exists. `Conflict` also means the message survives sanitization — which
/// is the point, since the diagnosis *is* the pair of names. Branch names are
/// no more secret here than in the branches endpoint the same reader already
/// passed the same authorization for.
pub(crate) fn head_without_branch_error(head: &str, branches: &[String]) -> AppError {
    let shown = branches
        .iter()
        .take(DESYNC_BRANCH_SAMPLE)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    let rest = branches.len().saturating_sub(DESYNC_BRANCH_SAMPLE);
    let overflow = if rest > 0 {
        format!(" and {rest} more")
    } else {
        String::new()
    };
    AppError::Conflict(format!(
        "repository HEAD points at `{head}`, which does not exist; \
         existing branches: {shown}{overflow}. \
         The repository is not empty — set its default branch to a branch that exists."
    ))
}

/// Classify a repository the read side could not resolve a commit from.
///
/// Only an *answerable* "no commits" question returns [`RepoEmptiness::Empty`]:
/// if the repository cannot be opened, `HEAD` cannot be read, or the branch
/// refs cannot be enumerated, the state is unknown rather than empty — we log
/// why and answer [`RepoEmptiness::NotEmpty`] so the caller surfaces the real
/// error instead of rendering a healthy-looking empty repo (card_6f2a9ab1e623).
pub(crate) fn classify_repo_emptiness(repo_path: &std::path::Path) -> RepoEmptiness {
    let repo = match rg_git::repository::open(repo_path) {
        Ok(repo) => repo,
        Err(e) => {
            tracing::warn!(
                repo = %repo_path.display(),
                error = %format!("{e:#}"),
                "cannot open repository to check for unborn HEAD"
            );
            return RepoEmptiness::NotEmpty;
        }
    };
    let head = match repo.head() {
        Ok(head) => head,
        Err(e) => {
            tracing::warn!(
                repo = %repo_path.display(),
                error = %format!("{e:#}"),
                "cannot read HEAD — treating repository as non-empty so the real error surfaces"
            );
            return RepoEmptiness::NotEmpty;
        }
    };
    // `Head::id()` is None exactly when HEAD names a branch that does not
    // exist. Whether that means "no commits yet" or "HEAD lost its branch" is
    // decided by `refs/heads/*`, not by HEAD alone.
    if head.id().is_some() {
        return RepoEmptiness::NotEmpty;
    }
    let head_name = head
        .referent_name()
        .map(|name| name.as_bstr().to_string())
        .unwrap_or_else(|| "HEAD".to_string());

    let mut branches = match branch_names(&repo) {
        Ok(branches) => branches,
        Err(e) => {
            tracing::warn!(
                repo = %repo_path.display(),
                error = %format!("{e:#}"),
                "cannot enumerate branches — treating repository as non-empty so the real error surfaces"
            );
            return RepoEmptiness::NotEmpty;
        }
    };
    if branches.is_empty() {
        return RepoEmptiness::Empty;
    }
    branches.sort();
    RepoEmptiness::HeadWithoutBranch {
        head: head_name,
        branches,
    }
}

/// Short names of every `refs/heads/*`, packed refs included.
///
/// A ref that cannot be read fails the whole enumeration: a shortened list
/// would answer "this repository has no branches" and send the caller straight
/// back into the empty-repo verdict this function exists to prevent.
fn branch_names(repo: &gix::Repository) -> anyhow::Result<Vec<String>> {
    let references = repo.references()?;
    let mut names = Vec::new();
    for reference in references.prefixed(b"refs/heads/".as_slice())? {
        let reference = reference.map_err(anyhow::Error::from_boxed)?;
        let name = reference.name().as_bstr();
        names.push(String::from_utf8_lossy(&name["refs/heads/".len()..]).to_string());
    }
    Ok(names)
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
        BlobQuery,
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Repository or ref not found", body = serde_json::Value),
        (status = 500, description = "Repository storage or commit history could not be read", body = serde_json::Value),
    ),
)]
pub async fn get_blob(
    State(state): State<AppState>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
    Path((owner, repo, path)): Path<(String, String, String)>,
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
        ("limit" = Option<i64>, Query, description = "Max number of commits (1-100, default 50)"),
    ),
    responses(
        (status = 200, description = "Success", body = serde_json::Value),
        (status = 400, description = "Invalid repository path, ambiguous ref, or non-positive limit", body = serde_json::Value),
        (status = 401, description = "Unauthorized", body = serde_json::Value),
        (status = 404, description = "Repository or ref not found", body = serde_json::Value),
        (status = 409, description = "HEAD points at a branch that does not exist while other branches do", body = serde_json::Value),
        (status = 500, description = "Repository storage or commit history could not be read", body = serde_json::Value),
    ),
)]
pub async fn get_log(
    State(state): State<AppState>,
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<LogQuery>,
) -> impl IntoResponse {
    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return AppError::bad_request(e.to_string()).into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return AppError::bad_request(e.to_string()).into_response();
    }

    let limit = match commit_log_limit(params.limit) {
        Ok(limit) => limit,
        Err(error) => return error.into_response(),
    };

    // H-01: Auth check for private repos

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, repo));
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
    }

    let git_ref = params.r#ref.clone().unwrap_or_else(|| "HEAD".to_string());
    let file_path = params.path.unwrap_or_default();

    match get_commit_log(&repo_path, &git_ref, &file_path, limit) {
        Ok(log) => (StatusCode::OK, Json(serde_json::json!({ "commits": log }))).into_response(),
        Err(e) => {
            // An unborn HEAD is the one rev-parse failure that means a healthy
            // empty history. Keep that response distinct from a missing ref
            // (typed 404), a HEAD that lost its branch (typed 409 naming both
            // sides) and a repository failure (sanitized 5xx).
            if git_ref == "HEAD" {
                match classify_repo_emptiness(&repo_path) {
                    RepoEmptiness::Empty => {
                        return (
                            StatusCode::OK,
                            Json(serde_json::json!({ "commits": Vec::<CommitEntry>::new() })),
                        )
                            .into_response();
                    }
                    RepoEmptiness::HeadWithoutBranch { head, branches } => {
                        return head_without_branch_error(&head, &branches).into_response();
                    }
                    RepoEmptiness::NotEmpty => {}
                }
            }
            // `From<anyhow::Error>` owns error logging and deliberately stays
            // quiet for the typed NotFound case. Logging unconditionally here
            // would turn every mistyped `?ref=` into an error-level event.
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
        (status = 500, description = "Repository storage failure", body = serde_json::Value),
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

    match list_branch_refs(&repo_path) {
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
        (status = 500, description = "Repository storage failure", body = serde_json::Value),
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
    let repo = rg_git::repository::open(repo_path)
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
    //
    // Typing them is only half of it: `rev_parse_single` still answered "no
    // such ref" for a ref store it could not read and for a ref whose target
    // commit had left the object store, so a broken repository kept arriving
    // as a client miss (card_7cb31c61cee2). Both boundaries below therefore
    // come from APIs whose `None` means absence and nothing else.
    let commit_id = resolve_content_ref(&repo, git_ref, repo_path)?;

    let mut tree = commit_id
        .object()
        .with_context(|| {
            format!(
                "reading the object of ref '{}' in {} before listing its tree",
                git_ref,
                repo_path.display()
            )
        })?
        .peel_to_tree()
        .with_context(|| {
            format!(
                "resolving the tree of ref '{}' in {}",
                git_ref,
                repo_path.display()
            )
        })?;

    // Traverse into sub_path if specified
    if !sub_path.is_empty() {
        let Some(entry) = lookup_tree_path(&repo, tree, sub_path, repo_path, git_ref)? else {
            return Err(
                anyhow::Error::new(rg_core::error::NotFound::new("path")).context(format!(
                    "sub-path '{}' is not in {:?} at '{}'",
                    sub_path, repo_path, git_ref
                )),
            );
        };
        if !entry.kind.is_tree() {
            // The path resolves, it just is not a directory — the mirror of
            // `get_blob_content`'s "path is not a file", and the fixed text
            // carries no request data (H-05). Left to `find_tree` below it
            // came out as a 500, because "the client pointed at a blob" and
            // "the object store lost a tree" are the same untyped error
            // once they are both `anyhow!("failed to find sub-tree")`.
            return Err(rg_core::error::invalid_request("path is not a directory"));
        }
        tree = repo.find_tree(entry.id).with_context(|| {
            format!(
                "reading the tree of sub-path '{}' in {:?} at '{}'",
                sub_path, repo_path, git_ref
            )
        })?;
    }

    let mut entries = Vec::new();
    for entry in tree.iter() {
        let entry = entry.with_context(|| {
            let listed_path = if sub_path.is_empty() {
                "<root>"
            } else {
                sub_path
            };
            format!(
                "failed to inspect tree entry while listing '{}' in {:?} at '{}'",
                listed_path, repo_path, git_ref
            )
        })?;
        let oid = entry.oid();
        let name = entry.filename().to_string();
        let kind = TreeEntryKind::from_mode(entry.mode());

        let full_path = if sub_path.is_empty() {
            name.clone()
        } else {
            format!("{}/{}", sub_path, name)
        };

        let size = if kind.is_blob() {
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
    let repo = rg_git::repository::open(repo_path)
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

    // Exactly two outcomes below belong to the client: the `ref:path` pair does
    // not resolve, and it resolves to something that is not a file. They carry
    // `rg_core::error::NotFound` / `InvalidRequest` so the HTTP layer can name
    // them; everything else in this function — a repository that will not open,
    // an unreadable object header, an object whose header lied — is ours and
    // stays a 5xx. The whole function used to be flattened into one
    // `AppError::not_found(e)` at the call site, which reported a broken
    // repository as an absent file *and* put `repo_path` in the response body
    // (404s are not sanitized by `IntoResponse`, H-05).
    //
    // `rev_parse_single("{ref}:{path}")` could not hold that line on its own:
    // one error value covered the missing ref, the missing path, an unreadable
    // ref store and a commit or tree that had left the object store, so a
    // repository losing objects kept answering `404 file not found`
    // (card_7cb31c61cee2). Ref and path are resolved separately below, each
    // through an API whose `None` means absence and nothing else.
    let commit_id = resolve_content_ref(&repo, git_ref, repo_path)?;

    let tree = commit_id
        .object()
        .with_context(|| {
            format!(
                "reading the object of ref '{}' in {} before reading a file from it",
                git_ref,
                repo_path.display()
            )
        })?
        .peel_to_tree()
        .with_context(|| {
            format!(
                "resolving the tree of ref '{}' in {}",
                git_ref,
                repo_path.display()
            )
        })?;

    let Some(entry) = lookup_tree_path(&repo, tree, path, repo_path, git_ref)? else {
        return Err(
            anyhow::Error::new(rg_core::error::NotFound::new("file")).context(format!(
                "path '{}' is not in {:?} at '{}'",
                path, repo_path, git_ref
            )),
        );
    };
    // A gitlink names a commit in another repository. Its oid is deliberately
    // absent from this repository's object store, so trying to inspect that
    // header would turn a valid non-file path into a storage-failure 500.
    // Classify from the tree mode first; a real blob whose object disappeared
    // still reaches `find_header` below and correctly remains a 5xx.
    if !entry.kind.is_blob() {
        return Err(rg_core::error::invalid_request("path is not a file"));
    }
    let object_id = entry.id;

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
    let repo = rg_git::repository::open(repo_path)
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

/// A tree entry located by [`lookup_tree_path`]. Only the two facts both
/// content endpoints need travel out of the walk, so no tree buffer has to stay
/// borrowed while the caller decides what the entry means.
struct TreePathEntry {
    id: gix::ObjectId,
    kind: TreeEntryKind,
}

/// Walk `path` inside `tree`, keeping "this path is not in that tree" apart
/// from "a tree on the way could not be read".
///
/// `Ok(None)` is the single honest absence: some component is not listed, or a
/// component before the last one is not a directory, so no such path exists in
/// this commit. A tree object that will not load or will not decode stays an
/// `Err` — `gix`'s own `Tree::lookup_entry_by_path` cannot be used for this,
/// because it drops undecodable entries (`filter_map(Result::ok)`) and so
/// reports a corrupt tree as a missing path, which is exactly the collapse this
/// walk exists to avoid.
fn lookup_tree_path(
    repo: &gix::Repository,
    tree: gix::Tree<'_>,
    path: &str,
    repo_path: &std::path::Path,
    git_ref: &str,
) -> anyhow::Result<Option<TreePathEntry>> {
    let mut tree = tree;
    let mut components = path.split('/').filter(|c| !c.is_empty()).peekable();

    while let Some(component) = components.next() {
        let mut matching_entry = None;
        for entry in tree.iter() {
            let entry = entry.with_context(|| {
                format!(
                    "failed to inspect tree while resolving path '{}' at component '{}' in {:?} at '{}'",
                    path, component, repo_path, git_ref
                )
            })?;
            if entry.filename() == component {
                matching_entry = Some(TreePathEntry {
                    id: entry.oid().to_owned(),
                    kind: TreeEntryKind::from_mode(entry.mode()),
                });
                break;
            }
        }

        let Some(entry) = matching_entry else {
            return Ok(None);
        };
        if components.peek().is_none() {
            return Ok(Some(entry));
        }
        if !entry.kind.is_tree() {
            // A deeper path underneath a file cannot exist. That is the client
            // naming a path this commit does not have, not a storage fault.
            return Ok(None);
        }

        tree = repo.find_tree(entry.id).with_context(|| {
            format!(
                "failed to read directory '{}' while resolving path '{}' in {:?} at '{}'",
                component, path, repo_path, git_ref
            )
        })?;
    }

    // Only reachable for a path with no non-empty components (`""`, `"/"`).
    Ok(None)
}

/// Resolve the documented ref forms of the content endpoints — branch, tag,
/// commit SHA, `HEAD` — without asking `rev_parse_single` to encode both
/// absence and a broken ref/object store in its one error value.
fn resolve_content_ref<'repo>(
    repo: &'repo gix::Repository,
    git_ref: &str,
    repo_path: &std::path::Path,
) -> anyhow::Result<gix::Id<'repo>> {
    use gix::prelude::ObjectIdExt as _;

    if git_ref == "HEAD" {
        let head = repo.head().with_context(|| {
            format!(
                "reading HEAD before resolving content ref in {}",
                repo_path.display()
            )
        })?;

        return head
            .try_into_peeled_id()
            .with_context(|| format!("resolving HEAD in {}", repo_path.display()))?
            .ok_or_else(|| anyhow::Error::new(rg_core::error::NotFound::new("ref")));
    }

    // A spelling Git's own ref grammar rejects is a deterministic client
    // mistake, not a storage failure. `try_find_reference` refuses `main^`,
    // `@{-1}`, `refs/heads/-x` and `a..b` too, but only as an anonymous gix
    // validation error, which `AppError::from` could report to the caller as
    // nothing but a 500 — the one answer that invites a retry of a request that
    // can never succeed.
    //
    // A commit SHA, full or abbreviated, needs no exception here: hex digits
    // spell a legal branch name, so a SHA passes this gate and is resolved
    // against the object database further down.
    let malformed = if git_ref.starts_with("refs/") {
        rg_git::refname::validate_refname(git_ref).is_err()
    } else {
        rg_git::refname::validate_branch_name(git_ref).is_err()
    };
    if malformed {
        // Fixed text: the rejected ref is caller-chosen and reaches the client
        // verbatim in the response body, the same reason
        // `validate_repo_file_path` does not echo its input.
        return Err(rg_core::error::invalid_request("invalid ref name"));
    }

    // The API documents branch, tag, and fully qualified ref names. The
    // `Option` from this lookup is the one honest "the client named no such
    // ref" outcome; malformed ref data and I/O errors remain server failures.
    let ref_names = if git_ref.starts_with("refs/") {
        vec![git_ref.to_owned()]
    } else {
        vec![
            format!("refs/heads/{git_ref}"),
            format!("refs/tags/{git_ref}"),
        ]
    };
    for ref_name in ref_names {
        let Some(mut reference) =
            repo.try_find_reference(ref_name.as_str())
                .with_context(|| {
                    format!(
                        "looking up content ref '{}' in {}",
                        ref_name,
                        repo_path.display()
                    )
                })?
        else {
            continue;
        };

        return reference.peel_to_id().with_context(|| {
            format!(
                "resolving content ref '{}' in {}",
                ref_name,
                repo_path.display()
            )
        });
    }

    // A commit SHA — full or abbreviated — is also a documented ref form. It
    // has no reference namespace to look up, so it is resolved against the
    // object database, and only after the named-ref branch above has ruled out
    // a real reference of the same spelling. `lookup_prefix` is what keeps the
    // boundary honest here too: its `None` is "no object has this id", while an
    // object database that cannot be scanned is an `Err`. Routed through
    // `rev_parse_single` both came back as `404 ref not found`.
    if let Ok(prefix) = gix::hash::Prefix::from_hex(git_ref) {
        let resolution = repo.objects.lookup_prefix(prefix, None).with_context(|| {
            format!(
                "looking up content ref '{}' in the object database of {}",
                git_ref,
                repo_path.display()
            )
        })?;
        return match resolution {
            None => Err(anyhow::Error::new(rg_core::error::NotFound::new("ref"))),
            Some(Ok(id)) => Ok(id.attach(repo)),
            // An ambiguous SHA prefix is a malformed request, not an absent ref
            // and not a storage failure.
            Some(Err(())) => Err(rg_core::error::invalid_request("ref SHA is ambiguous")),
        };
    }

    Err(anyhow::Error::new(rg_core::error::NotFound::new("ref")))
}

fn get_commit_log(
    repo_path: &std::path::Path,
    git_ref: &str,
    _path: &str,
    limit: usize,
) -> anyhow::Result<Vec<CommitEntry>> {
    let repo = rg_git::repository::open(repo_path)
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

    let mut entries = Vec::new();

    let head_id = resolve_content_ref(&repo, git_ref, repo_path)?;

    // Resolving a ref only proves that its name carries an object id; it does
    // not prove that the object store can supply that commit. Read it before
    // starting the best-effort walk so a missing HEAD/branch target is a 5xx,
    // never a plausible `404 ref not found` or an empty successful history.
    head_id.object().with_context(|| {
        format!(
            "reading commit-log target object for ref '{}' in {}",
            git_ref,
            repo_path.display()
        )
    })?;

    // A commit log is a snapshot: returning the readable prefix after any
    // traversal or decoding failure is indistinguishable from healthy history.
    // Keep the repository/ref/object context for the server log, but fail the
    // whole request so the client receives a sanitized 5xx instead of a lie.
    // Use rev_walk to traverse commit history.
    let walk = repo.rev_walk([head_id]);
    let walk_iter = walk.all().with_context(|| {
        format!(
            "starting commit-log walk for ref '{}' in {}",
            git_ref,
            repo_path.display()
        )
    })?;
    for (count, info) in walk_iter.enumerate() {
        if count >= limit {
            break;
        }

        let info = info.with_context(|| {
            format!(
                "reading commit-log entry for ref '{}' in {}",
                git_ref,
                repo_path.display()
            )
        })?;

        let commit_id = info.id;
        let object = repo.find_object(commit_id).with_context(|| {
            format!(
                "reading commit '{}' while walking ref '{}' in {}",
                commit_id,
                git_ref,
                repo_path.display()
            )
        })?;

        let commit = object.try_into_commit().with_context(|| {
            format!(
                "decoding commit '{}' while walking ref '{}' in {}",
                commit_id,
                git_ref,
                repo_path.display()
            )
        })?;

        let message = commit
            .message_raw()
            .with_context(|| {
                format!(
                    "decoding message for commit '{}' while walking ref '{}' in {}",
                    commit_id,
                    git_ref,
                    repo_path.display()
                )
            })?
            .to_string();
        let first_line = message.lines().next().unwrap_or("").to_string();

        let author = commit.author().with_context(|| {
            format!(
                "decoding author for commit '{}' while walking ref '{}' in {}",
                commit_id,
                git_ref,
                repo_path.display()
            )
        })?;
        let author_name = String::from_utf8_lossy(author.name).to_string();
        let author_email = String::from_utf8_lossy(author.email).to_string();
        // Parse author time from the signature string (format: "timestamp offset")
        // e.g., "1700000000 +0000"
        let timestamp = author
            .time
            .split_whitespace()
            .next()
            .unwrap_or("")
            .parse::<i64>()
            .with_context(|| {
                format!(
                    "parsing author timestamp for commit '{}' while walking ref '{}' in {}",
                    commit_id,
                    git_ref,
                    repo_path.display()
                )
            })?;
        let author_date = chrono::DateTime::<chrono::Utc>::from_timestamp(timestamp, 0)
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "author timestamp {} is out of range for commit '{}' while walking ref '{}' in {}",
                    timestamp,
                    commit_id,
                    git_ref,
                    repo_path.display()
                )
            })?
            .to_rfc3339();

        entries.push(CommitEntry {
            sha: commit_id.to_string(),
            author_name,
            author_email,
            author_date,
            message: first_line,
            gpg_signature: None,
        });
    }

    Ok(entries)
}

/// Enumerate the repository's branches, marking the one `HEAD` points at.
///
/// `HEAD` is read from the same open repository as the branch refs, so the
/// snapshot is internally consistent. A detached or unreadable-target `HEAD`
/// simply yields no default branch — that is a legitimate repository state and
/// must not fail the listing — whereas an unreadable *branch* ref still fails
/// the whole read (card_9fcb45a0018d: a shortened list would falsely claim the
/// omitted branch does not exist).
fn list_branch_refs(repo_path: &std::path::Path) -> anyhow::Result<Vec<BranchRef>> {
    let repo = rg_git::repository::open(repo_path)
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

    // An unborn HEAD still records the branch a first push must use, so this is
    // read before the branch list and is deliberately not fatal on its own.
    let default_branch = repo
        .head()
        .with_context(|| format!("failed to read HEAD in {}", repo_path.display()))?
        .referent_name()
        .map(|name| name.as_bstr().to_string())
        .and_then(|name| {
            name.strip_prefix("refs/heads/")
                .map(|branch| branch.to_string())
        });

    let references = repo.references()?;
    let mut branches = Vec::new();
    for reference in references.prefixed(b"refs/heads/".as_slice())? {
        match reference {
            Ok(reference) => {
                let name = reference.name().as_bstr();
                let stripped = &name["refs/heads/".len()..];
                let name = String::from_utf8_lossy(stripped).to_string();
                branches.push(BranchRef {
                    is_default: default_branch.as_deref() == Some(name.as_str()),
                    name,
                });
            }
            Err(e) => {
                return Err(anyhow::Error::from_boxed(e).context(format!(
                    "failed to read a branch reference in {}",
                    repo_path.display()
                )));
            }
        }
    }

    Ok(branches)
}

fn list_tag_names(repo_path: &std::path::Path) -> anyhow::Result<Vec<String>> {
    let repo = rg_git::repository::open(repo_path)
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

    let references = repo.references()?;
    let mut tags = Vec::new();
    for reference in references.tags()? {
        match reference {
            Ok(reference) => {
                let name = reference.name().as_bstr();
                let stripped = &name["refs/tags/".len()..];
                tags.push(String::from_utf8_lossy(stripped).to_string());
            }
            Err(e) => {
                return Err(anyhow::Error::from_boxed(e).context(format!(
                    "failed to read a tag reference in {}",
                    repo_path.display()
                )));
            }
        }
    }

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

    // `verify_commit_signature` opens a gix repository, decodes the commit
    // object, and — when a `gpgsig` header is present — shells out to
    // `git log --format=%G?…` through `GitCommandGateway::run`, whose
    // `recv_timeout` blocks the calling thread for up to `git_cmd_secs`. GPG
    // itself may then reach a keyserver, so "slow" here is not hypothetical.
    // Off the tokio runtime worker, matching the shape `list_issue_templates`
    // uses for the same class of shell-out.
    match tokio::task::spawn_blocking(move || verify_commit_signature(&repo_path, &sha)).await {
        Ok(Ok(sig)) => (StatusCode::OK, Json(sig)).into_response(),
        // See `get_blob`: an unopenable repository is not a missing commit.
        Ok(Err(e)) => AppError::from(e).into_response(),
        Err(join_error) => AppError::internal(join_error).into_response(),
    }
}

/// Verify a commit's GPG signature using `git log --show-signature`.
fn verify_commit_signature(repo_path: &std::path::Path, sha: &str) -> anyhow::Result<GpgSignature> {
    let repo = rg_git::repository::open(repo_path)
        .map_err(|e| crate::error::repository_storage_open_error(repo_path, e))?;

    let commit_id = resolve_signature_commit_id(&repo, sha, repo_path)?;

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
            verdict: SignatureVerdict::Unsigned,
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
    gpg_signature_from_output(&verify_output)
}

/// Resolve an object-id prefix without asking `rev_parse_single` to encode both
/// absence and a broken object store in one error value.
fn resolve_signature_commit_id(
    repo: &gix::Repository,
    sha: &str,
    repo_path: &std::path::Path,
) -> anyhow::Result<gix::hash::ObjectId> {
    let prefix = gix::hash::Prefix::from_hex(sha)
        .map_err(|_| rg_core::error::invalid_request("invalid commit SHA format"))?;
    let resolution = repo
        .objects
        .lookup_prefix(prefix, None)
        .with_context(|| format!("looking up commit '{sha}' in {}", repo_path.display()))?;

    match resolution {
        None => Err(anyhow::Error::new(rg_core::error::NotFound::new("commit"))),
        Some(Ok(commit_id)) => Ok(commit_id),
        // An ambiguous SHA prefix is a malformed client request, not an absent
        // commit and not a storage failure.
        Some(Err(())) => Err(rg_core::error::invalid_request("commit SHA is ambiguous")),
    }
}

/// Interpret a *successful* `git log` signature report. A Git process failure
/// is operational, not a legitimate verdict about the commit.
fn gpg_signature_from_output(
    verify_output: &rg_git::cli_gateway::GitOutput,
) -> anyhow::Result<GpgSignature> {
    verify_output
        .ensure_success()
        .context("git could not verify commit signature")?;

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

    // Codes per `git log --format=%G?`. The split that matters is not
    // good-versus-bad but checked-versus-unchecked: `E` and `U` are answers
    // about this instance's keyring, not about the commit.
    let (verdict, status): (SignatureVerdict, String) = match status_code {
        "G" => (SignatureVerdict::Valid, "valid".to_string()),
        // `X` is an expired *signature*, `Y` an expired *key* — two different
        // facts that used to share one label.
        "X" => (SignatureVerdict::Invalid, "expired_signature".to_string()),
        "Y" => (SignatureVerdict::Invalid, "expired_key".to_string()),
        "R" => (SignatureVerdict::Invalid, "revoked_key".to_string()),
        "B" => (SignatureVerdict::Invalid, "bad_signature".to_string()),
        // Good signature, unknown trust: the cryptography held, only this
        // instance's trust database has nothing to say about the key.
        "U" => (
            SignatureVerdict::Undeterminable,
            "untrusted_key".to_string(),
        ),
        // "signature can't be checked (e.g. missing key)" — never "expired".
        "E" => (SignatureVerdict::Undeterminable, "unverifiable".to_string()),
        "N" => (SignatureVerdict::Unsigned, "no_signature".to_string()),
        // A code this build does not know is one more thing we cannot check.
        _ => (
            SignatureVerdict::Undeterminable,
            format!("unknown_{}", status_code),
        ),
    };

    Ok(GpgSignature {
        verdict,
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
        (status = 413, description = "File content exceeds the API limit", body = serde_json::Value),
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
        actor_id,
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
        DeleteFileQuery,
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
        actor_id,
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
    let repo = rg_git::repository::open(repo_path)
        .with_context(|| format!("failed to open repository: {:?}", repo_path))?;

    let reference = format!("refs/heads/{}", branch);
    let oid = repo
        .rev_parse_single(reference.as_str())
        .map_err(|e| anyhow::anyhow!("failed to resolve branch '{}': {}", branch, e))?;

    Ok(oid.to_string())
}

#[cfg(test)]
mod tests {
    use std::{io::Write as _, process::Command};

    use axum::response::IntoResponse;
    use rg_git::cli_gateway::GitOutput;

    use super::{
        classify_repo_emptiness, commit_log_limit, get_blob_content, get_commit_log,
        gpg_signature_from_output, head_without_branch_error, list_branch_refs, list_tag_names,
        list_tree_entries, AppError, RepoEmptiness, SignatureVerdict, TreeEntryKind,
    };

    #[derive(Clone, Default)]
    struct CapturedLogs(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

    /// A `git log --format=%G?%n%GK%n%GN%n%GE` report that succeeded, carrying
    /// `code` as its status letter.
    fn signature_report(code: &str) -> GitOutput {
        GitOutput {
            stdout: format!("{code}\nkey\nSigner\nsigner@example.com\n").into_bytes(),
            stderr: Vec::new(),
            status: Command::new("true").status().expect("true must run"),
            command: "git log --format=%G?".to_string(),
        }
    }

    fn overwrite_loose_object(repo_path: &std::path::Path, oid: &str, kind: &str, data: &[u8]) {
        let object_path = repo_path.join("objects").join(&oid[..2]).join(&oid[2..]);
        std::fs::remove_file(&object_path).expect("existing loose object must be removable");
        let file = std::fs::File::create(object_path).expect("loose object must be writable");
        let mut encoder = flate2::write::ZlibEncoder::new(file, flate2::Compression::default());
        write!(encoder, "{kind} {}\0", data.len()).expect("object header must compress");
        encoder
            .write_all(data)
            .expect("object payload must compress");
        encoder.finish().expect("object must finish compressing");
    }

    /// A parent repository whose `vendor` entry is a real mode-160000 gitlink.
    /// The returned temporary directory owns both repositories for the caller.
    fn repository_with_submodule() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let inner = dir.path().join("inner");
        let worktree = dir.path().join("worktree");
        let git = rg_git::cli_gateway::global_gateway()
            .as_ref()
            .expect("git gateway must initialize");

        for repo in [&inner, &worktree] {
            git.run_or_bail(&["init", "-q", "-b", "main", repo.to_str().unwrap()], None)
                .unwrap();
            for args in [
                ["config", "user.name", "Repository content test"],
                ["config", "user.email", "repo-content@example.com"],
                ["config", "commit.gpgsign", "false"],
            ] {
                git.run_or_bail(&args, Some(repo)).unwrap();
            }
        }

        std::fs::write(inner.join("inner.txt"), "vendored\n").unwrap();
        git.run_or_bail(&["add", "inner.txt"], Some(&inner))
            .unwrap();
        git.run_or_bail(&["commit", "-qm", "inner commit"], Some(&inner))
            .unwrap();

        std::fs::write(worktree.join("README.md"), "# parent\n").unwrap();
        git.run_or_bail(
            &[
                "-c",
                "protocol.file.allow=always",
                "submodule",
                "add",
                "-q",
                inner.to_str().unwrap(),
                "vendor",
            ],
            Some(&worktree),
        )
        .unwrap();
        git.run_or_bail(&["add", "-A"], Some(&worktree)).unwrap();
        git.run_or_bail(&["commit", "-qm", "add submodule"], Some(&worktree))
            .unwrap();

        let repo_path = worktree.join(".git");
        let repo = gix::open(&repo_path).expect("parent repository must open");
        let gitlink = repo
            .head()
            .expect("HEAD must be readable")
            .try_into_peeled_id()
            .expect("HEAD must resolve")
            .expect("parent repository must have a commit")
            .object()
            .expect("parent commit must be readable")
            .peel_to_tree()
            .expect("parent tree must be readable")
            .lookup_entry_by_path("vendor")
            .expect("parent tree must decode")
            .expect("submodule must be in the parent tree")
            .object_id()
            .to_owned();
        assert!(
            repo.find_header(gitlink).is_err(),
            "the fixture must keep the gitlink target foreign to the parent object store"
        );
        drop(repo);

        (dir, repo_path)
    }

    /// `card_5ebf6d40cdab` — a gitlink is a tree leaf, but not a blob. Treating
    /// every non-tree mode as a blob emitted a false corruption warning while
    /// listing a healthy repository whose foreign commit is correctly absent.
    #[test]
    fn a_submodule_is_a_commit_entry_without_a_blob_size_warning() {
        let (_dir, repo_path) = repository_with_submodule();

        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let entries = list_tree_entries(&repo_path, "HEAD", "")
            .expect("a healthy tree containing a submodule must list");
        let vendor = entries
            .iter()
            .find(|entry| entry.name == "vendor")
            .unwrap_or_else(|| panic!("submodule must remain in the listing: {entries:?}"));
        assert_eq!(vendor.kind, TreeEntryKind::Commit);
        assert_eq!(
            vendor.size, None,
            "a gitlink has no parent-repository blob size"
        );
        assert!(
            !logs.text().contains("cannot read blob size"),
            "a healthy gitlink must not be logged as an unreadable blob: {}",
            logs.text()
        );
    }

    /// The independent read half of `card_5ebf6d40cdab`: classification must
    /// stop before `find_header`, because the foreign commit's absence is not a
    /// parent-repository storage failure.
    #[test]
    fn opening_a_submodule_as_a_blob_is_a_bad_request() {
        let (_dir, repo_path) = repository_with_submodule();

        let error = match get_blob_content(&repo_path, "HEAD", "vendor") {
            Ok(_) => panic!("a submodule path must not become readable as a blob"),
            Err(error) => error,
        };
        let response = AppError::from(error).into_response();
        assert_eq!(
            response.status(),
            axum::http::StatusCode::BAD_REQUEST,
            "opening a listed gitlink as a blob is a client-shape error, not a storage failure"
        );
    }

    #[test]
    fn commit_log_limit_rejects_non_positive_values_and_caps_large_ones() {
        for invalid in [i64::MIN, -1, 0] {
            let response = commit_log_limit(Some(invalid))
                .expect_err("a non-positive limit must be rejected")
                .into_response();
            assert_eq!(response.status(), axum::http::StatusCode::BAD_REQUEST);
        }

        assert_eq!(commit_log_limit(None).unwrap(), 50);
        assert_eq!(commit_log_limit(Some(1)).unwrap(), 1);
        assert_eq!(commit_log_limit(Some(200)).unwrap(), 100);
        assert_eq!(commit_log_limit(Some(i64::MAX)).unwrap(), 100);
    }

    #[test]
    fn an_invalid_signature_is_a_negative_verification_result() {
        let signature = gpg_signature_from_output(&signature_report("B"))
            .expect("Git reported a signature result");
        assert_eq!(signature.verdict, SignatureVerdict::Invalid);
        assert_eq!(signature.status, "bad_signature");
    }

    /// The defect behind card_61b29791d099: `E` is "signature can't be checked
    /// (e.g. missing key)". On an instance that never imported contributor keys
    /// that is the answer for every signed commit, and reporting it as a
    /// negative verdict accuses each of them of carrying a forged signature.
    #[test]
    fn a_signature_git_could_not_check_is_not_a_negative_verdict() {
        let signature = gpg_signature_from_output(&signature_report("E"))
            .expect("Git reported a signature result");
        assert_eq!(
            signature.verdict,
            SignatureVerdict::Undeterminable,
            "a missing public key says nothing about the commit"
        );
        assert!(
            !signature.status.contains("expired"),
            "`E` is not an expiry — nothing here has run out of time: {}",
            signature.status
        );
    }

    /// `U` is a good signature under a key this instance has no trust setting
    /// for. The cryptography held; only the keyring is silent.
    #[test]
    fn an_untrusted_key_is_not_a_bad_signature() {
        let signature = gpg_signature_from_output(&signature_report("U"))
            .expect("Git reported a signature result");
        assert_eq!(signature.verdict, SignatureVerdict::Undeterminable);
        assert_eq!(signature.status, "untrusted_key");
    }

    /// `X` is an expired *signature*, `Y` an expired *key*. Both are negative
    /// verdicts, but they are not the same fact and must not share a label.
    #[test]
    fn an_expired_signature_and_an_expired_key_stay_distinguishable() {
        let expired_signature = gpg_signature_from_output(&signature_report("X"))
            .expect("Git reported a signature result");
        let expired_key = gpg_signature_from_output(&signature_report("Y"))
            .expect("Git reported a signature result");

        assert_eq!(expired_signature.verdict, SignatureVerdict::Invalid);
        assert_eq!(expired_key.verdict, SignatureVerdict::Invalid);
        assert_ne!(
            expired_signature.status, expired_key.status,
            "an expired signature and an expired key are two different findings"
        );
    }

    /// A status letter this build does not know is one more thing it could not
    /// check — guessing "invalid" would invent evidence.
    #[test]
    fn an_unknown_status_code_is_undeterminable() {
        let signature = gpg_signature_from_output(&signature_report("Q"))
            .expect("Git reported a signature result");
        assert_eq!(signature.verdict, SignatureVerdict::Undeterminable);
        assert_eq!(signature.status, "unknown_Q");
    }

    /// The serialized shape the commit page reads: a discriminator, never a
    /// boolean a consumer would have to fold the third answer into.
    #[test]
    fn the_verdict_reaches_the_client_as_its_own_field() {
        let body = serde_json::to_value(
            gpg_signature_from_output(&signature_report("E"))
                .expect("Git reported a signature result"),
        )
        .expect("a signature report must serialize");

        assert_eq!(body["verdict"], "undeterminable");
        assert!(
            body.get("verified").is_none(),
            "a boolean next to the verdict would keep the old accusation alive: {body}"
        );
    }

    #[test]
    fn a_failed_signature_verification_command_is_a_server_error() {
        let output = GitOutput {
            stdout: Vec::new(),
            stderr: b"git operational failure".to_vec(),
            status: Command::new("false").status().expect("false must run"),
            command: "git log --format=%G?".to_string(),
        };

        let error = match gpg_signature_from_output(&output) {
            Ok(_) => panic!("a failed Git process must not become a verdict about the commit"),
            Err(error) => error,
        };
        let response = AppError::from(error).into_response();
        assert!(
            response.status().is_server_error(),
            "a failed Git process must reach the HTTP layer as 5xx"
        );
    }

    #[test]
    fn server_side_commit_signature_verifier_failure_is_still_a_server_error() {
        let repo = tempfile::tempdir().unwrap();
        rg_git::cli_gateway::global_gateway()
            .as_ref()
            .unwrap()
            .run(&["init"], Some(repo.path()))
            .unwrap()
            .ensure_success()
            .unwrap();

        let error = rg_git::protocol::receive_pack::unsigned_commit_for_required_signature(
            repo.path(),
            "0000000000000000000000000000000000000000",
            "not-a-commit",
            "refs/heads/main",
            &["refs/heads/main".to_string()],
        )
        .expect_err("an unreadable created commit must be operational failure");
        let response = AppError::from(anyhow::Error::new(error)).into_response();
        assert!(
            response.status().is_server_error(),
            "a verifier outage must stay 5xx, not become a 403 policy refusal"
        );
    }

    /// The ordinary positive case the empty-tree response exists for: a repo
    /// that was created but never pushed to.
    #[test]
    fn a_repository_without_commits_is_empty() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("fresh.git");
        gix::init_bare(&repo_path).expect("a bare repo must initialise");

        assert!(
            matches!(classify_repo_emptiness(&repo_path), RepoEmptiness::Empty),
            "an unborn HEAD over no branches at all is the one state that \
             legitimately means `no commits yet`"
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
            matches!(classify_repo_emptiness(&repo_path), RepoEmptiness::NotEmpty),
            "a HEAD that cannot be read is an unknown state, not an empty repository"
        );
    }

    /// A repository that cannot be opened at all is likewise unknown, not
    /// empty — this arm was already correct and must stay that way.
    #[test]
    fn a_path_that_is_not_a_repository_is_not_reported_as_empty() {
        let dir = tempfile::tempdir().unwrap();

        assert!(matches!(
            classify_repo_emptiness(&dir.path().join("nothing-here.git")),
            RepoEmptiness::NotEmpty
        ));
    }

    /// card_9e11f76dddd1: the state the old `bool` could not express. HEAD is
    /// unborn — it names a branch that does not exist — but `refs/heads/*` is
    /// full of history. Answering "empty" here drew a complete repository as
    /// the "push an existing repository" empty state, with nothing in the
    /// response or the log saying the branch was simply named differently.
    #[tokio::test]
    async fn an_unborn_head_over_existing_branches_is_a_desync_not_an_empty_repository() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("named-differently");
        let git = |args: &[&str]| {
            let output = rg_git::cli_gateway::global_gateway()
                .as_ref()
                .expect("git gateway must initialize")
                .run(args, Some(&worktree))
                .expect("git must run");
            assert!(
                output.success(),
                "git {args:?} failed: {}",
                output.stderr_str()
            );
        };

        std::fs::create_dir_all(&worktree).unwrap();
        git(&["init", "-q", "-b", "master"]);
        git(&["config", "user.name", "ForgeKeep Test"]);
        git(&["config", "user.email", "forgekeep@example.test"]);
        std::fs::write(worktree.join("file.txt"), "content\n").unwrap();
        git(&["add", "file.txt"]);
        git(&["commit", "-q", "-m", "history that HEAD cannot name"]);

        let repo_path = worktree.join(".git");
        // The exact shape a repository ends up in when its first push named a
        // branch the pre-created HEAD never heard of.
        std::fs::write(repo_path.join("HEAD"), "ref: refs/heads/main\n").unwrap();

        let RepoEmptiness::HeadWithoutBranch { head, branches } =
            classify_repo_emptiness(&repo_path)
        else {
            panic!("history behind an unborn HEAD must not be classified as empty or unknown");
        };
        assert_eq!(head, "refs/heads/main");
        assert_eq!(branches, vec!["master".to_string()]);

        // The diagnosis has to be readable from the response itself — a
        // sanitized 5xx would send the operator to the server instead.
        let error = head_without_branch_error(&head, &branches);
        assert_eq!(error.status(), axum::http::StatusCode::CONFLICT);
        let response = error.into_response();
        assert_eq!(response.status(), axum::http::StatusCode::CONFLICT);
        let body = axum::body::to_bytes(response.into_body(), 64 * 1024)
            .await
            .expect("error body must be readable");
        let body = String::from_utf8(body.to_vec()).expect("error body must be UTF-8");
        for expected in ["refs/heads/main", "master"] {
            assert!(
                body.contains(expected),
                "the desync response must name both sides, missing {expected:?}: {body}"
            );
        }
        assert!(
            !body.contains("Internal server error"),
            "a diagnosable repository state must not be sanitized away: {body}"
        );
    }

    /// The branch list in the desync message is a diagnosis, not a listing: a
    /// repository with hundreds of branches must not turn one error body into
    /// a dump of all of them.
    #[test]
    fn the_desync_message_samples_a_long_branch_list() {
        let branches: Vec<String> = (0..25).map(|i| format!("branch-{i:02}")).collect();

        let error = head_without_branch_error("refs/heads/main", &branches);
        let rendered = error.to_string();

        assert!(rendered.contains("branch-00"), "{rendered}");
        assert!(
            !rendered.contains("branch-10"),
            "only the first {} names belong in the message: {rendered}",
            super::DESYNC_BRANCH_SAMPLE
        );
        assert!(rendered.contains("and 15 more"), "{rendered}");
    }

    /// card_c9c2a0d88340 / card_58d30e3cb060: a malformed entry encountered
    /// while resolving `?path=` or listing the root tree must remain a
    /// repository failure. Dropping the iterator error turns the sub-path case
    /// into a typed `path not found` 404, and the root case into a partial 200,
    /// even though the client cannot fix a corrupt tree object.
    #[test]
    fn unreadable_tree_entries_are_server_errors_during_sub_path_lookup_and_root_listing() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("broken-tree");
        let git = |args: &[&str]| {
            let output = rg_git::cli_gateway::global_gateway()
                .as_ref()
                .expect("git gateway must initialize")
                .run(args, Some(&worktree))
                .expect("git must run");
            assert!(
                output.success(),
                "git {args:?} failed: {}",
                output.stderr_str()
            );
            output.stdout_str().trim().to_string()
        };

        std::fs::create_dir_all(worktree.join("requested-dir")).unwrap();
        git(&["init", "-q"]);
        git(&["config", "user.name", "ForgeKeep Test"]);
        git(&["config", "user.email", "forgekeep@example.test"]);
        std::fs::write(worktree.join("requested-dir/file.txt"), "content\n").unwrap();
        git(&["add", "requested-dir/file.txt"]);
        git(&["commit", "-q", "-m", "tree fixture"]);

        let tree_oid = git(&["rev-parse", "HEAD^{tree}"]);
        let repo_path = worktree.join(".git");
        overwrite_loose_object(&repo_path, &tree_oid, "tree", b"x");

        let repo = gix::open(&repo_path).expect("repository must still open");
        let commit_id = repo
            .rev_parse_single("HEAD")
            .expect("HEAD must still resolve");
        let commit = repo
            .find_commit(commit_id)
            .expect("commit must remain readable");
        let tree_oid = commit.decode().expect("commit must decode").tree();
        let tree = repo
            .find_tree(tree_oid)
            .expect("tree object header must remain readable");
        assert!(
            tree.iter()
                .next()
                .expect("malformed tree must expose one iterator result")
                .is_err(),
            "fixture must fail while decoding a tree entry"
        );

        let error = match list_tree_entries(&repo_path, "HEAD", "requested-dir") {
            Ok(_) => panic!("tree corruption must not become a successful listing"),
            Err(error) => error,
        };
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("requested-dir"),
            "error needs path context: {rendered}"
        );
        assert!(
            rendered.contains("HEAD"),
            "error needs ref context: {rendered}"
        );
        assert!(
            rendered.contains(&repo_path.display().to_string()),
            "error needs repository context: {rendered}"
        );

        let response = AppError::from(error).into_response();
        assert!(
            response.status().is_server_error(),
            "an unreadable tree entry must stay 5xx, got {}",
            response.status()
        );

        let root_error = match list_tree_entries(&repo_path, "HEAD", "") {
            Ok(_) => panic!("root-tree corruption must not become a partial successful listing"),
            Err(error) => error,
        };
        let rendered = format!("{root_error:#}");
        assert!(
            rendered.contains("<root>"),
            "root-listing error needs root-path context: {rendered}"
        );
        assert!(
            rendered.contains("HEAD"),
            "root-listing error needs ref context: {rendered}"
        );
        assert!(
            rendered.contains(&repo_path.display().to_string()),
            "root-listing error needs repository context: {rendered}"
        );

        let response = AppError::from(root_error).into_response();
        assert!(
            response.status().is_server_error(),
            "an unreadable root-tree entry must stay 5xx, got {}",
            response.status()
        );
    }

    /// A reachable parent that cannot be read makes the commit log incomplete.
    /// It must fail rather than looking like a shorter healthy history.
    #[test]
    fn an_unreadable_commit_fails_with_repository_ref_and_object_context() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("broken-history");
        let git = |args: &[&str]| {
            let output = rg_git::cli_gateway::global_gateway()
                .as_ref()
                .expect("git gateway must initialize")
                .run(args, Some(&worktree))
                .expect("git must run");
            assert!(
                output.success(),
                "git {args:?} failed: {}",
                output.stderr_str()
            );
            output.stdout_str().trim().to_string()
        };

        std::fs::create_dir_all(&worktree).unwrap();
        git(&["init", "-q"]);
        git(&["config", "user.name", "ForgeKeep Test"]);
        git(&["config", "user.email", "forgekeep@example.test"]);
        std::fs::write(worktree.join("history.txt"), "first\n").unwrap();
        git(&["add", "history.txt"]);
        git(&["commit", "-q", "-m", "first"]);
        let missing_commit = git(&["rev-parse", "HEAD"]);
        std::fs::write(worktree.join("history.txt"), "second\n").unwrap();
        git(&["commit", "-q", "-am", "second"]);

        let repo_path = worktree.join(".git");
        std::fs::remove_file(
            repo_path
                .join("objects")
                .join(&missing_commit[..2])
                .join(&missing_commit[2..]),
        )
        .expect("the parent commit must be a loose object");

        let error = match get_commit_log(&repo_path, "HEAD", "", 50) {
            Ok(_) => panic!("a reachable unreadable commit must fail the complete history read"),
            Err(error) => error,
        };
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&repo_path.display().to_string()),
            "error must name the repository: {rendered}"
        );
        assert!(
            rendered.contains("ref 'HEAD'"),
            "error must name the requested ref: {rendered}"
        );
        assert!(
            rendered.contains(&missing_commit),
            "error must name the unreadable object: {rendered}"
        );
    }

    /// The branch picker marks the default branch itself. Before this the
    /// endpoint answered with bare names and the browser's `is_default` markup
    /// could never light up (card_ee7e4c250ca0), so the marker is asserted
    /// against Git's own `HEAD` rather than against the database mirror of it.
    #[test]
    fn branch_listing_marks_the_branch_head_points_at() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("defaulted.git");
        gix::init_bare(&repo_path).expect("a bare repo must initialise");
        let oid = "0123456789abcdef0123456789abcdef01234567";
        std::fs::create_dir_all(repo_path.join("refs/heads")).unwrap();
        for branch in ["main", "release"] {
            std::fs::write(
                repo_path.join("refs/heads").join(branch),
                format!("{oid}\n"),
            )
            .unwrap();
        }
        std::fs::write(repo_path.join("HEAD"), "ref: refs/heads/release\n").unwrap();

        let branches = list_branch_refs(&repo_path).expect("branch listing must succeed");
        let marked: Vec<&str> = branches
            .iter()
            .filter(|b| b.is_default)
            .map(|b| b.name.as_str())
            .collect();
        assert_eq!(
            marked,
            vec!["release"],
            "exactly the branch HEAD points at must be marked as default"
        );
        assert_eq!(branches.len(), 2, "every branch must still be listed");
    }

    /// A detached `HEAD` is a legitimate repository state, not a failure: the
    /// picker must still list every branch, just without a default marker.
    #[test]
    fn branch_listing_survives_a_detached_head() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("detached.git");
        gix::init_bare(&repo_path).expect("a bare repo must initialise");
        let oid = "0123456789abcdef0123456789abcdef01234567";
        std::fs::create_dir_all(repo_path.join("refs/heads")).unwrap();
        std::fs::write(repo_path.join("refs/heads/main"), format!("{oid}\n")).unwrap();
        std::fs::write(repo_path.join("HEAD"), format!("{oid}\n")).unwrap();

        let branches = list_branch_refs(&repo_path).expect("branch listing must succeed");
        assert_eq!(branches.len(), 1, "the branch must still be listed");
        assert!(
            !branches[0].is_default,
            "a detached HEAD marks no branch as default"
        );
    }

    /// A ref picker is a snapshot, not a best-effort hint: one unreadable ref
    /// must fail the whole read instead of pretending the omitted ref is absent.
    #[test]
    fn unreadable_branch_and_tag_refs_fail_with_context() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken-refs.git");
        gix::init_bare(&repo_path).expect("a bare repo must initialise");
        std::fs::create_dir_all(repo_path.join("refs/heads")).unwrap();
        std::fs::create_dir_all(repo_path.join("refs/tags")).unwrap();
        std::fs::write(repo_path.join("refs/heads/broken"), "not an object id\n").unwrap();
        std::fs::write(repo_path.join("refs/tags/broken"), "not an object id\n").unwrap();

        let branch_error = list_branch_refs(&repo_path).expect_err("broken branch ref must fail");
        let tag_error = list_tag_names(&repo_path).expect_err("broken tag ref must fail");
        assert!(
            branch_error
                .to_string()
                .contains(&repo_path.display().to_string()),
            "branch error must name the repository: {branch_error:#}"
        );
        assert!(
            branch_error.to_string().contains("branch reference"),
            "branch error must identify the failing namespace: {branch_error:#}"
        );
        assert!(
            tag_error
                .to_string()
                .contains(&repo_path.display().to_string()),
            "tag error must name the repository: {tag_error:#}"
        );
        assert!(
            tag_error.to_string().contains("tag reference"),
            "tag error must identify the failing namespace: {tag_error:#}"
        );
    }
}
