//! Repository archive download.
//!
//! GET /api/v1/repos/{owner}/{name}/archive/{sha}.zip
//! GET /api/v1/repos/{owner}/{name}/archive/{sha}.tar.gz

use crate::AppState;
use axum::{
    extract::{Path, State},
    http::{header, StatusCode},
    response::IntoResponse,
};

use crate::api::repo_access::{CiRead, RepoContents};
use crate::error::AppError;

/// The one 4xx message this endpoint has for a tree-ish it could not resolve.
///
/// Fixed text on purpose: a `400` body is *not* sanitized by
/// `AppError::into_response`, so anything built from git's own stderr would
/// reach the client verbatim (H-05).
const BAD_TREE_ISH: &str = "invalid ref or SHA";

/// Which `git archive` failures are the caller's fault.
///
/// git exits `128` for almost everything fatal — a mistyped ref, a directory
/// that is not a repository, an unreadable object database — so the exit code
/// alone cannot tell the client's mistake from ours. Only these two messages
/// mean "what you named is not a tree-ish":
///
/// - `fatal: not a valid object name: <ref>` — no such ref/oid (also what an
///   empty repository answers for `main`);
/// - `fatal: not a tree object: <oid>` — it resolved, but to a blob or a tag of
///   one.
///
/// Deliberately an allow-list rather than a deny-list: a git version that words
/// some *infrastructure* failure differently falls through to the 5xx side,
/// which is the safe direction. The opposite default is the bug this replaces —
/// it blamed the client for an outage and filed nothing in the alerts.
fn is_bad_tree_ish(stderr: &str) -> bool {
    stderr.contains("not a valid object name") || stderr.contains("not a tree object")
}

/// GET /api/v1/repos/{owner}/{name}/archive/{sha}.zip
#[utoipa::path(
    get,
    path = "/repos/{owner}/{name}/archive/{archive}",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
        ("archive" = String, Path, description = "Archive filename (e.g. main.zip, v1.0.tar.gz)"),
    ),
    responses(
        (status = 200, description = "Archive binary stream", content_type = "application/zip"),
        (status = 400, description = "Unsupported archive format, or invalid ref/SHA"),
        (status = 404, description = "Repository not found"),
        (status = 500, description = "The archive could not be produced"),
    ),
)]
pub async fn download_archive(
    State(state): State<AppState>,
    Path((owner, name, archive)): Path<(String, String, String)>,
    // The gate now runs before the filename is parsed, so an unreadable
    // repository answers 401/403 rather than telling a stranger which archive
    // extensions it would have accepted.
    CiRead::<RepoContents> { .. }: CiRead<RepoContents>,
) -> impl IntoResponse {
    // axum 0.8 allows only one parameter per path segment, so the filename
    // (`<sha>.<ext>`) arrives as a single `{archive}` segment that we split
    // here. `.tar.gz`/`.tgz` are checked before `.zip`.
    let (sha, ext) = if let Some(s) = archive.strip_suffix(".tar.gz") {
        (s.to_string(), "tar.gz")
    } else if let Some(s) = archive.strip_suffix(".tgz") {
        (s.to_string(), "tgz")
    } else if let Some(s) = archive.strip_suffix(".zip") {
        (s.to_string(), "zip")
    } else {
        return AppError::bad_request("Unsupported format").into_response();
    };

    let repo_path = state.repo_root.join(format!("{}/{}.git", owner, name));
    if !repo_path.exists() {
        return AppError::not_found("repository data not found").into_response();
    }

    let format_flag = match ext {
        "zip" => "zip",
        "tar.gz" | "tgz" => "tar.gz",
        _ => return AppError::bad_request("Unsupported format").into_response(),
    };

    let mime = match ext {
        "zip" => "application/zip",
        _ => "application/gzip",
    };

    let git = match rg_git::cli_gateway::global_gateway().as_ref() {
        Ok(g) => g,
        Err(e) => return AppError::internal(e.to_string()).into_response(),
    };

    // `sha` reaches `git archive` as a bare positional argument, so a value
    // starting with `-` is read by git as an *option* rather than as a tree-ish.
    // `git archive --remote=ssh://…` makes the server open an outbound git
    // connection to a host of the caller's choosing — the very thing
    // `rg_core::net::guard_git_url` is installed to prevent on every other
    // remote-facing path, reachable here through an argument that never looked
    // like a URL. No ref name and no object id may begin with `-`
    // (`git check-ref-format` refuses it), so refusing it costs no legitimate
    // request and it is the same answer git would give anyway.
    if sha.starts_with('-') {
        return AppError::bad_request(BAD_TREE_ISH).into_response();
    }

    let git_out = match git.run(
        &["archive", &format!("--format={}", format_flag), &sha],
        Some(&repo_path),
    ) {
        Ok(o) => o,
        Err(e) => return AppError::from(e).into_response(),
    };

    let output = match git_out.ensure_success() {
        Ok(()) => git_out.stdout,
        Err(error) => {
            // The old arm was `Err(_) => bad_request("Invalid ref or SHA")`: the
            // error value was dropped rather than converted, so a bare
            // repository that will not open, a missing object database or a
            // full disk all answered `400` and left nothing in the operator log
            // — the client was told to fix a ref that was fine, and the outage
            // reached neither its retry logic nor the alerts.
            let error = if is_bad_tree_ish(&git_out.stderr_str()) {
                // This one really is the caller's: `sha` comes straight from the
                // URL and is not validated before the call. The marker keeps the
                // 400 while making the body `BAD_TREE_ISH`, so git's own wording
                // (which repeats the ref back) stays out of it.
                error.context(rg_core::error::InvalidRequest::new(BAD_TREE_ISH))
            } else {
                // Everything else is ours. `AppError::from` classifies it (504
                // on a git timeout, 500 otherwise) and logs the whole chain,
                // git stderr included, while `IntoResponse` keeps the body
                // generic (H-05).
                error.context("git archive failed")
            };
            return AppError::from(error).into_response();
        }
    };

    // Truncate by chars (not bytes) so a short ref like `main` or a
    // multi-byte ref name cannot panic on a non-char-boundary byte slice.
    let short: String = sha.chars().take(7).collect();
    let filename = format!("{}-{}.{}", name, short, ext);

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, mime),
            (
                header::CONTENT_DISPOSITION,
                &format!("attachment; filename=\"{}\"", filename),
            ),
        ],
        output,
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The strings in these two tests are the ones git 2.x actually prints; they
    /// were captured from a bare repository rather than written from memory.
    #[test]
    fn a_ref_git_cannot_resolve_is_the_callers_mistake() {
        assert!(is_bad_tree_ish(
            "fatal: not a valid object name: nosuchref\n"
        ));
        // An empty repository answers the same way for its default branch.
        assert!(is_bad_tree_ish("fatal: not a valid object name: main\n"));
        // Resolved, but to a blob (`main:README.md`) or a tag of one.
        assert!(is_bad_tree_ish(
            "fatal: not a tree object: 45b983be36b73c0788dc9cbcb76cbb80fc7bb057\n"
        ));
    }

    /// The half the old `Err(_) => bad_request(…)` arm got wrong: none of these
    /// is anything the client can fix, so all of them must fall through to the
    /// 5xx side and into the operator log.
    #[test]
    fn a_broken_repository_is_never_the_callers_mistake() {
        for stderr in [
            // What a bind-mount-created empty directory (or a repo with its
            // `objects/` gone) answers.
            "fatal: not a git repository (or any of the parent directories): .git\n",
            "fatal: bad object HEAD\n",
            "error: unable to read sha1 file\n",
            "fatal: unable to write file: No space left on device\n",
            "fatal: Unable to create '/srv/repos/acme/widgets.git/index.lock': Permission denied\n",
            "",
        ] {
            assert!(
                !is_bad_tree_ish(stderr),
                "must stay a 5xx, got a client error for: {stderr}"
            );
        }
    }

    /// The 400 branch must answer with the fixed message and not with git's,
    /// which echoes the ref the client sent back at it.
    #[test]
    fn the_bad_tree_ish_marker_becomes_a_400_with_the_fixed_message() {
        let err = anyhow::anyhow!("git archive --format=zip nosuchref failed (128): fatal: not a valid object name: nosuchref")
            .context(rg_core::error::InvalidRequest::new(BAD_TREE_ISH));
        let app_err = AppError::from(err);

        assert_eq!(app_err.status(), axum::http::StatusCode::BAD_REQUEST);
        let AppError::BadRequest(message) = &app_err else {
            panic!("expected BadRequest, got {app_err:?}");
        };
        assert_eq!(message, BAD_TREE_ISH);
        assert!(!message.contains("nosuchref"), "{message}");
    }

    /// And the other branch keeps git's stderr — for the log, where it belongs.
    #[test]
    fn an_infrastructure_failure_becomes_a_500_that_kept_the_reason() {
        let err = anyhow::anyhow!(
            "git archive --format=zip main failed (128): fatal: not a git repository"
        )
        .context("git archive failed");
        let app_err = AppError::from(err);

        assert_eq!(
            app_err.status(),
            axum::http::StatusCode::INTERNAL_SERVER_ERROR
        );
        let AppError::InternalError(logged) = &app_err else {
            panic!("expected InternalError, got {app_err:?}");
        };
        assert!(logged.contains("not a git repository"), "{logged}");
    }
}
