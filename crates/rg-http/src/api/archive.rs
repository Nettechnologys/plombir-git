//! Repository archive download.
//!
//! GET /api/v1/repos/{owner}/{name}/archive/{sha}.zip
//! GET /api/v1/repos/{owner}/{name}/archive/{sha}.tar.gz

use crate::AppState;
use axum::{
    body::Body,
    extract::{Path, State},
    http::{header, StatusCode},
    response::{IntoResponse, Response},
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
    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return AppError::from(e).into_response();
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

    // Stream `git archive` instead of running it synchronously and collecting
    // the result. The old call was `GitCommandGateway::run`, which is both a
    // `Vec` the size of the repository's archive *and* a blocking `recv_timeout`
    // on the calling thread — so one "Download ZIP" of a large repository held
    // the whole archive in memory and parked a tokio worker for up to
    // `git_cmd_secs` while it did (card_fbdae59573ca). Neither figure was chosen
    // by any configured limit; both were chosen by whoever clicked the button.
    let child = match git
        .spawn_async(
            &["archive", &format!("--format={}", format_flag), &sha],
            Some(&repo_path),
        )
        .await
    {
        Ok(child) => child,
        Err(error) => return AppError::from(error.context("git archive failed")).into_response(),
    };
    let mut stream = match crate::http_stream::split_git_child(child) {
        Ok(stream) => stream,
        Err(error) => {
            return AppError::from(anyhow::Error::from(error).context("git archive failed"))
                .into_response()
        }
    };

    // Read the first chunk before answering. Streaming and honest status codes
    // pull in opposite directions — once a byte of body is on the wire the
    // status is spent — and the split is this first read: until it returns
    // nothing has been sent, and everything git decides up front is still
    // answerable with a code. That is what keeps the `400 BAD_TREE_ISH` this
    // endpoint promises, because a ref git cannot resolve is refused before it
    // writes anything. It is the same split `git_http::stream_upload_pack_response`
    // makes for a clone.
    //
    // The read is idle-bounded too: an async-spawned git has no `git_cmd_secs`
    // wall-clock behind it, and the body's own idle guard cannot start until
    // there is a body — so a git that hangs before its first byte would
    // otherwise hold the request open with no bound at all.
    let idle_secs = state.git_idle_timeout_secs;
    let idle = (idle_secs > 0).then(|| std::time::Duration::from_secs(idle_secs));
    let mut head = vec![0u8; crate::http_stream::RESPONSE_CHUNK_BYTES];
    let read = {
        use tokio::io::AsyncReadExt as _;
        match idle {
            Some(dur) => match tokio::time::timeout(dur, stream.stdout.read(&mut head)).await {
                Ok(result) => result,
                // Dropping `stream` on the way out reaps git: `spawn_async` sets
                // `kill_on_drop`, so the hung process does not outlive the
                // request that gave up on it.
                Err(_elapsed) => {
                    return AppError::timeout(format!(
                        "git archive for {owner}/{name} produced nothing within {idle_secs}s"
                    ))
                    .into_response()
                }
            },
            None => stream.stdout.read(&mut head).await,
        }
    };
    let first = match read {
        Ok(first) => first,
        Err(error) => {
            return AppError::from(anyhow::Error::from(error).context("read git archive output"))
                .into_response()
        }
    };

    if first == 0 {
        // End of stream with nothing written: git is already finished (that is
        // what closed the pipe), so its verdict is the whole answer and no bytes
        // have committed us to a status yet.
        let status = stream.child.wait().await;
        let drained = stream.stderr.await.unwrap_or_default();
        let stderr = String::from_utf8_lossy(&drained);

        let code = match status {
            // An empty tree is a valid, empty archive rather than a failure.
            Ok(status) if status.success() => {
                return archive_response(&name, &sha, ext, mime, Body::empty())
            }
            Ok(status) => status.code(),
            Err(error) => {
                return AppError::from(anyhow::Error::from(error).context("git archive failed"))
                    .into_response()
            }
        };

        // The arm this replaces was `Err(_) => bad_request("Invalid ref or SHA")`:
        // the error value was dropped rather than converted, so a bare
        // repository that will not open, a missing object database or a full
        // disk all answered `400` and left nothing in the operator log — the
        // client was told to fix a ref that was fine, and the outage reached
        // neither its retry logic nor the alerts.
        let error = anyhow::anyhow!(
            "git archive --format={format_flag} failed ({}): {}",
            code.map_or_else(|| "signal".to_string(), |code| code.to_string()),
            stderr.trim()
        );
        let error = if is_bad_tree_ish(&stderr) {
            // This one really is the caller's: `sha` comes straight from the URL
            // and is not validated before the call. The marker keeps the 400
            // while making the body `BAD_TREE_ISH`, so git's own wording (which
            // repeats the ref back) stays out of it.
            error.context(rg_core::error::InvalidRequest::new(BAD_TREE_ISH))
        } else {
            // Everything else is ours. `AppError::from` classifies it and logs
            // the whole chain, git stderr included, while `IntoResponse` keeps
            // the body generic (H-05).
            error.context("git archive failed")
        };
        return AppError::from(error).into_response();
    }

    // Past this point the answer is a `200` whose body is git's stdout. A git
    // that fails from here on cannot take the status back, so the streamer
    // breaks the body rather than ending it — a truncated archive must not read
    // as a complete download.
    head.truncate(first);
    stream.head = axum::body::Bytes::from(head);
    let body = crate::http_stream::git_child_body_with_idle(
        stream,
        idle_secs,
        crate::http_stream::GitStreamSource {
            job_id: None,
            repo_path,
            what: "repository archive",
        },
    );
    archive_response(&name, &sha, ext, mime, body)
}

/// The `200` envelope both exits share: content type, and a filename that
/// survives a repository or ref name git was perfectly happy with.
fn archive_response(name: &str, sha: &str, ext: &str, mime: &'static str, body: Body) -> Response {
    // Truncate by chars (not bytes) so a short ref like `main` or a
    // multi-byte ref name cannot panic on a non-char-boundary byte slice.
    let short: String = sha.chars().take(7).collect();
    let filename = format!("{}-{}.{}", name, short, ext);

    let mut response = (StatusCode::OK, [(header::CONTENT_TYPE, mime)], body).into_response();
    // The archive name is built from the repository name and a ref, both of
    // which may be non-ASCII, and a `format!` into a header array made that a
    // `500` on a repository that is otherwise perfectly downloadable.
    response.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        crate::content_disposition::attachment(&filename),
    );
    response
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
