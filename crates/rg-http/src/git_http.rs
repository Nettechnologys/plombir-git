//! Git Smart HTTP protocol endpoints (`/info/refs`, `git-upload-pack`,
//! `git-receive-pack`) plus post-push hooks (CI, webhooks, notifications) and
//! branch/tag protection enforcement.

use anyhow::{Context, Result};
use axum::body::Body;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use sea_orm::DatabaseConnection;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use crate::pat_auth::{extract_git_credential, GitCredential};
use crate::{git_v2, AppState};
use rg_core::branch_protection::push_rules::{
    branch_protection_rejected_refs, signed_commit_required_refs, tag_protection_rejected_refs,
};

/// RAII timer that records a Git transport operation into the Prometheus
/// metrics on drop — so every return path of the (branch-heavy) pack handlers
/// is covered by a single construction site. Created only *after* the access
/// check passes, so unauthorized / 404 attempts are not counted as operations;
/// a timed-out or errored transfer still records (a slow push is real signal).
struct GitOpTimer {
    operation: &'static str,
    start: std::time::Instant,
}

impl GitOpTimer {
    fn new(operation: &'static str) -> Self {
        Self {
            operation,
            start: std::time::Instant::now(),
        }
    }
}

impl Drop for GitOpTimer {
    fn drop(&mut self) {
        crate::metrics::recorder::git_operation(self.operation, self.start.elapsed());
    }
}

/// Wall-clock guard around a streaming git protocol handler.
///
/// The protocol handlers (`handle_upload_pack_http`, `handle_v2_http`,
/// `handle_receive_pack_http_with_rejections`) spawn a `git` subprocess via
/// [`rg_git::cli_gateway::GitCommandGateway::spawn_async`], which only sets
/// `kill_on_drop(true)` and asks the caller to bound the I/O loop. Without a
/// bound a hung or pathologically slow git process holds the connection +
/// subprocess forever.
///
/// On timeout the inner future is dropped, which drops the `git` child held
/// inside the handler → `kill_on_drop` kills the subprocess. Callers then map
/// `Err(Elapsed)` to a `504 Gateway Timeout`.
///
/// `secs == 0` disables the bound (the future runs unbounded).
async fn with_git_timeout<T>(
    secs: u64,
    fut: impl std::future::Future<Output = T>,
) -> Result<T, tokio::time::error::Elapsed> {
    if secs == 0 {
        return Ok(fut.await);
    }
    tokio::time::timeout(std::time::Duration::from_secs(secs), fut).await
}

#[derive(Debug)]
struct StagedGitBody {
    file: tokio::fs::File,
    path: tempfile::TempPath,
}

/// Spool a git request body to disk with byte and per-frame **idle** ceilings.
///
/// This is the HTTP transport's slow-drip defense, and it lives *here* rather
/// than around the protocol handler on purpose. The network bytes arrive here,
/// frame by frame, before the staged file is handed to rg-git. A request may be
/// as large as a legitimate push without reserving the same amount of RAM.
///
/// If no body frame arrives within `idle_secs`, the buffer aborts with a
/// `504 Gateway Timeout` — the same status the wall-clock path returns, so a
/// stalled git upload classifies identically regardless of which watchdog fires.
/// `idle_secs == 0` disables the idle bound (plain disk staging).
///
/// Errors map to: idle stall → 504; over-limit (upstream `LengthLimitError` or
/// the explicit `max_bytes` backstop) → 413; any other body error → 400; local
/// staging failures → 500.
///
/// NOTE: this guards the *request* body (push upload / clone negotiation). The
/// slow-drip *download* twin — a client that reads a buffered clone one byte at
/// a time to pin its memory — is handled on the response side by
/// [`git_response_body_with_idle`], which streams the finished pack through a
/// bounded channel so socket backpressure trips the same idle window.
async fn stage_git_body(
    body: axum::body::Body,
    repo_root: &std::path::Path,
    max_bytes: usize,
    idle_secs: u64,
) -> Result<StagedGitBody, (StatusCode, String)> {
    use http_body_util::BodyExt;

    let staging_dir = rg_core::staging::StagingArea::GitRequests.path_in(repo_root);
    tokio::fs::create_dir_all(&staging_dir)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                rg_core::platform::fs::describe_path_error(
                    "git request staging directory",
                    &staging_dir,
                    &error,
                    rg_core::platform::fs::BLOB_STORAGE_HINT,
                ),
            )
        })?;
    let staged = tempfile::Builder::new()
        .prefix("git-")
        .suffix(".request")
        .tempfile_in(&staging_dir)
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                rg_core::platform::fs::describe_path_error(
                    "git request staging file",
                    &staging_dir,
                    &error,
                    rg_core::platform::fs::BLOB_STORAGE_HINT,
                ),
            )
        })?;
    let (file, path) = staged.into_parts();
    let mut file = tokio::fs::File::from_std(file);
    let idle = (idle_secs > 0).then(|| std::time::Duration::from_secs(idle_secs));
    let mut body = body;
    let mut written = 0_usize;

    loop {
        // Await the next frame, bounded by the idle window when enabled.
        let framed = match idle {
            Some(dur) => match tokio::time::timeout(dur, body.frame()).await {
                Ok(framed) => framed,
                Err(_elapsed) => {
                    return Err((
                        StatusCode::GATEWAY_TIMEOUT,
                        "git request body idle timeout: no data within the idle window".to_string(),
                    ));
                }
            },
            None => body.frame().await,
        };

        match framed {
            Some(Ok(frame)) => {
                if let Ok(data) = frame.into_data() {
                    written = written
                        .checked_add(data.len())
                        .filter(|size| *size <= max_bytes)
                        .ok_or_else(|| {
                            (
                                StatusCode::PAYLOAD_TOO_LARGE,
                                format!(
                                    "git request body exceeds the configured {max_bytes}-byte limit"
                                ),
                            )
                        })?;
                    file.write_all(&data).await.map_err(|error| {
                        (
                            StatusCode::INTERNAL_SERVER_ERROR,
                            rg_core::platform::fs::describe_path_error(
                                "git request staging file",
                                &path,
                                &error,
                                rg_core::platform::fs::BLOB_STORAGE_HINT,
                            ),
                        )
                    })?;
                }
                // A trailers-only frame carries no data — nothing to stage.
            }
            // Clean end of stream.
            None => break,
            // Body error: distinguish an upstream size-limit trip (413) from any
            // other transport error (400). `axum::Error` *wraps* the underlying
            // cause (e.g. `LengthLimitError` from `RequestBodyLimitLayer`), so we
            // unwrap to its inner boxed error and walk the whole `source()` chain
            // — a direct downcast on the outer `axum::Error` would never match.
            Some(Err(err)) => {
                let inner = err.into_inner();
                if crate::body_limit::is_length_limit_error(&*inner) {
                    return Err((
                        StatusCode::PAYLOAD_TOO_LARGE,
                        "git request body exceeds the maximum allowed size".to_string(),
                    ));
                }
                return Err((
                    StatusCode::BAD_REQUEST,
                    format!("failed to read git request body: {inner}"),
                ));
            }
        }
    }

    file.flush().await.map_err(|error| {
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            rg_core::platform::fs::describe_path_error(
                "git request staging file",
                &path,
                &error,
                rg_core::platform::fs::BLOB_STORAGE_HINT,
            ),
        )
    })?;
    file.seek(std::io::SeekFrom::Start(0))
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                rg_core::platform::fs::describe_path_error(
                    "git request staging file",
                    &path,
                    &error,
                    rg_core::platform::fs::BLOB_STORAGE_HINT,
                ),
            )
        })?;

    Ok(StagedGitBody { file, path })
}

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
    // Hot path: every git clone/fetch/push runs an access check. Metered into
    // the db-query series (see `metrics::time_db` for the sampling boundary).
    let access = if require_write {
        crate::metrics::time_db(
            "repo.can_write",
            rg_core::repo::service::can_write(db, owner, repo_name, actor_id),
        )
        .await
    } else {
        crate::metrics::time_db(
            "repo.can_read",
            rg_core::repo::service::can_read(db, owner, repo_name, actor_id),
        )
        .await
    };

    match access {
        Ok(true) => Ok(()),
        // Anonymous + denied → 401 with a Basic challenge so git prompts for
        // credentials. The String body carries the default text/plain type.
        Ok(false) if actor_id.is_none() => Err((
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Basic realm=\"Plombir Git\"")],
            "authentication required".to_string(),
        )),
        // Authenticated but lacking permission → 403.
        Ok(false) => Err((
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "text/plain")],
            "access denied".to_string(),
        )),
        // The repository genuinely is not there — the only failure of this
        // lookup that belongs to the caller. `can_read` / `can_write` mark it
        // with `rg_core::error::NotFound`, which survives any `.context(…)` on
        // the way up, so this arm cannot be reached by a lookup that merely
        // *failed*.
        Err(e) if e.downcast_ref::<rg_core::error::NotFound>().is_some() => Err((
            StatusCode::NOT_FOUND,
            [(header::CONTENT_TYPE, "text/plain")],
            "repository not found".to_string(),
        )),
        // Everything else is ours: the question could not be asked, so the
        // answer is not "no such repository". Saying 404 here tells git the
        // repo is gone — a clone stops, and CI / mirrors / cron pushes do not
        // retry a 404 — and it used to ship the `db: …` context of the failed
        // operation in a body nothing sanitizes (H-05). Status from
        // `git_db_status`, detail to the log only.
        Err(e) => Err((
            git_db_status(&e),
            [(header::CONTENT_TYPE, "text/plain")],
            git_failure_body("git access check", &e),
        )),
    }
}

/// The one sentence a git client is allowed to hear about a failure of ours,
/// with the whole `anyhow` chain going to the log instead.
///
/// The git transport has no JSON envelope for `AppError` to sanitize — every
/// return path writes its own body — so `{:#}` on an error puts the storage
/// path, the gix internals and the `db: <operation>` context straight in front
/// of whoever ran `git clone` (H-05). `operation` names the step for the log;
/// the client gets a fixed string chosen by the status.
/// Every ref a push must be refused, branch rules and tag rules together.
///
/// Both halves read an allow-list out of a stored JSON column and both are
/// fallible for the same reason, so they are joined here and the call site has
/// one error to answer rather than two identical arms.
/// Apply a Personal Access Token's narrowing to a git request.
///
/// Git is not behind the REST PAT middleware or the per-route layer, so the two
/// confinements a token can carry are applied here: a token confined to MCP
/// tools has no business on the git transport at all, and a token confined to
/// repositories reaches only those. An unknown repository and one outside the
/// allow-list answer alike, so the confinement is not an existence oracle.
async fn git_grant_refusal(
    state: &AppState,
    credential: Option<&GitCredential>,
    owner: &str,
    repo: &str,
    headers: &axum::http::HeaderMap,
) -> Result<(), (StatusCode, [(header::HeaderName, &'static str); 1], String)> {
    let Some(grant) = credential.and_then(|credential| credential.grant.as_ref()) else {
        return Ok(());
    };
    let refusal = |message: String| {
        (
            StatusCode::FORBIDDEN,
            [(header::CONTENT_TYPE, "text/plain")],
            message,
        )
    };
    if grant.is_mcp_only() {
        grant
            .record_denial(
                headers,
                serde_json::json!({
                    "reason": "mcp_only",
                    "transport": "git",
                    "repository": format!("{owner}/{repo}"),
                }),
            )
            .await;
        return Err(refusal(
            "this token may be used only through the MCP endpoint".to_string(),
        ));
    }
    if !grant.is_repo_restricted() {
        return Ok(());
    }
    let admitted = match find_repo_by_name(&state.db, owner, repo).await {
        Ok(found) => found.is_some_and(|found| grant.admits_repository(found.id)),
        Err(e) => {
            return Err((
                git_db_status(&e),
                [(header::CONTENT_TYPE, "text/plain")],
                git_failure_body("load repository", &e),
            ));
        }
    };
    if admitted {
        return Ok(());
    }
    grant
        .record_denial(
            headers,
            serde_json::json!({
                "reason": "repository_not_allowed",
                "transport": "git",
                "repository": format!("{owner}/{repo}"),
            }),
        )
        .await;
    Err(refusal(format!("this token may not access {owner}/{repo}")))
}

fn receive_pack_rejected_refs(
    protection_rules: Vec<rg_db::ops::protected_branch_ops::Rule>,
    tag_protection_rules: Vec<rg_db::ops::protected_tag_ops::Rule>,
    actor_id: Option<i64>,
) -> anyhow::Result<Vec<(String, String)>> {
    let mut rejected = branch_protection_rejected_refs(protection_rules, actor_id)?;
    rejected.extend(tag_protection_rejected_refs(
        tag_protection_rules,
        actor_id,
    )?);
    Ok(rejected)
}

fn git_failure_body(operation: &'static str, e: &anyhow::Error) -> String {
    let status = git_db_status(e);
    tracing::error!(
        error = %format!("{e:#}"),
        %operation,
        %status,
        "git transport request failed"
    );
    if status == StatusCode::SERVICE_UNAVAILABLE {
        "database temporarily unavailable".to_string()
    } else {
        "internal server error".to_string()
    }
}

/// Drain a finished Git protocol response without turning a failed reader into
/// a valid, empty protocol response.
fn spawn_git_response_reader<R>(reader: R) -> tokio::task::JoinHandle<std::io::Result<Vec<u8>>>
where
    R: tokio::io::AsyncRead + Send + Unpin + 'static,
{
    tokio::spawn(async move {
        let mut reader = reader;
        let mut output = Vec::new();
        reader.read_to_end(&mut output).await?;
        Ok(output)
    })
}

/// A join failure is a server failure too: the Git subprocess may have
/// completed, but we cannot honestly tell the client that its response was
/// delivered if the task that copied it failed or was cancelled.
///
/// Both failure modes collapse into one `anyhow::Error` here so every caller
/// classifies them identically — the `unwrap_or_default()` that used to sit at
/// these call sites is what turned a broken copy into a valid, empty protocol
/// response.
async fn collect_git_response_bytes(
    reader_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) -> Result<Vec<u8>> {
    reader_task
        .await
        .map_err(anyhow::Error::from)
        .and_then(|result| result.map_err(anyhow::Error::from))
}

/// Turn a drained receive-pack response into either its bytes or the sanitized
/// 5xx that says the drain failed.
///
/// `handle_git_receive_pack` returns the bare tuple shape (not `Response`) on
/// every branch, and its success branch also fires the post-push hooks — so it
/// needs the error as a value it can `return` *before* those side effects,
/// rather than an already-built `Response`.
///
/// Upload-pack has no twin of this any more: its response is streamed rather
/// than collected (`stream_upload_pack_response`), because a clone is sized by
/// the repository. A receive-pack response is a per-ref status report — bounded
/// by the push's own ref count — so draining it stays the simpler, honest
/// choice there.
async fn collect_receive_pack_response(
    reader_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
    operation: &'static str,
) -> std::result::Result<Vec<u8>, ReceivePackResponse> {
    collect_git_response_bytes(reader_task)
        .await
        .map_err(|error| {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain")],
                Body::from(git_failure_body(operation, &error)),
            )
        })
}

/// The bare tuple shape `handle_git_receive_pack` answers with on every branch.
type ReceivePackResponse = (StatusCode, [(header::HeaderName, &'static str); 1], Body);

/// A receive-pack run that came back without a `ReceivePackOutcome`.
///
/// The two arms answer the client differently (500 vs 504) but owe the push the
/// same thing: neither of them proves the refs stayed put. `TimedOut` in
/// particular *cannot* prove it — the budget bounds the whole handler, so it
/// elapses over a push that is already written just as readily as over one that
/// never started. Collapsing both into one `Err` is what puts the
/// applied-updates check on a single path (card_ca431156e7df).
enum ReceivePackFailure {
    Handler(anyhow::Error),
    TimedOut,
}

/// The failure twin of [`finish_landed_receive_pack`]: what a receive-pack run
/// that came back without an outcome still owes, and what the client hears.
///
/// "No outcome" is not "nothing happened". `with_git_timeout` bounds the whole
/// push handler, so an elapsed budget drops the future — including a future
/// whose `update_ref` calls have already run, since the report-status write and
/// the response drain are the last things in it. The client is told the truth
/// (504: it does not know what became of its push), but the branch has moved,
/// and its retry will carry no objects and be answered `Everything
/// up-to-date` — so this is the only chance the pipeline, the `push` webhook,
/// the watch fan-out and the open-PR head-SHA refresh will get. The sink was
/// created outside the timeout for exactly this reason (card_ca431156e7df).
fn split_failed_receive_pack(
    failure: ReceivePackFailure,
    applied: &rg_git::protocol::receive_pack::AppliedRefUpdates,
) -> (
    Option<Vec<rg_git::protocol::receive_pack::RefUpdate>>,
    ReceivePackResponse,
) {
    let landed = applied.take_landed();

    let response = match failure {
        ReceivePackFailure::Handler(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(git_failure_body("receive-pack", &error)),
        ),
        ReceivePackFailure::TimedOut => (
            StatusCode::GATEWAY_TIMEOUT,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from("git operation timed out"),
        ),
    };

    (landed, response)
}

/// Finish a push that has already landed: hand back the ref updates the
/// post-push hooks are owed, together with what the client hears.
///
/// Everything that can fail here fails *after* rg-git's point of no return —
/// the refs are in `refs/*`, and the pusher's retry carries no objects and gets
/// `Everything up-to-date`. So the client is still told the truth (it did not
/// get its report-status, so it hears a 5xx), but the CI pipeline, the `push`
/// webhook, the watch fan-out and the open-PR head-SHA refresh are owed either
/// way. Handing the updates back instead of `return`ing a response is what
/// keeps that true: no branch remains from which a landed push can leave
/// without its hooks (card_abd7384eed60 — the mirror image of
/// card_9c1d563ece91, which answered `200` for a response we never read).
async fn finish_landed_receive_pack(
    outcome: rg_git::protocol::receive_pack::ReceivePackOutcome,
    buf_writer: tokio::io::DuplexStream,
    reader_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) -> (
    Vec<rg_git::protocol::receive_pack::RefUpdate>,
    ReceivePackResponse,
) {
    let rg_git::protocol::receive_pack::ReceivePackOutcome {
        ref_updates,
        report_status,
    } = outcome;

    let response =
        match drain_landed_receive_pack_response(report_status, buf_writer, reader_task).await {
            Ok(output) => (
                StatusCode::OK,
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from(output),
            ),
            Err(response) => response,
        };

    (ref_updates, response)
}

/// The response half of [`finish_landed_receive_pack`]: the report-status
/// bytes the client is owed, or the 5xx naming the step that failed.
async fn drain_landed_receive_pack_response(
    report_status: Result<()>,
    mut buf_writer: tokio::io::DuplexStream,
    reader_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
) -> std::result::Result<Vec<u8>, ReceivePackResponse> {
    if let Err(error) = report_status {
        drop(buf_writer);
        reader_task.abort();
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(git_failure_body("send receive-pack response", &error)),
        ));
    }

    if let Err(error) = buf_writer.flush().await {
        drop(buf_writer);
        reader_task.abort();
        return Err((
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            Body::from(format!(
                "failed to flush git receive-pack response: {error}"
            )),
        ));
    }
    drop(buf_writer);

    // A copy that failed, panicked, or was cancelled cannot be reported as a
    // delivered push: the partial (or empty) buffer would go out as
    // `200 …-receive-pack-result`, telling the client its refs landed with
    // nothing to say which of them actually did (card_9c1d563ece91 — the
    // receive-pack twin of the upload-pack fix in card_2bfc8c1d8648).
    collect_receive_pack_response(reader_task, "read receive-pack response").await
}

/// Working window between the protocol handler and the response stream.
///
/// This is now the *whole* server-side cost of a clone's body: rg-git streams
/// `git pack-objects` a chunk at a time into this duplex, and the response
/// stream drains it a chunk at a time into the socket. Nothing on the path
/// holds a pack any more.
const UPLOAD_PACK_WINDOW_BYTES: usize = 64 * 1024;

/// Which upload-pack dialect a POST asked for.
///
/// The two differ only in the handler called; every resource bound, status
/// mapping and log line below is shared, so they cannot drift apart the way two
/// copies of the same match arm did.
#[derive(Clone, Copy)]
enum UploadPackProtocol {
    V1,
    V2,
}

impl UploadPackProtocol {
    /// The operation name used in the operator log and the sanitized 5xx body.
    fn operation(self) -> &'static str {
        match self {
            Self::V1 => "upload-pack",
            Self::V2 => "upload-pack (v2)",
        }
    }
}

/// How the protocol handler ended, as seen from outside its task.
enum UploadPackOutcome {
    Completed,
    Failed(anyhow::Error),
    TimedOut,
}

/// Serve a clone/fetch by streaming the protocol handler's output, never
/// collecting it.
///
/// **Why the first chunk is read here.** Streaming and honest status codes pull
/// in opposite directions: once a byte of body is on the wire the status is
/// spent, but a clone is exactly the response that must not be buffered to
/// learn its outcome. The split is the first read. Until it returns, nothing
/// has been sent and every failure is still answerable as `500` / `504` — which
/// covers the failures that matter, because a repository that will not open, a
/// malformed request or an immediate `pack-objects` spawn failure all happen
/// before the handler writes anything. After it returns, the failures left are
/// mid-pack ones, and those are reported the way the git protocol reports them:
/// a band-3 error from rg-git plus a broken body here, never a tidy short
/// response that reads as a complete clone (`card_2bfc8c1d8648`).
async fn stream_upload_pack_response(
    protocol: UploadPackProtocol,
    repo_path: std::path::PathBuf,
    staged: StagedGitBody,
    stream_timeout_secs: u64,
    idle_timeout_secs: u64,
    owner: &str,
    repo: &str,
) -> Response {
    let (mut buf_reader, mut buf_writer) = tokio::io::duplex(UPLOAD_PACK_WINDOW_BYTES);
    let operation = protocol.operation();

    let handler = tokio::spawn(async move {
        // The spool file travels with its handler: dropping the `TempPath` here
        // rather than in the request scope unlinks it as soon as the pack that
        // reads it is done.
        let StagedGitBody {
            file,
            path: _staged_path,
        } = staged;

        let outcome = with_git_timeout(stream_timeout_secs, async {
            match protocol {
                UploadPackProtocol::V1 => {
                    rg_git::protocol::upload_pack::handle_upload_pack_http(
                        &repo_path,
                        file,
                        &mut buf_writer,
                    )
                    .await
                }
                UploadPackProtocol::V2 => {
                    rg_git::protocol::v2::handle_v2_http(&repo_path, file, &mut buf_writer).await
                }
            }
        })
        .await;

        // Flush, then close the write half: end-of-stream on the reader is what
        // tells the response body the answer is complete.
        let flushed = buf_writer.flush().await;
        drop(buf_writer);

        match outcome {
            Ok(Ok(())) => match flushed {
                Ok(()) => UploadPackOutcome::Completed,
                Err(error) => UploadPackOutcome::Failed(
                    anyhow::Error::from(error).context("failed to flush git upload-pack response"),
                ),
            },
            Ok(Err(error)) => UploadPackOutcome::Failed(error),
            Err(_elapsed) => UploadPackOutcome::TimedOut,
        }
    });

    let mut head = vec![0u8; UPLOAD_PACK_WINDOW_BYTES];
    let first = match buf_reader.read(&mut head).await {
        Ok(read) => read,
        Err(error) => {
            handler.abort();
            let error = anyhow::Error::from(error).context("read git upload-pack response");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain")],
                Body::from(git_failure_body(operation, &error)),
            )
                .into_response();
        }
    };

    if first == 0 {
        // End of stream with nothing written: the handler is already finished
        // (that is what closed the write half), so its verdict is the whole
        // answer and no bytes have committed us to a status yet.
        return match handler.await {
            Ok(UploadPackOutcome::Completed) => (
                StatusCode::OK,
                [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
                Body::empty(),
            )
                .into_response(),
            Ok(UploadPackOutcome::Failed(error)) => (
                StatusCode::INTERNAL_SERVER_ERROR,
                [(header::CONTENT_TYPE, "text/plain")],
                Body::from(git_failure_body(operation, &error)),
            )
                .into_response(),
            Ok(UploadPackOutcome::TimedOut) => {
                tracing::warn!(
                    %owner, %repo, timeout_secs = stream_timeout_secs,
                    "git {operation} exceeded wall-clock timeout — killed git, returning 504"
                );
                (
                    StatusCode::GATEWAY_TIMEOUT,
                    [(header::CONTENT_TYPE, "text/plain")],
                    Body::from("git operation timed out"),
                )
                    .into_response()
            }
            // A panicked or cancelled handler task is a server failure like any
            // other: it must not become a valid, empty protocol response.
            Err(join_error) => {
                let error = anyhow::Error::from(join_error).context("git upload-pack handler task");
                (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(header::CONTENT_TYPE, "text/plain")],
                    Body::from(git_failure_body(operation, &error)),
                )
                    .into_response()
            }
        };
    }

    head.truncate(first);
    let owner = owner.to_string();
    let repo = repo.to_string();
    let completion = async move {
        match handler.await {
            Ok(UploadPackOutcome::Completed) => Ok(()),
            Ok(UploadPackOutcome::Failed(error)) => {
                tracing::error!(
                    error = %format!("{error:#}"),
                    %operation, %owner, %repo,
                    "git upload-pack failed after the response had begun — breaking the body so \
                     the client cannot read a partial pack as a complete clone"
                );
                Err(std::io::Error::other("git upload-pack failed mid-response"))
            }
            Ok(UploadPackOutcome::TimedOut) => {
                tracing::warn!(
                    %owner, %repo, timeout_secs = stream_timeout_secs,
                    "git {operation} exceeded wall-clock timeout mid-response — killed git, \
                     breaking the body"
                );
                Err(std::io::Error::other(
                    "git operation timed out mid-response",
                ))
            }
            Err(join_error) => {
                tracing::error!(
                    error = %join_error,
                    %operation, %owner, %repo,
                    "git upload-pack handler task did not finish — breaking the body"
                );
                Err(std::io::Error::other(
                    "git upload-pack handler task did not finish",
                ))
            }
        }
    };

    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
        crate::http_stream::reader_body_with_idle(
            axum::body::Bytes::from(head),
            buf_reader,
            completion,
            idle_timeout_secs,
        ),
    )
        .into_response()
}

/// Classify a DB-touching error into an HTTP status while preserving the git
/// smart-HTTP response envelope.
///
/// The git client reads the status line and body in the protocol's own framing
/// (`application/x-git-*-result` / `text/plain`), so — unlike the JSON API — we
/// can't route these errors through `AppError` without breaking the
/// content-type git expects. Instead we classify only the *status*: a
/// `sea_orm::DbErr` the caller should come back for — the database was
/// unreachable, or the transaction lost to a concurrent writer — seen through
/// any `anyhow` `.context()` layers, is a transient failure → 503; everything
/// else stays 500. The predicate is shared with the JSON API
/// (`From<DbErr> for AppError`) and the OCI registry (`oci::oci_status_for`)
/// via `AppError::is_db_retryable`, so a database that is down or contended
/// classifies identically on every transport.
fn git_db_status(e: &anyhow::Error) -> StatusCode {
    match e.downcast_ref::<sea_orm::DbErr>() {
        Some(db_err) if crate::error::AppError::is_db_retryable(db_err) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
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

    // Extract actor from auth header
    let credential = match extract_git_credential(&state.db, &headers, &state.jwt_secret).await {
        Ok(credential) => credential,
        Err(e) => {
            return (
                git_db_status(&e),
                [(header::CONTENT_TYPE, "text/plain")],
                git_failure_body("resolve git credential", &e),
            );
        }
    };
    let require_write = service == "git-receive-pack";
    let actor_id = credential.as_ref().map(|credential| credential.user_id);
    if let Err(resp) = git_grant_refusal(&state, credential.as_ref(), &owner, &repo, &headers).await
    {
        return resp;
    }

    // Check access
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, require_write).await {
        return resp;
    }

    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "text/plain")],
            git_failure_body("open repository", &e),
        );
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
                        git_failure_body("v2 capability advertisement", &e),
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
                    // `build_info_refs` opens the bare repository, and its
                    // context carries the server-side path.
                    git_failure_body("ref advertisement", &e),
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

    let advertisement = rg_git::ref_advertisement::collect_for_clients(repo_path)
        .context("failed to collect repository refs")?;
    let mut ref_list = advertisement.refs;

    if let Some(head_oid) = advertisement.head_oid {
        ref_list.insert(0, (head_oid, "HEAD".to_string()));
    }

    let caps = if service == "git-upload-pack" {
        "multi_ack_detailed no-done side-band-64k thin-pack ofs-delta agent=plombir-git/0.1"
    } else {
        "report-status report-status-v2 side-band-64k agent=plombir-git/0.1"
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
    body: axum::body::Body,
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

    // Check read access
    let credential = match extract_git_credential(&state.db, &headers, &state.jwt_secret).await {
        Ok(credential) => credential,
        Err(e) => {
            return (
                git_db_status(&e),
                [(header::CONTENT_TYPE, "text/plain")],
                Body::from(git_failure_body("resolve git credential", &e)),
            )
                .into_response();
        }
    };
    let actor_id = credential.as_ref().map(|credential| credential.user_id);
    if let Err(resp) = git_grant_refusal(&state, credential.as_ref(), &owner, &repo, &headers).await
    {
        return (resp.0, resp.1, Body::from(resp.2)).into_response();
    }
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, false).await {
        return (resp.0, resp.1, Body::from(resp.2)).into_response();
    }

    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(header::CONTENT_TYPE, "application/x-git-upload-pack-result")],
            Body::from(git_failure_body("open repository", &e)),
        )
            .into_response();
    }

    // Record fetch/clone/pull duration + count across every return path below.
    let _op_timer = GitOpTimer::new("fetch");

    // Authenticate and resolve repository storage before reading attacker-owned
    // bytes. The request is then spooled under the negotiation ceiling, keeping
    // both unauthenticated work and per-request RAM bounded.
    let staged = match stage_git_body(
        body,
        &state.repo_root,
        rg_git::protocol::MAX_NEGOTIATION_INPUT_BYTES,
        state.git_idle_timeout_secs,
    )
    .await
    {
        Ok(staged) => staged,
        Err((status, msg)) => {
            return (
                status,
                [(header::CONTENT_TYPE, "text/plain")],
                Body::from(msg),
            )
                .into_response();
        }
    };
    // Check if client wants Protocol V2
    let protocol = if git_v2::wants_protocol_v2(&headers) {
        UploadPackProtocol::V2
    } else {
        UploadPackProtocol::V1
    };

    stream_upload_pack_response(
        protocol,
        repo_path,
        staged,
        state.git_stream_timeout_secs,
        state.git_idle_timeout_secs,
        &owner,
        &repo,
    )
    .await
}

pub(crate) async fn handle_git_receive_pack(
    State(state): State<AppState>,
    headers: axum::http::HeaderMap,
    axum::extract::Path((owner, repo)): axum::extract::Path<(String, String)>,
    body: axum::body::Body,
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

    // Check write access
    let credential = match extract_git_credential(&state.db, &headers, &state.jwt_secret).await {
        Ok(credential) => credential,
        Err(e) => {
            return (
                git_db_status(&e),
                [(header::CONTENT_TYPE, "text/plain")],
                Body::from(git_failure_body("resolve git credential", &e)),
            );
        }
    };
    let actor_id = credential.as_ref().map(|credential| credential.user_id);
    if let Err(resp) = git_grant_refusal(&state, credential.as_ref(), &owner, &repo, &headers).await
    {
        return (resp.0, resp.1, Body::from(resp.2));
    }
    if let Err(resp) = check_git_access(&state.db, &owner, &repo, actor_id, true).await {
        return (resp.0, resp.1, Body::from(resp.2));
    }

    if let Err(e) = crate::error::ensure_repository_storage(&repo_path) {
        return (
            StatusCode::INTERNAL_SERVER_ERROR,
            [(
                header::CONTENT_TYPE,
                "application/x-git-receive-pack-result",
            )],
            Body::from(git_failure_body("open repository", &e)),
        );
    }

    // Record push duration + count across every return path below.
    let _op_timer = GitOpTimer::new("push");

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
                git_db_status(&e),
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from(git_failure_body("load repository", &e)),
            );
        }
    };
    let protection_rules = match rg_db::ops::protected_branch_ops::list_rules_by_repo(
        &state.db,
        repo_model.id,
    )
    .await
    {
        Ok(rules) => rules,
        Err(e) => {
            return (
                git_db_status(&e),
                [(
                    header::CONTENT_TYPE,
                    "application/x-git-receive-pack-result",
                )],
                Body::from(git_failure_body("load branch protections", &e)),
            );
        }
    };
    let tag_protection_rules =
        match rg_db::ops::protected_tag_ops::list_rules_by_repo(&state.db, repo_model.id).await {
            Ok(rules) => rules,
            Err(e) => {
                return (
                    git_db_status(&e),
                    [(
                        header::CONTENT_TYPE,
                        "application/x-git-receive-pack-result",
                    )],
                    Body::from(git_failure_body("load tag protections", &e)),
                );
            }
        };

    // Decided before anything is spawned: a rule whose stored allow-list does
    // not decode is a fault of ours, and the client has to hear that rather
    // than "push to protected branch … is not allowed" — which would blame the
    // pusher for a broken row and, for a pusher who *is* on the list, be a lie.
    let require_signed_refs = signed_commit_required_refs(&protection_rules);
    // A token kept off protected branches is refused every one of them, first,
    // whatever the rules would let its account do (card_60a80311d512).
    let token_rejections: Vec<(String, String)> = match credential
        .as_ref()
        .and_then(|credential| credential.grant.as_ref())
        .filter(|grant| grant.denies_protected_writes())
    {
        Some(_) => protection_rules
            .iter()
            .map(|rule| {
                (
                    format!("refs/heads/{}", rule.protection.branch_name),
                    format!(
                        "this token may not write to protected branch '{}'",
                        rule.protection.branch_name
                    ),
                )
            })
            .collect(),
        None => Vec::new(),
    };
    let rejected_refs =
        match receive_pack_rejected_refs(protection_rules, tag_protection_rules, actor_id) {
            Ok(refs) => token_rejections.into_iter().chain(refs).collect(),
            Err(e) => {
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    [(
                        header::CONTENT_TYPE,
                        "application/x-git-receive-pack-result",
                    )],
                    Body::from(git_failure_body("evaluate push protection rules", &e)),
                );
            }
        };

    // All access and policy reads happen before the body is consumed. A valid
    // pusher's request is spooled to disk under the same ceiling rg-git applies
    // again at pack ingestion, so neither HTTP nor SSH has an unbounded path.
    let staged = match stage_git_body(
        body,
        &state.repo_root,
        rg_git::protocol::receive_pack::MAX_PACK_INPUT_BYTES,
        state.git_idle_timeout_secs,
    )
    .await
    {
        Ok(staged) => staged,
        Err((status, msg)) => {
            return (
                status,
                [(header::CONTENT_TYPE, "text/plain")],
                Body::from(msg),
            );
        }
    };
    let StagedGitBody {
        file,
        path: _staged_path,
    } = staged;

    let (buf_reader, mut buf_writer) = tokio::io::duplex(64 * 1024);
    // Spawn concurrent reader to prevent duplex deadlock when response > 64KB
    let reader_task = spawn_git_response_reader(buf_reader);

    // Created out here on purpose: `with_git_timeout` bounds the *whole* push
    // handler, and an elapsed budget drops that future rather than letting it
    // report anything — so a sink living inside it would be dropped along with
    // the ref updates it holds. See [`AppliedRefUpdates`].
    let applied = rg_git::protocol::receive_pack::AppliedRefUpdates::new();

    let session = match with_git_timeout(
        state.git_stream_timeout_secs,
        rg_git::protocol::receive_pack::handle_receive_pack_http_with_rejections(
            &repo_path,
            file,
            &mut buf_writer,
            rejected_refs,
            require_signed_refs,
            &applied,
        ),
    )
    .await
    {
        Ok(Ok(outcome)) => Ok(outcome),
        Ok(Err(error)) => Err(ReceivePackFailure::Handler(error)),
        Err(_elapsed) => Err(ReceivePackFailure::TimedOut),
    };

    match session {
        // The push has landed — the pack is indexed and the refs are written.
        // What is left is the client's report-status, and whether that reaches
        // it decides the status code, *not* whether the hooks run.
        Ok(outcome) => {
            let (ref_updates, response) =
                finish_landed_receive_pack(outcome, buf_writer, reader_task).await;

            // ── Post-push hooks: trigger CI + Webhook ───────────────
            //
            // Shared with every other path that moves a ref (SSH's
            // `receive-pack`, the web editor's `POST /contents/*`) through
            // `AppState::spawn_post_push_hooks`, which owns the detach: the
            // work is tracked by `delivery_tracker()`, not a bare
            // `tokio::spawn`, so the shutdown drain in `rg_http::run` awaits it
            // instead of a SIGTERM severing the pipeline / webhook / PR
            // head-SHA refresh the client was already told it got.
            //
            // Unconditional on the response above by design: a push whose
            // report-status died on the wire still moved the branch, and a
            // retry would only be told `Everything up-to-date`.
            state.spawn_post_push_hooks(
                repo_path.clone(),
                owner.clone(),
                repo.clone(),
                actor_id,
                ref_updates,
            );

            response
        }
        // No outcome came back — but "no outcome" does not mean "nothing
        // happened". A wall-clock budget that elapses after `update_ref` drops
        // the handler mid-report-status, leaving a branch that has moved and a
        // client that will be told nothing landed; its retry carries no objects
        // and is answered `Everything up-to-date`, so this is the only chance
        // the pipeline, the `push` webhook, the watch fan-out and the open-PR
        // head-SHA refresh get (card_ca431156e7df). The sink outlived the drop
        // and knows what actually landed.
        Err(failure) => {
            // Neither arm will read the drained response, so the duplex goes and
            // the collector is cancelled for both — the timeout arm always did
            // this, and the handler-error arm was only relying on the writer's
            // scope end to reach the same place.
            drop(buf_writer);
            reader_task.abort();
            if matches!(failure, ReceivePackFailure::TimedOut) {
                tracing::warn!(
                    %owner, %repo, timeout_secs = state.git_stream_timeout_secs,
                    "git receive-pack exceeded wall-clock timeout — killed git, returning 504"
                );
            }

            let (landed, response) = split_failed_receive_pack(failure, &applied);
            if let Some(ref_updates) = landed {
                tracing::warn!(
                    %owner, %repo, refs = ref_updates.len(),
                    "git receive-pack came back empty-handed after its refs landed — running the post-push hooks anyway"
                );
                state.spawn_post_push_hooks(
                    repo_path.clone(),
                    owner.clone(),
                    repo.clone(),
                    actor_id,
                    ref_updates,
                );
            }

            response
        }
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

#[cfg(test)]
mod tests {
    use super::{
        build_info_refs, collect_receive_pack_response, finish_landed_receive_pack,
        git_failure_body, spawn_git_response_reader, split_failed_receive_pack, stage_git_body,
        with_git_timeout, ReceivePackFailure,
    };
    use axum::body::{Body, Bytes};
    use axum::http::StatusCode;
    use http_body_util::BodyExt;
    use rg_git::protocol::receive_pack::{AppliedRefUpdates, ReceivePackOutcome, RefUpdate};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[allow(dead_code)]
    mod rust_source {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../tests/support/rust_source.rs"
        ));
    }
    // NOTE: a second `use axum::http::StatusCode` further down in this module
    // (pre-existing) was removed in favor of this single top-level import.

    #[test]
    fn legacy_info_refs_keeps_the_protocol_null_head_for_an_unborn_repository() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("unborn.git");
        gix::init_bare(&repo_path).unwrap();

        let advertisement = build_info_refs(&repo_path, "git-upload-pack").unwrap();

        assert!(advertisement.contains("0000000000000000000000000000000000000000 HEAD\0"));
    }

    #[test]
    fn legacy_info_refs_rejects_an_unreadable_ref_instead_of_omitting_it() {
        let dir = tempfile::tempdir().unwrap();
        let repo_path = dir.path().join("broken-ref.git");
        gix::init_bare(&repo_path).unwrap();
        std::fs::create_dir_all(repo_path.join("refs/heads")).unwrap();
        std::fs::write(repo_path.join("refs/heads/broken"), "not-an-object-id\n").unwrap();

        let error = build_info_refs(&repo_path, "git-upload-pack").unwrap_err();

        assert!(
            format!("{error:#}").contains("failed to read a reference"),
            "{error:#}"
        );
    }

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("log lock").clone()).expect("logs are UTF-8")
        }
    }

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log lock").extend_from_slice(buf);
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

    /// A slow-drip request body (a chunk, then a long stall) trips the idle
    /// buffer at ~the idle window and returns 504 — not after the whole stall.
    #[tokio::test(start_paused = true)]
    async fn stage_git_body_trips_on_idle_drip() {
        // Chunk 0 arrives immediately; chunk 1 is 10s away (>> 1s idle).
        let body = Body::from_stream(futures::stream::unfold(0u8, |i| async move {
            match i {
                0 => Some((Ok::<_, std::io::Error>(Bytes::from_static(b"AAAA")), 1)),
                1 => {
                    tokio::time::sleep(Duration::from_secs(10)).await;
                    Some((Ok(Bytes::from_static(b"BBBB")), 2))
                }
                _ => None,
            }
        }));

        let root = tempfile::tempdir().unwrap();
        let err = stage_git_body(body, root.path(), 1024, 1)
            .await
            .unwrap_err();
        assert_eq!(err.0, StatusCode::GATEWAY_TIMEOUT, "idle drip → 504");
    }

    /// Continuous-but-slow chunks (each gap under the idle window) buffer fully:
    /// a legit slow-link push must not false-trip.
    #[tokio::test(start_paused = true)]
    async fn stage_git_body_allows_continuous_slow_traffic() {
        let body = Body::from_stream(futures::stream::unfold(0u8, |i| async move {
            if i >= 5 {
                return None;
            }
            // 300ms between chunks, under the 1s idle window every time.
            tokio::time::sleep(Duration::from_millis(300)).await;
            Some((Ok::<_, std::io::Error>(Bytes::from_static(b"pack")), i + 1))
        }));

        let root = tempfile::tempdir().unwrap();
        let mut staged = stage_git_body(body, root.path(), 1024, 1)
            .await
            .expect("continuous traffic must not trip");
        let mut bytes = Vec::new();
        staged.file.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"packpackpackpackpack");
    }

    /// `idle_secs == 0` disables the idle bound: even a long stall is tolerated.
    #[tokio::test(start_paused = true)]
    async fn stage_git_body_disabled_never_trips() {
        let body = Body::from_stream(futures::stream::unfold(0u8, |i| async move {
            match i {
                0 => {
                    tokio::time::sleep(Duration::from_secs(3600)).await;
                    Some((Ok::<_, std::io::Error>(Bytes::from_static(b"late")), 1))
                }
                _ => None,
            }
        }));

        let root = tempfile::tempdir().unwrap();
        let mut staged = stage_git_body(body, root.path(), 1024, 0)
            .await
            .expect("disabled window must not trip");
        let mut bytes = Vec::new();
        staged.file.read_to_end(&mut bytes).await.unwrap();
        assert_eq!(bytes, b"late");
    }

    #[tokio::test]
    async fn stage_git_body_rejects_overflow_and_removes_the_spool() {
        let root = tempfile::tempdir().unwrap();
        let staging_dir = root.path().join(".tmp/git-requests");
        let body = Body::from_stream(futures::stream::iter([
            Ok::<_, std::io::Error>(Bytes::from_static(b"123")),
            Ok::<_, std::io::Error>(Bytes::from_static(b"45")),
        ]));

        let error = stage_git_body(body, root.path(), 4, 0)
            .await
            .expect_err("the fifth byte must be refused");

        assert_eq!(error.0, StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(std::fs::read_dir(staging_dir).unwrap().count(), 0);
    }

    #[test]
    fn git_http_authenticates_before_staging_request_bytes() {
        let source = include_str!("git_http.rs");

        for function in ["handle_git_upload_pack", "handle_git_receive_pack"] {
            let calls = rust_source::production_function_call_sites(
                source,
                function,
                &["check_git_access", "stage_git_body"],
            );
            let auth = calls
                .iter()
                .find(|call| {
                    rust_source::source_line(source, call.line).contains("check_git_access")
                })
                .unwrap_or_else(|| panic!("{function} no longer calls check_git_access"));
            let staging = calls
                .iter()
                .find(|call| rust_source::source_line(source, call.line).contains("stage_git_body"))
                .unwrap_or_else(|| panic!("{function} no longer stages its request body"));

            assert!(
                auth.open_paren < staging.open_paren,
                "{function} consumes attacker-owned request bytes before repository access is \
                 decided"
            );
        }
    }

    #[tokio::test]
    async fn with_git_timeout_elapses_on_slow_future() {
        // A streaming handler slower than the wall-clock bound must elapse so
        // the caller can kill git + return 504.
        let res = with_git_timeout(1, async {
            tokio::time::sleep(Duration::from_secs(30)).await;
            42
        })
        .await;
        assert!(res.is_err(), "slow future should elapse");
    }

    #[tokio::test]
    async fn with_git_timeout_passes_fast_future() {
        let res = with_git_timeout(30, async { 7 }).await;
        assert_eq!(res.ok(), Some(7), "fast future should complete, not elapse");
    }

    #[tokio::test]
    async fn with_git_timeout_zero_disables_bound() {
        // 0 = opt out: the future runs unbounded and its value passes through.
        let res = with_git_timeout(0, async { 5 }).await;
        assert_eq!(res.ok(), Some(5));
    }

    /// The post-push hooks must stay on the *tracked* spawn path.
    ///
    /// The live-push drain test (`push_hook_drain_tests`) asserts the effect,
    /// but it cannot fail reliably: an untracked task usually still finishes
    /// before the assertion reads the row, so the regression it guards is
    /// timing-dependent by nature. This one is not — the drain in
    /// `rg_http::run` can only await what the tracker owns, so a bare
    /// `tokio::spawn` here is the bug regardless of how the race lands.
    #[test]
    fn post_push_hooks_are_detached_through_the_delivery_tracker() {
        fn one_call(
            source: &str,
            function: &str,
            name: &str,
        ) -> Result<rust_source::CallSite, String> {
            let calls = rust_source::production_function_call_sites(source, function, &[name]);
            match calls.as_slice() {
                [call] => Ok(*call),
                _ => Err(format!(
                    "expected one `{name}` call in production `{function}`, found {}",
                    calls.len()
                )),
            }
        }

        // Two paths through the push handler owe the hooks, and the guard
        // holds both: the run that came back with its outcome (whatever became
        // of the report-status), and the run that came back with nothing at all
        // because the wall-clock budget dropped the handler after `update_ref`
        // (card_ca431156e7df). A handoff that goes missing from either one
        // silently costs a landed push its pipeline, webhook and PR refresh.
        fn receive_pack_handoffs(source: &str) -> Result<Vec<rust_source::CallSite>, String> {
            let calls = rust_source::production_function_call_sites(
                source,
                "handle_git_receive_pack",
                &["state.spawn_post_push_hooks"],
            );
            if calls.len() != 2 {
                return Err(format!(
                    "expected both `state.spawn_post_push_hooks` handoffs in production \
                     `handle_git_receive_pack` (landed-with-outcome and landed-without-one), found {}",
                    calls.len()
                ));
            }
            Ok(calls)
        }

        fn contract(git_http_source: &str, app_source: &str) -> Result<(), String> {
            receive_pack_handoffs(git_http_source)?;

            let spawn = one_call(
                app_source,
                "spawn_post_push_hooks",
                "self.delivery_tracker.spawn",
            )?;
            let run = one_call(app_source, "spawn_post_push_hooks", "run")?;
            if !rust_source::call_site_contains(app_source, spawn, run) {
                return Err(format!(
                    "post-push run at lib.rs:{} is outside the tracked spawn at lib.rs:{}",
                    run.line, spawn.line
                ));
            }

            let shared_tracker_calls = rust_source::production_function_call_sites(
                app_source,
                "run_with_listener",
                &["rg_core::task_tracker::delivery_tracker"],
            );
            let app_state_initializers: Vec<_> = shared_tracker_calls
                .into_iter()
                .filter(|call| {
                    rust_source::source_line(app_source, call.line).contains("delivery_tracker:")
                })
                .collect();
            let [shared_tracker] = app_state_initializers.as_slice() else {
                return Err(format!(
                    "expected one shared tracker initializer for AppState, found {}",
                    app_state_initializers.len()
                ));
            };
            let tracker_line = rust_source::source_line(app_source, shared_tracker.line);
            if !tracker_line.contains("delivery_tracker:") {
                return Err(format!(
                    "shared tracker call at lib.rs:{} does not initialize AppState::delivery_tracker: `{}`",
                    shared_tracker.line,
                    tracker_line.trim()
                ));
            }
            Ok(())
        }

        fn without_call(source: &str, function: &str, name: &str) -> String {
            let call = one_call(source, function, name).expect("mutation target must exist");
            without_call_site(source, call, name)
        }

        /// Every one-handoff-short mutation of the handler, paired with the
        /// line it silenced. A free function so the guard reads the file
        /// through `receive_pack_handoffs` and never hands raw `.rs` bytes
        /// around itself — the same shape `without_call` already has.
        fn without_each_handoff(source: &str) -> Vec<(usize, String)> {
            receive_pack_handoffs(source)
                .expect("mutation targets must exist")
                .into_iter()
                .map(|handoff| {
                    (
                        handoff.line,
                        without_call_site(source, handoff, "state.spawn_post_push_hooks"),
                    )
                })
                .collect()
        }

        fn without_call_site(source: &str, call: rust_source::CallSite, name: &str) -> String {
            let name_at = source[..call.open_paren]
                .rfind(name)
                .expect("call name must precede its opening parenthesis");
            let mut mutated = source.to_owned();
            mutated.replace_range(name_at..name_at + name.len(), &" ".repeat(name.len()));
            mutated
        }

        let git_http_source = include_str!("git_http.rs");
        let app_source = include_str!("lib.rs");
        contract(git_http_source, app_source).unwrap_or_else(|error| panic!("{error}"));

        for (line, without_handoff) in without_each_handoff(git_http_source) {
            assert!(
                contract(&without_handoff, app_source).is_err(),
                "removing the receive-pack handoff at git_http.rs:{line} must fail this guard"
            );
        }

        for (function, name) in [
            ("spawn_post_push_hooks", "self.delivery_tracker.spawn"),
            ("spawn_post_push_hooks", "run"),
        ] {
            let mutated = without_call(app_source, function, name);
            assert!(
                contract(git_http_source, &mutated).is_err(),
                "removing `{name}` from production `{function}` must fail this guard"
            );
        }

        let tracker_name = "rg_core::task_tracker::delivery_tracker";
        let tracker_call = rust_source::production_function_call_sites(
            app_source,
            "run_with_listener",
            &[tracker_name],
        )
        .into_iter()
        .find(|call| rust_source::source_line(app_source, call.line).contains("delivery_tracker:"))
        .expect("AppState shared tracker initializer must exist");
        let without_shared_tracker = without_call_site(app_source, tracker_call, tracker_name);
        assert!(
            contract(git_http_source, &without_shared_tracker).is_err(),
            "removing the AppState shared tracker initializer must fail this guard"
        );
    }

    /// Read a process's state char from `/proc/<pid>/stat`, or `None` if it no
    /// longer exists. `'Z'` = zombie (killed, awaiting reap). `comm` may contain
    /// spaces/parens, so split on the last `)`.
    #[cfg(target_os = "linux")]
    fn proc_state(pid: u32) -> Option<char> {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        let after = stat.rsplit_once(')')?.1;
        after
            .split_whitespace()
            .next()
            .and_then(|s| s.chars().next())
    }

    /// The core anti-zombie guarantee: the subprocess lives *inside* the future,
    /// so when `with_git_timeout` drops that future on elapse, `kill_on_drop`
    /// reaps it — it must not keep running. Emulated here with a long `sleep`
    /// child (the git handlers spawn their child the same `kill_on_drop` way).
    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn with_git_timeout_kills_child_on_elapse() {
        use tokio::process::Command;

        let holder = std::sync::Arc::new(std::sync::Mutex::new(None));
        let captured = holder.clone();
        let res = with_git_timeout(1, async move {
            let mut child = Command::new("sleep")
                .arg("60")
                .kill_on_drop(true)
                .spawn()
                .expect("spawn sleep child");
            *captured.lock().unwrap() = child.id();
            drop(child.wait().await); // hang until the future is dropped
        })
        .await;
        assert!(res.is_err(), "the hanging future should elapse");

        let pid = holder.lock().unwrap().expect("child pid captured");
        // Poll for the kill + reap to land. The loop exits the moment it does,
        // so a generous ceiling costs nothing when the code is correct and only
        // buys tolerance for a loaded machine — the same wall-clock-as-assertion
        // trap as card_2b890485c8d8. ~30s.
        let mut killed = false;
        for _ in 0..300 {
            match proc_state(pid) {
                None | Some('Z') => {
                    killed = true;
                    break;
                }
                _ => tokio::time::sleep(Duration::from_millis(100)).await,
            }
        }
        assert!(
            killed,
            "sleep child (pid {pid}) must be killed on timeout, not left running"
        );
    }

    // --- card_8f1b9713fd2e: git smart-HTTP DB-outage classification ---------
    //
    // The git handlers can't route DB errors through `AppError` (that would
    // swap in JSON and break the `application/x-git-*-result` content-type the
    // client parses), so `git_db_status` classifies only the *status*. These
    // assert a connection-level outage → 503 while genuine bugs / non-DB errors
    // stay 500 — the same split `error.rs` guarantees for the JSON API, proven
    // here directly on the git predicate.
    use super::git_db_status;
    use sea_orm::{ConnAcquireErr, DbErr, RuntimeErr};

    #[test]
    fn git_failure_logs_the_full_cause_and_returns_only_the_safe_body() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::ERROR)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);
        let error =
            anyhow::Error::from(DbErr::Conn(RuntimeErr::Internal("connection reset".into())))
                .context("lookup personal access token");

        let body = git_failure_body("resolve git credential", &error);

        assert_eq!(body, "database temporarily unavailable");
        let rendered = logs.text();
        assert!(rendered.contains("resolve git credential"), "{rendered}");
        assert!(
            rendered.contains("lookup personal access token"),
            "{rendered}"
        );
        assert!(rendered.contains("connection reset"), "{rendered}");
    }

    #[test]
    fn git_db_status_connection_outage_is_503() {
        let e = anyhow::Error::from(DbErr::Conn(RuntimeErr::Internal("connection reset".into())));
        assert_eq!(git_db_status(&e), StatusCode::SERVICE_UNAVAILABLE);
        let e = anyhow::Error::from(DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed));
        assert_eq!(git_db_status(&e), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn git_db_status_sees_outage_through_context() {
        // `rg_db::ops::protected_branch_ops::list_by_repo` (and the repo lookup)
        // return `anyhow::Result`, wrapping the `DbErr` via `.context()`. The
        // downcast must see through that layer, or the push path would 500.
        let e = anyhow::Error::from(DbErr::ConnectionAcquire(ConnAcquireErr::ConnectionClosed))
            .context("db: list protected branches by repo");
        assert_eq!(git_db_status(&e), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[test]
    fn git_db_status_statement_error_stays_500() {
        // A statement-level DbErr with no backend contention code under it is a
        // bug: the variant alone does not decide the status, the backend's own
        // error code does — see `AppError::is_db_retryable`.
        let e = anyhow::Error::from(DbErr::Exec(RuntimeErr::Internal(
            "UNIQUE constraint failed".into(),
        )));
        assert_eq!(git_db_status(&e), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn git_db_status_non_db_error_stays_500() {
        // A plain error with no DbErr underneath (e.g. a gix / IO failure while
        // building the pack) must not be misclassified as a DB outage.
        let e = anyhow::anyhow!("failed to open repository");
        assert_eq!(git_db_status(&e), StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[derive(Default)]
    struct PartialThenFailReader {
        sent_partial: bool,
    }

    impl tokio::io::AsyncRead for PartialThenFailReader {
        fn poll_read(
            mut self: std::pin::Pin<&mut Self>,
            _cx: &mut std::task::Context<'_>,
            buf: &mut tokio::io::ReadBuf<'_>,
        ) -> std::task::Poll<std::io::Result<()>> {
            if self.sent_partial {
                return std::task::Poll::Ready(Err(std::io::Error::other(
                    "injected reader failure",
                )));
            }

            self.sent_partial = true;
            buf.put_slice(b"partial git protocol response");
            std::task::Poll::Ready(Ok(()))
        }
    }

    /// One pkt-line, the way a git client frames it.
    fn pkt(payload: &str) -> Vec<u8> {
        let mut encoded = format!("{:04x}", payload.len() + 4).into_bytes();
        encoded.extend_from_slice(payload.as_bytes());
        encoded
    }

    /// A request body spooled the way `stage_git_body` leaves it: an open file
    /// positioned at zero, with its `TempPath` still owning the unlink.
    async fn staged_request(bytes: &[u8]) -> super::StagedGitBody {
        let mut file = tempfile::NamedTempFile::new().expect("spool file");
        std::io::Write::write_all(&mut file, bytes).expect("write spool");
        let path = file.into_temp_path();
        let file = tokio::fs::File::open(&path).await.expect("reopen spool");
        super::StagedGitBody { file, path }
    }

    /// Before the first byte of the response, the status is still the server's
    /// to choose — and a handler that fails there must still answer with the
    /// sanitized 500, exactly as the fully-buffered version did.
    ///
    /// The failure is injected as a malformed pkt-line length, which
    /// `read_want_have_split` rejects before anything is written back.
    #[tokio::test]
    async fn upload_pack_failing_before_any_output_is_a_sanitized_500() {
        let response = super::stream_upload_pack_response(
            super::UploadPackProtocol::V1,
            std::path::PathBuf::from("/nonexistent-repo.git"),
            staged_request(b"zzzz").await,
            30,
            30,
            "owner",
            "repo",
        )
        .await;

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("failure body")
            .to_bytes();
        assert_eq!(body.as_ref(), b"internal server error");
    }

    /// The streaming counterpart of the same guarantee, and the reason the
    /// response is not simply handed a graceful end-of-stream: once the pack has
    /// begun there is no status code left, so a failed generation must break the
    /// body instead. A clean end here would hand the client a well-formed,
    /// truncated clone that looks complete (`card_2bfc8c1d8648`).
    ///
    /// `git pack-objects` is pointed at a directory that is not a repository, so
    /// it fails *after* `handle_upload_pack_http` has already written NAK.
    #[tokio::test]
    async fn upload_pack_failing_mid_response_breaks_the_body() {
        if rg_git::cli_gateway::global_gateway().is_err() {
            eprintln!("skipping mid-response failure test: git not available");
            return;
        }

        let not_a_repo = tempfile::tempdir().expect("scratch dir");
        let mut request = pkt(&format!("want {}\0side-band-64k\n", "0".repeat(40)));
        request.extend_from_slice(b"0000");
        request.extend_from_slice(&pkt("done\n"));

        let response = super::stream_upload_pack_response(
            super::UploadPackProtocol::V1,
            not_a_repo.path().to_path_buf(),
            staged_request(&request).await,
            30,
            30,
            "owner",
            "repo",
        )
        .await;

        // NAK is already on the wire, so the transport is committed to 200 …
        assert_eq!(response.status(), StatusCode::OK);
        // … and the only honest way left to say "this is not a whole clone" is
        // to end the body with an error rather than a terminating chunk.
        assert!(
            response.into_body().collect().await.is_err(),
            "a failure after the response began must break the body"
        );
    }

    /// A handler that legitimately produces nothing still answers 200 with an
    /// empty body — the `first == 0` arm must not be mistaken for a failure.
    /// A V2 POST carrying only a flush is exactly that request.
    #[tokio::test]
    async fn upload_pack_producing_no_output_is_an_empty_200() {
        let response = super::stream_upload_pack_response(
            super::UploadPackProtocol::V2,
            std::path::PathBuf::from("/nonexistent-repo.git"),
            staged_request(b"0000").await,
            30,
            30,
            "owner",
            "repo",
        )
        .await;

        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("empty body")
            .to_bytes();
        assert!(body.is_empty(), "flush-only request has nothing to answer");
    }

    async fn assert_receive_pack_reader_failure(
        reader_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>>,
        label: &'static str,
    ) {
        let (status, headers, body) =
            collect_receive_pack_response(reader_task, "read receive-pack response")
                .await
                .expect_err(label);

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{label}");
        assert_eq!(headers[0].1, "text/plain", "{label}");
        let body = body
            .collect()
            .await
            .expect("reader failure body")
            .to_bytes();
        assert_eq!(body.as_ref(), b"internal server error", "{label}");
    }

    /// `handle_git_receive_pack` may only answer `200
    /// application/x-git-receive-pack-result` — and only fire the post-push
    /// hooks — for a response it actually managed to read. A reader that failed
    /// mid-copy, panicked, or was cancelled must surface as a sanitized 5xx
    /// instead of a successful empty push confirmation (card_9c1d563ece91).
    #[tokio::test]
    async fn receive_pack_reader_failures_are_sanitized_5xx() {
        assert_receive_pack_reader_failure(
            spawn_git_response_reader(PartialThenFailReader::default()),
            "partial read then I/O error",
        )
        .await;

        let panic_task: tokio::task::JoinHandle<std::io::Result<Vec<u8>>> =
            tokio::spawn(async { panic!("injected reader task panic") });
        assert_receive_pack_reader_failure(panic_task, "reader task panic").await;

        let cancelled_task = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Ok::<Vec<u8>, std::io::Error>(Vec::new())
        });
        cancelled_task.abort();
        assert_receive_pack_reader_failure(cancelled_task, "cancelled reader task").await;
    }

    /// The guard must not cost a healthy push its response body: a reader that
    /// completes normally still hands the full protocol response to the 200
    /// path.
    #[tokio::test]
    async fn receive_pack_collects_a_complete_response_unchanged() {
        let payload = b"0032unpack ok\n0000".to_vec();
        let reader_task = spawn_git_response_reader(std::io::Cursor::new(payload.clone()));

        let output = collect_receive_pack_response(reader_task, "read receive-pack response")
            .await
            .expect("healthy reader");

        assert_eq!(output, payload);
    }

    fn landed_update() -> RefUpdate {
        RefUpdate {
            old_sha: "0".repeat(40),
            new_sha: "a".repeat(40),
            refname: "refs/heads/main".to_string(),
            status: "ok".to_string(),
            message: "ok".to_string(),
        }
    }

    /// The branch has moved; the client just never heard about it. It must get
    /// the 5xx it is owed — and the post-push hooks must get the update anyway,
    /// because the pusher's retry carries no objects and would be answered
    /// `Everything up-to-date` (card_abd7384eed60).
    #[tokio::test]
    async fn a_push_whose_report_status_died_still_yields_its_ref_updates() {
        let (buf_reader, buf_writer) = tokio::io::duplex(1024);
        let reader_task = spawn_git_response_reader(buf_reader);

        let (ref_updates, (status, _headers, _body)) = finish_landed_receive_pack(
            ReceivePackOutcome {
                ref_updates: vec![landed_update()],
                report_status: Err(anyhow::anyhow!("client hung up")),
            },
            buf_writer,
            reader_task,
        )
        .await;

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(
            ref_updates
                .iter()
                .map(|update| update.refname.as_str())
                .collect::<Vec<_>>(),
            ["refs/heads/main"],
            "the hooks are owed the ref updates of a push that already landed"
        );
    }

    /// The healthy push is unchanged: its report-status is what the client
    /// gets, under `200 …-receive-pack-result`.
    #[tokio::test]
    async fn a_delivered_report_status_answers_200_with_its_own_bytes() {
        let payload = b"0032unpack ok\n0000".to_vec();
        let (buf_reader, mut buf_writer) = tokio::io::duplex(1024);
        let reader_task = spawn_git_response_reader(buf_reader);
        buf_writer.write_all(&payload).await.unwrap();

        let (ref_updates, (status, _headers, body)) = finish_landed_receive_pack(
            ReceivePackOutcome {
                ref_updates: vec![landed_update()],
                report_status: Ok(()),
            },
            buf_writer,
            reader_task,
        )
        .await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(ref_updates.len(), 1);
        assert_eq!(
            body.collect().await.unwrap().to_bytes(),
            Bytes::from(payload)
        );
    }

    /// The push landed and then the wall-clock budget elapsed over the response
    /// write, so no `ReceivePackOutcome` was ever produced. The client is told
    /// `504` — it really does not know what became of its push — but the branch
    /// has moved, its retry would be answered `Everything up-to-date`, and the
    /// hooks are owed the update all the same (card_ca431156e7df).
    #[test]
    fn a_push_timed_out_after_its_refs_landed_still_yields_them() {
        let applied = AppliedRefUpdates::new();
        applied.record(&[landed_update()]);

        let (landed, (status, _headers, _body)) =
            split_failed_receive_pack(ReceivePackFailure::TimedOut, &applied);

        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert_eq!(
            landed
                .as_deref()
                .map(|updates| updates.iter().map(|u| u.refname.as_str()).collect()),
            Some(vec!["refs/heads/main"]),
            "an elapsed budget drops the handler, so the sink is the only place these survive"
        );
    }

    /// A run that never reached its refs owes nothing — the sink is what tells
    /// the two apart, because the failure itself cannot.
    #[test]
    fn a_receive_pack_that_never_reached_its_refs_owes_no_hooks() {
        let applied = AppliedRefUpdates::new();

        let (landed, (status, _headers, _body)) =
            split_failed_receive_pack(ReceivePackFailure::TimedOut, &applied);
        assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
        assert!(landed.is_none(), "no ref was written, so no hook is owed");

        let (landed, (status, _headers, _body)) = split_failed_receive_pack(
            ReceivePackFailure::Handler(anyhow::anyhow!("pack indexing failed")),
            &applied,
        );
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(landed.is_none(), "no ref was written, so no hook is owed");
    }
}
