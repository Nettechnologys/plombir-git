//! HTTP client calls against the Plombir Git server's runner API
//! (registration, job polling, heartbeats, workspace/cache/artifact transfer,
//! status).

use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::workspace::{
    cache_archive_path, cache_download_spool_path, job_workspace_path,
    workspace_download_spool_path,
};

/// Register a runner with the server.
pub async fn register_runner(
    client: &reqwest::Client,
    server: &str,
    repository: &str,
    name: &str,
    labels: &[String],
    auth_token: &str,
) -> Result<(i64, String)> {
    let resp = client
        .post(format!("{}/api/v1/runners/register", server))
        .bearer_auth(auth_token)
        .json(&serde_json::json!({
            "repository": repository,
            "name": name,
            "labels": labels,
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
        }))
        .send()
        .await?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Registration failed ({}): {}", status, body);
    }

    let data: serde_json::Value = resp.json().await?;
    let runner_id = data["id"].as_i64().context("missing id in response")?;
    let token = data["token"]
        .as_str()
        .context("missing token in response")?
        .to_string();

    Ok((runner_id, token))
}

/// Poll for a pending job (long-polling with 30s timeout).
pub async fn poll_job(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    token: &str,
) -> Result<Option<PollJobResponse>> {
    let resp = client
        .get(format!(
            "{}/api/v1/runners/{}/jobs/poll?timeout=30",
            server, runner_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .send()
        .await?;

    match resp.status() {
        s if s == reqwest::StatusCode::NO_CONTENT => Ok(None),
        s if s.is_success() => {
            let job: PollJobResponse = resp.json().await?;
            Ok(Some(job))
        }
        s => {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Poll failed ({}): {}", s, body);
        }
    }
}

#[derive(serde::Deserialize)]
pub struct PollJobResponse {
    pub(crate) job_id: i64,
    pub(crate) name: String,
    pub(crate) script: Vec<String>,
    pub(crate) image: Option<String>,
    pub(crate) variables: Option<serde_json::Value>,
    pub(crate) cache_key: Option<String>,
    pub(crate) cache_paths: Option<Vec<String>>,
    pub(crate) artifact_name: Option<String>,
    pub(crate) artifact_paths: Option<Vec<String>>,
    #[allow(dead_code)]
    pub(crate) timeout: i64,
}

/// Per-request timeout for the heartbeat call.
///
/// The shared client sets only a `connect_timeout` (so long-poll / large
/// transfers aren't cut). A heartbeat is a trivial POST that must never stall
/// the 30s heartbeat loop, so it gets its own short whole-request timeout to
/// survive a server that completes the handshake but then hangs the response.
const HEARTBEAT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(15);

/// Per-request timeout for the deregistration call.
///
/// Deregistration only ever runs while the process is stopping, and a stop is
/// on a stopwatch: `docker compose down` (and systemd) send `SIGTERM` and follow
/// it with `SIGKILL` after a grace period. Waiting on a hung server past that
/// budget does not make the stop graceful, it makes it the same abrupt stop with
/// a pause in front — so this call gets a short whole-request timeout of its
/// own, like the heartbeat.
const DEREGISTER_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);

/// A fire-and-forget report from the runner to the server.
///
/// These calls must not abort the runner — it has a job in hand and the server
/// is the unreliable party — but discarding the outcome hides *both* halves of a
/// failure: the transport error and a non-2xx answer (expired token, deleted
/// runner, 413 on an oversized log). What the operator sees instead is a job
/// stuck in `running`, an empty log, or a runner that went `offline` while it
/// kept building. Each report therefore carries the consequence of losing it,
/// and that consequence goes into the warning.
#[derive(Clone, Copy)]
struct Report {
    /// Short call name, used as the subject of the log line.
    call: &'static str,
    /// What the operator silently loses when this report never lands.
    consequence: &'static str,
}

const HEARTBEAT_REPORT: Report = Report {
    call: "heartbeat",
    consequence: "the server will mark this runner offline while it keeps running jobs",
};

const DEREGISTER_REPORT: Report = Report {
    call: "runner deregistration",
    consequence: "the runner stays in the pool until its heartbeat expires, and the job it was \
                  holding stays 'running' until the stuck-job sweep reclaims it minutes later",
};

const START_JOB_REPORT: Report = Report {
    call: "job start",
    consequence: "the job stays pending on the server although the runner is already executing it",
};

const UPLOAD_LOG_REPORT: Report = Report {
    call: "job log upload",
    consequence: "the job output is lost — the job page will show no log at all",
};

const FINISH_JOB_REPORT: Report = Report {
    call: "job finish",
    consequence: "the job stays 'running' on the server forever",
};

/// Attempts for [`finish_job`] — a lost finish leaves the job running forever.
const FINISH_JOB_ATTEMPTS: u32 = 3;

/// The final log is sent as a replace operation, so repeating it after an
/// ambiguous transport failure cannot append the same output twice.
const LOG_UPLOAD_ATTEMPTS: u32 = 3;
const LOG_UPLOAD_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);
const LOG_UPLOAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// Backoff before the first retry of [`finish_job`]; doubled on each further
/// attempt. Kept short: the runner is holding up its next poll while it retries.
const FINISH_JOB_RETRY_BACKOFF: std::time::Duration = std::time::Duration::from_millis(500);

/// Longest response body echoed into a warning, in characters. A 5xx from a
/// reverse proxy can be a full HTML page; the first few hundred characters
/// identify it without flooding the runner's log.
const MAX_LOGGED_BODY: usize = 512;

/// Why a fire-and-forget call did not land.
struct SendFailure {
    /// Human-readable cause: transport error chain, or status + response body.
    detail: String,
    /// Whether repeating the call could plausibly succeed. Transport errors and
    /// 5xx are transient; a 4xx (expired token, unknown runner, oversized log)
    /// answers the same way no matter how often it is asked.
    retryable: bool,
}

/// Flatten an error and its `source` chain into a single line.
///
/// `reqwest::Error`'s own `Display` is deliberately generic ("error sending
/// request for url (…)"); the actionable reason — connection refused, DNS
/// failure, TLS handshake rejected — lives one or two `source()` hops down.
fn error_chain(error: &dyn std::error::Error) -> String {
    let mut chain = error.to_string();
    let mut source = error.source();
    while let Some(cause) = source {
        chain.push_str(": ");
        chain.push_str(&cause.to_string());
        source = cause.source();
    }
    chain
}

/// Trim a response body down to something safe to put in a log line.
fn body_excerpt(body: &str) -> String {
    let body = body.trim();
    if body.is_empty() {
        return "<empty body>".to_string();
    }
    let mut excerpt: String = body.chars().take(MAX_LOGGED_BODY).collect();
    if excerpt.len() < body.len() {
        excerpt.push_str("… (truncated)");
    }
    excerpt
}

/// Perform a request and describe why it failed, or `None` when it landed.
async fn describe_send_failure(request: reqwest::RequestBuilder) -> Option<SendFailure> {
    match request.send().await {
        Ok(response) if response.status().is_success() => None,
        Ok(response) => {
            let status = response.status();
            // Read the body before formatting: `text()` consumes the response.
            let body = response.text().await.unwrap_or_default();
            Some(SendFailure {
                detail: format!("server answered {status}: {}", body_excerpt(&body)),
                retryable: status.is_server_error(),
            })
        }
        Err(error) => Some(SendFailure {
            detail: format!("transport error: {}", error_chain(&error)),
            retryable: true,
        }),
    }
}

/// Log a lost report at warn level, naming the runner, the job and the
/// consequence. Never propagates — the caller stays fail-soft by design.
fn warn_report_lost(report: Report, runner_id: i64, job_id: Option<i64>, failure: &SendFailure) {
    match job_id {
        Some(job_id) => tracing::warn!(
            runner_id,
            job_id,
            "{} report failed: {}; {}",
            report.call,
            failure.detail,
            report.consequence
        ),
        None => tracing::warn!(
            runner_id,
            "{} report failed: {}; {}",
            report.call,
            failure.detail,
            report.consequence
        ),
    }
}

/// Send a fire-and-forget report, logging (never propagating) a failure.
async fn send_report(
    request: reqwest::RequestBuilder,
    report: Report,
    runner_id: i64,
    job_id: Option<i64>,
) {
    if let Some(failure) = describe_send_failure(request).await {
        warn_report_lost(report, runner_id, job_id, &failure);
    }
}

/// Send a heartbeat to keep the runner marked as online.
pub async fn send_heartbeat(client: &reqwest::Client, server: &str, runner_id: i64, token: &str) {
    let request = client
        .post(format!("{}/api/v1/runners/{}/heartbeat", server, runner_id))
        .header("Authorization", format!("Bearer {}", token))
        .timeout(HEARTBEAT_TIMEOUT);
    send_report(request, HEARTBEAT_REPORT, runner_id, None).await;
}

/// Announce that this runner is stopping: the server hands whatever jobs it was
/// holding back to the pool and drops it from the runner list, as one
/// transaction (`runner_ops::deregister_runner`).
///
/// Fire-and-forget like the other reports, and for the stronger of the usual
/// reasons: the process is already on its way out, so there is nothing left for
/// an error to abort. What the operator loses when it does not land is the whole
/// point of the call — the jobs wait out the stuck-job sweep and the runner
/// waits out its heartbeat, which is exactly the state a planned restart used to
/// leave behind.
pub async fn deregister_runner(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    token: &str,
) {
    let request = client
        .post(format!(
            "{}/api/v1/runners/{}/deregister",
            server, runner_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .timeout(DEREGISTER_TIMEOUT);
    send_report(request, DEREGISTER_REPORT, runner_id, None).await;
}

/// Whether an answer from the server says the job is no longer this runner's.
///
/// `409` is the server settling it (canceled, or finished by somebody else);
/// `404` is a job no longer assigned here — reclaimed and handed to another
/// runner, or gone with its repository; `401` / `403` / `410` are a runner the
/// server no longer recognises, whose jobs an admin deletion already handed
/// back. In every one of them carrying on executes work whose result the
/// server will refuse, while somebody else may already be running it.
fn disowns_the_job(status: reqwest::StatusCode) -> bool {
    matches!(status.as_u16(), 401 | 403 | 404 | 409 | 410)
}

/// What the server said to [`start_job`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobStart {
    /// The job is `running` on this runner.
    Started,
    /// The server refused: the job settled (a cancellation, usually) or is no
    /// longer this runner's. It must not be executed.
    Refused,
    /// No answer that decides it — a transport error or a `5xx`. Executing is
    /// still right: a start that did not land leaves the job `assigned`, which
    /// the finish report settles as well, and the liveness check that runs
    /// beside the job is what notices a cancellation.
    Unconfirmed,
}

/// Notify the server that job execution has started, and hear whether it may.
///
/// The server already answered `409` to a job that settled before its runner
/// picked it up, but this call was fire-and-forget and the runner executed the
/// job anyway — a canceled deploy ran to completion (card_a0377b61860e).
pub async fn start_job(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
) -> JobStart {
    let request = client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/start",
            server, runner_id, job_id
        ))
        .header("Authorization", format!("Bearer {}", token));
    match request.send().await {
        Ok(response) if response.status().is_success() => JobStart::Started,
        Ok(response) if disowns_the_job(response.status()) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::info!(
                runner_id,
                job_id,
                "the server refused to start the job ({status}: {}); not executing it",
                body_excerpt(&body)
            );
            JobStart::Refused
        }
        Ok(response) => {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            warn_report_lost(
                START_JOB_REPORT,
                runner_id,
                Some(job_id),
                &SendFailure {
                    detail: format!("server answered {status}: {}", body_excerpt(&body)),
                    retryable: status.is_server_error(),
                },
            );
            JobStart::Unconfirmed
        }
        Err(error) => {
            warn_report_lost(
                START_JOB_REPORT,
                runner_id,
                Some(job_id),
                &SendFailure {
                    detail: format!("transport error: {}", error_chain(&error)),
                    retryable: true,
                },
            );
            JobStart::Unconfirmed
        }
    }
}

/// Whether the job this runner is executing is still its to run.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum JobLiveness {
    /// Still active work on this runner.
    Active,
    /// Canceled, settled, or no longer this runner's: stop executing it and
    /// publish nothing for it.
    Disowned,
    /// No deciding answer. Not a cancellation — a server restarting must not
    /// kill every build in flight; the next check asks again.
    Unknown,
}

/// Per-request timeout for [`job_liveness`]: the same reasoning, and the same
/// number, as the heartbeat's.
const JOB_STATUS_TIMEOUT: std::time::Duration = HEARTBEAT_TIMEOUT;

#[derive(serde::Deserialize)]
struct JobStatusBody {
    active: bool,
}

/// Ask the server whether `job_id` is still this runner's to run.
///
/// Asked on a period while the job executes and once more before its cache or
/// artifact is published — the only way a cancellation, which is a write on the
/// server, reaches work running on another machine (card_a0377b61860e).
pub async fn job_liveness(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
) -> JobLiveness {
    let request = client
        .get(format!(
            "{}/api/v1/runners/{}/jobs/{}/status",
            server, runner_id, job_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .timeout(JOB_STATUS_TIMEOUT);
    match request.send().await {
        Ok(response) if response.status().is_success() => {
            match response.json::<JobStatusBody>().await {
                Ok(JobStatusBody { active: true }) => JobLiveness::Active,
                Ok(JobStatusBody { active: false }) => JobLiveness::Disowned,
                Err(error) => {
                    tracing::warn!(
                        runner_id,
                        job_id,
                        "job status answer could not be read: {}; carrying on",
                        error_chain(&error)
                    );
                    JobLiveness::Unknown
                }
            }
        }
        Ok(response) if disowns_the_job(response.status()) => JobLiveness::Disowned,
        Ok(response) => {
            tracing::warn!(
                runner_id,
                job_id,
                "job status check answered {}; carrying on",
                response.status()
            );
            JobLiveness::Unknown
        }
        Err(error) => {
            tracing::warn!(
                runner_id,
                job_id,
                "job status check failed: {}; carrying on",
                error_chain(&error)
            );
            JobLiveness::Unknown
        }
    }
}

/// The most one log upload may carry.
///
/// The server declares the same ceiling on `POST
/// /api/v1/runners/{id}/jobs/{job_id}/log` (`api::runners::JOB_LOG_MAX_BYTES`);
/// `ci_job_log_boundary_tests` drives this very function against the live
/// router, which is the only place both halves of that agreement exist.
const LOG_UPLOAD_MAX_BYTES: usize = 8 * 1024 * 1024;

/// Room reserved inside [`LOG_UPLOAD_MAX_BYTES`] for the truncation notice, so
/// the shortened upload still fits under the ceiling that caused it.
const TRUNCATION_NOTICE_BUDGET: usize = 256;

/// Shorten `log` to what one upload may carry, keeping the tail and saying so.
///
/// The alternative is to send the whole thing and let the server refuse it: the
/// job then finishes with an empty log, which reads in the UI as "this build
/// printed nothing" rather than "the log did not fit". The tail is the half
/// kept because that is where a failing command's error and the artifact notice
/// this runner appends both live.
fn trim_log_for_upload(log: &str) -> std::borrow::Cow<'_, str> {
    if log.len() <= LOG_UPLOAD_MAX_BYTES {
        return std::borrow::Cow::Borrowed(log);
    }

    let mut cut = log.len() - (LOG_UPLOAD_MAX_BYTES - TRUNCATION_NOTICE_BUDGET);
    while !log.is_char_boundary(cut) {
        cut += 1;
    }
    let notice = format!(
        "[plombir-git-runner] log truncated: {cut} of {} bytes dropped, the tail follows\n",
        log.len()
    );
    debug_assert!(
        notice.len() <= TRUNCATION_NOTICE_BUDGET,
        "the notice must fit the room reserved for it, or the trimmed log is over the ceiling again"
    );

    let mut trimmed = String::with_capacity(notice.len() + log.len() - cut);
    trimmed.push_str(&notice);
    trimmed.push_str(&log[cut..]);
    std::borrow::Cow::Owned(trimmed)
}

/// Upload job log output.
pub async fn upload_log(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    log: &str,
) {
    let url = format!("{server}/api/v1/runners/{runner_id}/jobs/{job_id}/log");
    let body = trim_log_for_upload(log).into_owned();
    let mut backoff = LOG_UPLOAD_RETRY_BACKOFF;
    for attempt in 1..=LOG_UPLOAD_ATTEMPTS {
        let request = client
            .post(&url)
            .header("Authorization", format!("Bearer {token}"))
            .header("x-job-log-mode", "replace")
            .timeout(LOG_UPLOAD_TIMEOUT)
            .body(body.clone());
        match describe_send_failure(request).await {
            None => return,
            Some(failure) if failure.retryable && attempt < LOG_UPLOAD_ATTEMPTS => {
                tokio::time::sleep(backoff).await;
                backoff *= 2;
            }
            Some(failure) => {
                warn_report_lost(UPLOAD_LOG_REPORT, runner_id, Some(job_id), &failure);
                return;
            }
        }
    }
}

pub async fn download_workspace(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
) -> Result<PathBuf> {
    let response = client
        .get(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/workspace"
        ))
        .bearer_auth(token)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!(
            "workspace download failed ({status}): {}",
            response.text().await.unwrap_or_default()
        );
    }
    let workspace = job_workspace_path(job_id);
    if let Some(parent) = workspace.parent() {
        tokio::fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "failed to create runner workspace parent `{}` (set TMPDIR to a writable directory)",
                parent.display()
            )
        })?;
    }
    let spool = workspace_download_spool_path(&workspace);
    let spool_outcome = spool_response_body(response, &spool, "workspace").await;
    let unpack_outcome = match spool_outcome {
        Ok(_) => {
            let unpack_path = workspace.clone();
            let spool_path = spool.clone();
            match tokio::task::spawn_blocking(move || {
                unpack_archive_from_file(&spool_path, &unpack_path, "runner workspace")
            })
            .await
            {
                Ok(inner) => inner,
                Err(error) => Err(anyhow::anyhow!(
                    "unpacking the workspace archive panicked: {error}"
                )),
            }
        }
        Err(error) => Err(error),
    };
    // The spool is this download's alone and nothing else reads it, so it goes
    // whether the unpack succeeded or not — mirrors `save_cache`'s cleanup of
    // its packed archive on the upload side.
    remove_download_spool(&spool, job_id, "workspace").await;
    unpack_outcome?;
    Ok(workspace)
}

/// Replace `unpack_path` with the contents of a tar archive already on disk.
///
/// Every message names the path: the workspace lives under `TMPDIR`, so in a
/// container the failure is usually "that directory is read-only / owned by
/// another uid", and a bare `Permission denied (os error 13)` doesn't tell the
/// operator which directory to fix — or that `TMPDIR` is the knob. `what` names
/// what is being written so the same helper serves both the workspace tar and
/// the CI cache tar without either error line lying about which one broke.
fn unpack_archive_from_file(
    archive: &std::path::Path,
    unpack_path: &std::path::Path,
    what: &str,
) -> Result<()> {
    if unpack_path.exists() {
        std::fs::remove_dir_all(unpack_path).with_context(|| {
            format!("failed to remove stale {what} `{}`", unpack_path.display())
        })?;
    }
    std::fs::create_dir_all(unpack_path).with_context(|| {
        format!(
            "failed to create {what} `{}` (set TMPDIR to a writable directory)",
            unpack_path.display()
        )
    })?;
    let file = std::fs::File::open(archive).with_context(|| {
        format!(
            "failed to open {what} download spool `{}`",
            archive.display()
        )
    })?;
    rg_process::workspace_archive::unpack_into(std::io::BufReader::new(file), unpack_path)
        .with_context(|| format!("failed to unpack {what} into `{}`", unpack_path.display()))?;
    Ok(())
}

pub async fn restore_cache(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    key: &str,
    workspace: &std::path::Path,
) -> Result<bool> {
    let response = client
        .get(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/cache"
        ))
        .bearer_auth(token)
        .header("x-cache-key", key)
        .send()
        .await?;
    if response.status() == reqwest::StatusCode::NOT_FOUND {
        return Ok(false);
    }
    if !response.status().is_success() {
        anyhow::bail!(
            "cache restore failed: {}",
            response.text().await.unwrap_or_default()
        );
    }
    if let Some(parent) = workspace.parent() {
        tokio::fs::create_dir_all(parent).await.with_context(|| {
            format!(
                "failed to prepare CI cache spool directory `{}`",
                parent.display()
            )
        })?;
    }
    let spool = cache_download_spool_path(workspace);
    let spool_outcome = spool_response_body(response, &spool, "cache").await;
    let workspace = workspace.to_path_buf();
    let unpack_outcome = match spool_outcome {
        Ok(_) => {
            let spool_path = spool.clone();
            match tokio::task::spawn_blocking(move || {
                let file = std::fs::File::open(&spool_path).with_context(|| {
                    format!(
                        "failed to open cache download spool `{}`",
                        spool_path.display()
                    )
                })?;
                rg_process::workspace_archive::unpack_into(
                    std::io::BufReader::new(file),
                    &workspace,
                )
                .context("unpack job cache")
            })
            .await
            {
                Ok(inner) => inner,
                Err(error) => Err(anyhow::anyhow!(
                    "unpacking the cache archive panicked: {error}"
                )),
            }
        }
        Err(error) => Err(error),
    };
    remove_download_spool(&spool, job_id, "cache").await;
    unpack_outcome?;
    Ok(true)
}

/// Copy one response body onto disk a chunk at a time and return the size
/// written, verifying the declared `Content-Length` where the server sent one.
///
/// The two runner download endpoints answer with the same class of payload the
/// server side used to build in memory before `card_c1aa8089607d` — a workspace
/// tar as large as the repository, a CI cache archive up to a gigabyte
/// (`rg_http::api::runners::CACHE_ARCHIVE_MAX_BYTES`). Reading them through
/// `response.bytes()` on the client rebuilt that heap allocation one hop away,
/// so a runner in a container with a memory limit would OOM on a legitimate
/// large cache — the transfer would land, and the process would die restoring
/// it. Streaming onto disk keeps what the process holds at one chunk instead of
/// one archive.
///
/// The tail check on `Content-Length` (present on the CI cache route, absent on
/// the workspace route because `git archive` is chunked) is what tells a
/// mid-transfer disconnect from a valid short tar: a truncated tar looks like a
/// short-but-valid one to `unpack`, silently omitting the entries that never
/// arrived.
async fn spool_response_body(
    response: reqwest::Response,
    spool: &std::path::Path,
    what: &str,
) -> Result<u64> {
    use futures::StreamExt;
    use tokio::io::AsyncWriteExt;

    let declared = response.content_length();
    let mut file = tokio::fs::File::create(spool).await.with_context(|| {
        format!(
            "failed to create {what} download spool `{}`",
            spool.display()
        )
    })?;
    let mut stream = response.bytes_stream();
    let mut written: u64 = 0;
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.with_context(|| format!("{what} download stream failed"))?;
        file.write_all(&chunk).await.with_context(|| {
            format!(
                "failed to write {what} download spool `{}`",
                spool.display()
            )
        })?;
        written += chunk.len() as u64;
    }
    file.flush().await.with_context(|| {
        format!(
            "failed to flush {what} download spool `{}`",
            spool.display()
        )
    })?;
    if let Some(declared) = declared {
        if declared != written {
            anyhow::bail!(
                "{what} download truncated: server declared {declared} bytes but body carried \
                 {written}"
            );
        }
    }
    Ok(written)
}

/// Remove a download spool on every exit path, logging (never propagating) an
/// unexpected failure — the outcome of the download itself already reached the
/// caller by the time this runs.
async fn remove_download_spool(spool: &std::path::Path, job_id: i64, what: &str) {
    if let Err(error) = tokio::fs::remove_file(spool).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                job_id,
                path = %spool.display(),
                %error,
                "failed to remove the {what} download spool"
            );
        }
    }
}

/// The ceiling the server declares for one runner-uploaded archive — the CI
/// cache and the artifact staging route share it
/// (`rg_http::api::runners::CACHE_ARCHIVE_MAX_BYTES`).
///
/// Mirrored here so an archive that cannot be accepted is answered by this
/// runner with a sentence rather than by a gigabyte on the wire and a `413` at
/// the end of it — the same reason `trim_log_for_upload` knows the log ceiling.
pub(crate) const UPLOAD_ARCHIVE_MAX_BYTES: u64 = 1024 * 1024 * 1024;

/// Send an already-packed archive as a request body without reading it into
/// memory.
///
/// Both upload routes take an archive as large as the build that produced it,
/// and both used to be handed a `Vec<u8>` of exactly that size: the runner paid
/// the archive twice, once on disk and once on the heap, for the whole duration
/// of the transfer. `ReaderStream` hands the file to hyper a chunk at a time,
/// so what this process holds is a chunk rather than a build.
async fn archive_body(path: &std::path::Path) -> Result<reqwest::Body> {
    let file = tokio::fs::File::open(path)
        .await
        .with_context(|| format!("failed to open the packed archive `{}`", path.display()))?;
    Ok(reqwest::Body::wrap_stream(
        tokio_util::io::ReaderStream::new(file),
    ))
}

/// Pack `paths` out of the workspace into `archive`, returning its size.
///
/// Packed to a file rather than to a `Vec`, and packed *beside* the workspace
/// rather than inside it — an archive written into the very tree it is walking
/// races that walk.
fn pack_cache_archive(
    workspace: &std::path::Path,
    paths: &[String],
    archive: &std::path::Path,
) -> Result<u64> {
    let file = std::fs::File::create(archive)
        .with_context(|| format!("failed to create the cache archive `{}`", archive.display()))?;
    let mut builder = rg_process::workspace_archive::WorkspaceArchive::new(workspace, file)?;
    for path in paths {
        builder
            .append_declared(path)
            .with_context(|| format!("failed to add `{path}` to the cache archive"))?;
    }
    let mut file = builder.finish()?;
    use std::io::Write;
    file.flush()?;
    let len = file
        .metadata()
        .with_context(|| {
            format!(
                "failed to measure the cache archive `{}`",
                archive.display()
            )
        })?
        .len();
    Ok(len)
}

#[allow(clippy::too_many_arguments)]
pub async fn save_cache(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    key: &str,
    paths: &[String],
    workspace: &std::path::Path,
) -> Result<()> {
    let archive = cache_archive_path(workspace);
    let paths = paths.to_vec();
    let pack_workspace = workspace.to_path_buf();
    let pack_archive = archive.clone();
    let packed = tokio::task::spawn_blocking(move || {
        pack_cache_archive(&pack_workspace, &paths, &pack_archive)
    })
    .await;
    let outcome = match packed {
        Ok(Ok(len)) => {
            upload_cache_archive(client, server, runner_id, job_id, token, key, &archive, len).await
        }
        Ok(Err(error)) => Err(error),
        Err(error) => Err(anyhow::anyhow!(
            "packing the cache archive panicked: {error}"
        )),
    };
    // The packed archive is this save's alone and nothing else ever reads it,
    // so it goes whether the upload succeeded or not.
    if let Err(error) = tokio::fs::remove_file(&archive).await {
        if error.kind() != std::io::ErrorKind::NotFound {
            tracing::warn!(
                job_id,
                path = %archive.display(),
                %error,
                "failed to remove the packed cache archive"
            );
        }
    }
    outcome
}

/// Hand one packed cache archive to the server, or say why it was not sent.
#[allow(clippy::too_many_arguments)]
async fn upload_cache_archive(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    key: &str,
    archive: &std::path::Path,
    len: u64,
) -> Result<()> {
    if len == 0 {
        return Ok(());
    }
    // Refused here rather than on the wire. The route declares this ceiling and
    // answers a larger body with `413`, so sending it would move a gigabyte
    // across the network to be told what this line already knows — and the
    // runner would report a transport failure where the truth is that the cache
    // this job declared does not fit the one the instance accepts.
    if len > UPLOAD_ARCHIVE_MAX_BYTES {
        anyhow::bail!(
            "cache archive is {len} bytes, over the {UPLOAD_ARCHIVE_MAX_BYTES}-byte ceiling this \
             server accepts; nothing was uploaded and the cache stays as it was"
        );
    }
    let response = client
        .put(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/cache"
        ))
        .bearer_auth(token)
        .header("x-cache-key", key)
        .header(reqwest::header::CONTENT_TYPE, "application/x-tar")
        .header(reqwest::header::CONTENT_LENGTH, len)
        .body(archive_body(archive).await?)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!(
            "cache upload failed ({status}): {}",
            response.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

/// Stream a packed artifact archive into the job's server-side storage and
/// answer with the path it landed on.
///
/// The publish call below takes JSON metadata only — an artifact-sized request
/// body is exactly what it exists to avoid — so the bytes travel here and only
/// the resulting path travels there. Kept a separate call rather than folded
/// into [`publish_artifact`] so each URL is one function, which is what
/// `runner_route_coverage_tests` can hold against the route table.
pub async fn stage_artifact(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    archive: &std::path::Path,
) -> Result<String> {
    let len = tokio::fs::metadata(archive)
        .await
        .with_context(|| {
            format!(
                "failed to measure the packed artifact archive `{}`",
                archive.display()
            )
        })?
        .len();
    if len == 0 {
        anyhow::bail!("packed artifact archive is empty");
    }
    // Same ceiling, same reason as the cache: an archive the route will refuse
    // is refused here, before a build's worth of bytes goes on the wire.
    if len > UPLOAD_ARCHIVE_MAX_BYTES {
        anyhow::bail!(
            "packed artifact archive is {len} bytes, over the {UPLOAD_ARCHIVE_MAX_BYTES}-byte \
             ceiling this server accepts; nothing was staged"
        );
    }
    let response = client
        .put(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/artifacts/staging"
        ))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/x-tar")
        .header(reqwest::header::CONTENT_LENGTH, len)
        .body(archive_body(archive).await?)
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!(
            "artifact staging failed ({status}): {}",
            response.text().await.unwrap_or_default()
        );
    }
    #[derive(serde::Deserialize)]
    struct Staged {
        file_path: String,
    }
    let staged: Staged = response
        .json()
        .await
        .context("artifact staging answered a body this runner cannot read")?;
    Ok(staged.file_path)
}

/// Publish the metadata row that names an already-staged archive, which is what
/// makes the artifact appear on the pipeline.
pub async fn publish_artifact(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    name: &str,
    staged_path: &str,
) -> Result<()> {
    let response = client
        .post(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/artifacts"
        ))
        .bearer_auth(token)
        .json(&serde_json::json!({ "name": name, "file_path": staged_path }))
        .send()
        .await?;
    if !response.status().is_success() {
        let status = response.status();
        anyhow::bail!(
            "artifact publication failed ({status}): {}",
            response.text().await.unwrap_or_default()
        );
    }
    Ok(())
}

/// Report job completion.
///
/// The only report that is retried: the runner has already moved on to its next
/// poll, and nothing else on the server ever takes the job out of `running`, so
/// a lost finish is not eventually consistent — it is permanently wrong. Retries
/// cover the transient half (transport error, server restarting, 5xx); a 4xx is
/// reported once and dropped, because repeating it changes nothing.
pub async fn finish_job(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
    status: &str,
    exit_code: i32,
) {
    let url = format!(
        "{}/api/v1/runners/{}/jobs/{}/finish",
        server, runner_id, job_id
    );
    let payload = serde_json::json!({"status": status, "exit_code": exit_code});
    let mut backoff = FINISH_JOB_RETRY_BACKOFF;

    for attempt in 1..=FINISH_JOB_ATTEMPTS {
        let request = client
            .post(&url)
            .header("Authorization", format!("Bearer {}", token))
            .json(&payload);
        let failure = match request.send().await {
            Ok(response) if response.status().is_success() => return,
            // The server had already settled the job — canceled while it ran,
            // most often. The report still landed where it matters: the same
            // transaction put this runner back to `online`. Warning that the
            // job "stays running forever" would be the opposite of the truth.
            Ok(response) if response.status() == reqwest::StatusCode::CONFLICT => {
                tracing::info!(
                    runner_id,
                    job_id,
                    "the server had already settled this job; its result was not applied"
                );
                return;
            }
            Ok(response) => {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                SendFailure {
                    detail: format!("server answered {status}: {}", body_excerpt(&body)),
                    retryable: status.is_server_error(),
                }
            }
            Err(error) => SendFailure {
                detail: format!("transport error: {}", error_chain(&error)),
                retryable: true,
            },
        };
        if !failure.retryable || attempt == FINISH_JOB_ATTEMPTS {
            warn_report_lost(FINISH_JOB_REPORT, runner_id, Some(job_id), &failure);
            return;
        }
        tracing::warn!(
            runner_id,
            job_id,
            attempt,
            attempts = FINISH_JOB_ATTEMPTS,
            "{} report failed: {}; retrying in {:?}",
            FINISH_JOB_REPORT.call,
            failure.detail,
            backoff
        );
        tokio::time::sleep(backoff).await;
        backoff *= 2;
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{
        body_excerpt, error_chain, finish_job, job_liveness, pack_cache_archive, send_heartbeat,
        start_job, trim_log_for_upload, unpack_archive_from_file, upload_log, JobLiveness,
        JobStart, FINISH_JOB_ATTEMPTS, LOG_UPLOAD_MAX_BYTES, MAX_LOGGED_BODY,
    };

    /// The cache half of `executor`'s
    /// `packing_an_artifact_never_follows_a_symlink_out_of_the_workspace`: a
    /// cache is uploaded to the server and restored into every later job, so a
    /// followed link would carry the runner host's files to all of them
    /// (card_79b1c2a906ec).
    #[cfg(unix)]
    #[test]
    fn packing_a_cache_never_follows_a_symlink_out_of_the_workspace() {
        use std::os::unix::fs::symlink;
        const SECRET: &[u8] = b"runner-host-cache-secret";

        let root = tempfile::tempdir().unwrap();
        // Archives land outside the tree under test: one written inside a
        // directory a followed link walks would grow by reading itself.
        let output = tempfile::tempdir().unwrap();
        let workspace = root.path().join("job-7");
        std::fs::create_dir_all(workspace.join("target")).unwrap();
        std::fs::write(root.path().join("runner.toml"), SECRET).unwrap();
        symlink("../../runner.toml", workspace.join("target/relative")).unwrap();
        symlink(
            root.path().join("runner.toml"),
            workspace.join("target/absolute"),
        )
        .unwrap();
        symlink("..", workspace.join("leak-relative")).unwrap();
        symlink(root.path(), workspace.join("leak-absolute")).unwrap();
        let carries_secret = |archive: &std::path::Path| {
            let bytes = std::fs::read(archive).unwrap();
            bytes.windows(SECRET.len()).any(|window| window == SECRET)
        };

        let archive = output.path().join("target.tar");
        pack_cache_archive(&workspace, &["target".to_string()], &archive)
            .expect("target must pack");
        assert!(
            !carries_secret(&archive),
            "the cache carries a file a symlink inside `target` points at"
        );

        for declared in ["leak-relative", "leak-absolute/runner.toml"] {
            let archive = output
                .path()
                .join(format!("{}.tar", declared.replace('/', "-")));
            let error = pack_cache_archive(&workspace, &[declared.to_string()], &archive)
                .expect_err("a declared path resolving out of the workspace must be refused");
            assert!(
                format!("{error:#}").contains("leaves the CI workspace"),
                "{declared}: {error:#}"
            );
            assert!(
                !archive.exists() || !carries_secret(&archive),
                "the cache for `{declared}` carries the target's bytes"
            );
        }
    }

    /// Sink that keeps every formatted log line so a test can assert on what the
    /// operator would actually have seen.
    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }
    }

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

    /// Install a scoped subscriber capturing warnings on this thread. The guard
    /// must stay alive for the whole test (`#[tokio::test]` is single-threaded,
    /// so the awaited code runs on the same thread).
    fn capture_logs() -> (CapturedLogs, tracing::subscriber::DefaultGuard) {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (logs, guard)
    }

    struct FakeServer {
        url: String,
        requests: Arc<AtomicUsize>,
    }

    impl FakeServer {
        fn requests(&self) -> usize {
            self.requests.load(Ordering::SeqCst)
        }
    }

    /// Read one HTTP request (headers plus the body it declares) off the socket,
    /// so the client always sees a reply instead of a reset connection.
    async fn read_request(stream: &mut tokio::net::TcpStream) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..read]);
            let Some(header_end) = buf
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .map(|at| at + 4)
            else {
                continue;
            };
            let headers = String::from_utf8_lossy(&buf[..header_end]).to_ascii_lowercase();
            let body_len = headers
                .lines()
                .find_map(|line| line.strip_prefix("content-length:"))
                .and_then(|value| value.trim().parse::<usize>().ok())
                .unwrap_or(0);
            if buf.len() >= header_end + body_len {
                return Ok(());
            }
        }
    }

    /// Minimal HTTP server answering every request with one canned status/body
    /// and counting how many it served. The workspace carries no HTTP-mock
    /// dependency, and these calls only need a socket that talks back.
    async fn spawn_fake_server(status_line: &'static str, body: &'static str) -> FakeServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let requests = Arc::new(AtomicUsize::new(0));
        let served = Arc::clone(&requests);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let served = Arc::clone(&served);
                let response = format!(
                    "HTTP/1.1 {status_line}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                tokio::spawn(async move {
                    if read_request(&mut stream).await.is_ok() {
                        served.fetch_add(1, Ordering::SeqCst);
                        if stream.write_all(response.as_bytes()).await.is_err() {
                            return;
                        }
                        if stream.shutdown().await.is_err() {
                            // Client closed after reading the response.
                        }
                    }
                });
            }
        });
        FakeServer { url, requests }
    }

    /// A listener that owns its port and severs every accepted connection.
    /// This provokes a transport error without releasing an ephemeral port for
    /// another test process to claim first.
    async fn refusing_server_url() -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        url
    }

    #[derive(Debug)]
    struct Layer {
        message: &'static str,
        source: Option<Box<Layer>>,
    }

    impl std::fmt::Display for Layer {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            f.write_str(self.message)
        }
    }

    impl std::error::Error for Layer {
        fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
            self.source
                .as_deref()
                .map(|source| source as &(dyn std::error::Error + 'static))
        }
    }

    #[test]
    fn error_chain_keeps_the_causes_a_bare_display_would_drop() {
        let error = Layer {
            message: "error sending request",
            source: Some(Box::new(Layer {
                message: "tcp connect error",
                source: Some(Box::new(Layer {
                    message: "connection refused",
                    source: None,
                })),
            })),
        };

        assert_eq!(
            error_chain(&error),
            "error sending request: tcp connect error: connection refused"
        );
    }

    #[test]
    fn body_excerpt_names_an_empty_body_and_truncates_a_huge_one() {
        assert_eq!(body_excerpt("   \n "), "<empty body>");
        assert_eq!(body_excerpt("  token expired  "), "token expired");

        let excerpt = body_excerpt(&"x".repeat(MAX_LOGGED_BODY * 2));
        assert!(excerpt.ends_with("… (truncated)"), "{excerpt}");
        assert!(excerpt.len() < MAX_LOGGED_BODY * 2, "{}", excerpt.len());
    }

    #[test]
    fn a_log_within_the_ceiling_is_uploaded_untouched() {
        let log = "cargo build\n".repeat(4096);
        assert!(log.len() < LOG_UPLOAD_MAX_BYTES);
        assert!(matches!(
            trim_log_for_upload(&log),
            std::borrow::Cow::Borrowed(_)
        ));
        assert_eq!(trim_log_for_upload(&log), log);
    }

    #[test]
    fn an_oversized_log_keeps_its_tail_under_the_ceiling_and_says_what_it_dropped() {
        // A multi-byte character on every line, so a naive byte cut would land
        // mid-character and panic rather than move to the next boundary.
        let line = "шаг компиляции завершён\n";
        let log = line.repeat(LOG_UPLOAD_MAX_BYTES / line.len() + 1024);
        assert!(log.len() > LOG_UPLOAD_MAX_BYTES);

        let trimmed = trim_log_for_upload(&log);
        assert!(
            trimmed.len() <= LOG_UPLOAD_MAX_BYTES,
            "the trimmed upload must fit the ceiling that caused the trim: {}",
            trimmed.len()
        );
        assert!(
            trimmed.starts_with("[plombir-git-runner] log truncated:"),
            "the loss has to be visible in the log the operator reads: {}",
            &trimmed[..trimmed.len().min(120)]
        );
        assert!(
            trimmed.contains(&format!("of {} bytes dropped", log.len())),
            "the notice names how much log there was"
        );
        assert!(
            trimmed.ends_with(line),
            "the tail is the half kept — that is where a failure's error lands"
        );
    }

    #[test]
    fn an_uncreatable_workspace_is_reported_with_its_path_and_the_tmpdir_knob() {
        let dir = tempfile::tempdir().unwrap();
        // A regular file where the parent directory should be — the same shape
        // as an unwritable / occupied TMPDIR, without needing a hostile FS.
        let blocker = dir.path().join("not-a-directory");
        std::fs::write(&blocker, b"x").unwrap();
        let unpack_path = blocker.join("jobs").join("42");
        let archive = dir.path().join("empty.tar");
        std::fs::write(&archive, b"").unwrap();

        let error = unpack_archive_from_file(&archive, &unpack_path, "runner workspace")
            .expect_err("creating a workspace under a regular file must fail");

        let message = format!("{error:#}");
        assert!(
            message.contains(&unpack_path.display().to_string()),
            "{message}"
        );
        assert!(message.contains("TMPDIR"), "{message}");
    }

    #[test]
    fn a_corrupt_workspace_archive_is_reported_with_the_target_path() {
        let dir = tempfile::tempdir().unwrap();
        let unpack_path = dir.path().join("job-7");
        let archive = dir.path().join("garbage.tar");
        std::fs::write(&archive, b"this is not a tar archive").unwrap();

        let error = unpack_archive_from_file(&archive, &unpack_path, "runner workspace")
            .expect_err("a corrupt archive must not unpack");

        let message = format!("{error:#}");
        assert!(
            message.contains(&unpack_path.display().to_string()),
            "{message}"
        );
        assert!(message.contains("unpack runner workspace"), "{message}");
    }

    #[tokio::test]
    async fn upload_log_reports_a_rejected_upload_with_status_body_and_ids() {
        let server =
            spawn_fake_server("413 Payload Too Large", "log exceeds the 5 MiB limit").await;
        let (logs, _guard) = capture_logs();

        upload_log(
            &reqwest::Client::new(),
            &server.url,
            7,
            42,
            "token",
            "job output",
        )
        .await;

        let logs = logs.text();
        assert!(logs.contains("runner_id=7"), "{logs}");
        assert!(logs.contains("job_id=42"), "{logs}");
        assert!(logs.contains("413"), "{logs}");
        assert!(logs.contains("log exceeds the 5 MiB limit"), "{logs}");
        assert!(logs.contains("the job output is lost"), "{logs}");
    }

    #[tokio::test]
    async fn upload_log_retries_transient_failures_but_not_client_refusals() {
        let unavailable =
            spawn_fake_server("503 Service Unavailable", "temporarily unavailable").await;
        upload_log(
            &reqwest::Client::new(),
            &unavailable.url,
            7,
            42,
            "token",
            "complete log",
        )
        .await;
        assert_eq!(unavailable.requests(), super::LOG_UPLOAD_ATTEMPTS as usize);

        let refused = spawn_fake_server("413 Payload Too Large", "too large").await;
        upload_log(
            &reqwest::Client::new(),
            &refused.url,
            7,
            42,
            "token",
            "complete log",
        )
        .await;
        assert_eq!(refused.requests(), 1);
    }

    #[tokio::test]
    async fn start_job_reports_an_unconfirmed_start_and_stays_silent_on_success() {
        let failing = spawn_fake_server("500 Internal Server Error", "database restarting").await;
        let (logs, _guard) = capture_logs();
        assert_eq!(
            start_job(&reqwest::Client::new(), &failing.url, 3, 9, "token").await,
            JobStart::Unconfirmed
        );
        let rejected = logs.text();
        assert!(rejected.contains("runner_id=3"), "{rejected}");
        assert!(rejected.contains("job_id=9"), "{rejected}");
        assert!(rejected.contains("database restarting"), "{rejected}");
        assert!(rejected.contains("stays pending"), "{rejected}");

        let accepting = spawn_fake_server("200 OK", "").await;
        let (logs, _guard) = capture_logs();
        assert_eq!(
            start_job(&reqwest::Client::new(), &accepting.url, 3, 9, "token").await,
            JobStart::Started
        );
        assert_eq!(logs.text(), "");
    }

    /// card_a0377b61860e: the server's "this job is no longer yours" is an
    /// instruction not to run it, not a lost report to log and ignore.
    #[tokio::test]
    async fn start_job_is_refused_for_a_job_the_server_disowns() {
        for (status_line, body) in [
            ("409 Conflict", "job is no longer active"),
            ("404 Not Found", "job not found"),
            ("401 Unauthorized", "invalid runner token"),
        ] {
            let server = spawn_fake_server(status_line, body).await;
            assert_eq!(
                start_job(&reqwest::Client::new(), &server.url, 3, 9, "token").await,
                JobStart::Refused,
                "{status_line}"
            );
        }
    }

    #[tokio::test]
    async fn job_liveness_reads_the_answer_and_never_mistakes_an_outage_for_a_cancel() {
        let client = reqwest::Client::new();
        for (status_line, body, expected) in [
            (
                "200 OK",
                r#"{"job_id":9,"status":"running","active":true}"#,
                JobLiveness::Active,
            ),
            (
                "200 OK",
                r#"{"job_id":9,"status":"canceled","active":false}"#,
                JobLiveness::Disowned,
            ),
            ("404 Not Found", "job not found", JobLiveness::Disowned),
            (
                "409 Conflict",
                "job is no longer active",
                JobLiveness::Disowned,
            ),
            ("503 Service Unavailable", "busy", JobLiveness::Unknown),
            ("200 OK", "not json", JobLiveness::Unknown),
        ] {
            let server = spawn_fake_server(status_line, body).await;
            assert_eq!(
                job_liveness(&client, &server.url, 3, 9, "token").await,
                expected,
                "{status_line} {body}"
            );
        }
        let refusing = refusing_server_url().await;
        assert_eq!(
            job_liveness(&client, &refusing, 3, 9, "token").await,
            JobLiveness::Unknown
        );
    }

    /// A `409` from `finish` is the server having settled the job already — the
    /// runner's own cancel path ends here on purpose — and the same transaction
    /// put the runner back to `online`. It is neither retried nor reported as a
    /// job stuck `running`.
    #[tokio::test]
    async fn finish_job_takes_a_conflict_as_already_settled() {
        let server = spawn_fake_server("409 Conflict", "job already settled").await;
        let (logs, _guard) = capture_logs();

        finish_job(
            &reqwest::Client::new(),
            &server.url,
            4,
            77,
            "token",
            "failure",
            -1,
        )
        .await;

        assert_eq!(server.requests(), 1);
        assert_eq!(
            logs.text(),
            "",
            "a settled job was reported as a lost finish"
        );
    }

    #[tokio::test]
    async fn send_heartbeat_reports_a_transport_failure_with_the_runner_id() {
        let url = refusing_server_url().await;
        let (logs, _guard) = capture_logs();

        send_heartbeat(&reqwest::Client::new(), &url, 5, "token").await;

        let logs = logs.text();
        assert!(logs.contains("runner_id=5"), "{logs}");
        assert!(logs.contains("transport error"), "{logs}");
        assert!(logs.contains("mark this runner offline"), "{logs}");
    }

    #[tokio::test]
    async fn finish_job_retries_a_server_error_then_names_the_stuck_job() {
        let server = spawn_fake_server("500 Internal Server Error", "database is locked").await;
        let (logs, _guard) = capture_logs();

        finish_job(
            &reqwest::Client::new(),
            &server.url,
            4,
            77,
            "token",
            "success",
            0,
        )
        .await;

        assert_eq!(server.requests(), FINISH_JOB_ATTEMPTS as usize);
        let logs = logs.text();
        assert!(logs.contains("retrying in"), "{logs}");
        assert!(logs.contains("job_id=77"), "{logs}");
        assert!(logs.contains("database is locked"), "{logs}");
        assert!(
            logs.contains("stays 'running' on the server forever"),
            "{logs}"
        );
    }

    #[tokio::test]
    async fn finish_job_does_not_retry_a_client_error() {
        let server = spawn_fake_server("404 Not Found", "unknown runner").await;
        let (logs, _guard) = capture_logs();

        finish_job(
            &reqwest::Client::new(),
            &server.url,
            4,
            77,
            "token",
            "failure",
            1,
        )
        .await;

        assert_eq!(server.requests(), 1);
        let logs = logs.text();
        assert!(logs.contains("unknown runner"), "{logs}");
        assert!(!logs.contains("retrying in"), "{logs}");
    }
}

#[cfg(test)]
mod archive_upload_tests {
    use std::sync::{Arc, Mutex};

    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{save_cache, upload_cache_archive, UPLOAD_ARCHIVE_MAX_BYTES};

    /// One recorded request: the raw head, and the body the head declared.
    struct Recorded {
        head: String,
        body: Vec<u8>,
    }

    struct RecordingServer {
        url: String,
        seen: Arc<Mutex<Vec<Recorded>>>,
    }

    impl RecordingServer {
        fn requests(&self) -> std::sync::MutexGuard<'_, Vec<Recorded>> {
            self.seen.lock().unwrap()
        }
    }

    /// A socket that reads one whole request and answers `204`, keeping what it
    /// read. The workspace carries no HTTP-mock dependency, and this only needs
    /// to see the bytes the runner actually put on the wire.
    async fn spawn_recording_server() -> RecordingServer {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let recorded = Arc::clone(&recorded);
                tokio::spawn(async move {
                    let mut buf = Vec::new();
                    let mut chunk = [0_u8; 8192];
                    loop {
                        let Ok(read) = stream.read(&mut chunk).await else {
                            return;
                        };
                        if read == 0 {
                            break;
                        }
                        buf.extend_from_slice(&chunk[..read]);
                        let Some(head_end) = buf
                            .windows(4)
                            .position(|window| window == b"\r\n\r\n")
                            .map(|at| at + 4)
                        else {
                            continue;
                        };
                        let head = String::from_utf8_lossy(&buf[..head_end]).into_owned();
                        let declared = head
                            .to_ascii_lowercase()
                            .lines()
                            .find_map(|line| line.strip_prefix("content-length:"))
                            .and_then(|value| value.trim().parse::<usize>().ok())
                            .unwrap_or(0);
                        if buf.len() < head_end + declared {
                            continue;
                        }
                        recorded.lock().unwrap().push(Recorded {
                            head,
                            body: buf[head_end..head_end + declared].to_vec(),
                        });
                        break;
                    }
                    if stream
                        .write_all(
                            b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                        )
                        .await
                        .is_err()
                    {
                        // The client hung up before reading the reply — the
                        // request this server exists to record is already in.
                    }
                });
            }
        });
        RecordingServer { url, seen }
    }

    /// The archive travels as a length-declared stream off the disk.
    ///
    /// Two things are asserted together on purpose. The body must arrive whole —
    /// streaming a file is only worth anything if it is the same tar the buffer
    /// used to be — and it must carry a `Content-Length`, because a chunked body
    /// gives the route's transport ceiling nothing to refuse in advance: an
    /// over-ceiling upload would then have to travel in full before the server
    /// could say no.
    #[tokio::test]
    async fn a_saved_cache_is_streamed_with_its_length_declared() {
        let server = spawn_recording_server().await;
        let workspace = tempfile::tempdir().unwrap();
        let workspace = workspace.path().join("job-7");
        std::fs::create_dir_all(workspace.join("target")).unwrap();
        std::fs::write(workspace.join("target/dep.rlib"), b"cached-bytes").unwrap();

        save_cache(
            &reqwest::Client::new(),
            &server.url,
            4,
            7,
            "token",
            "deps-v1",
            &["target".to_string()],
            &workspace,
        )
        .await
        .expect("the cache upload must succeed");

        let requests = server.requests();
        let request = requests.first().expect("the cache archive was not sent");
        let head = request.head.to_ascii_lowercase();
        assert!(
            head.contains(&format!("content-length: {}", request.body.len())),
            "the archive must declare its length: {}",
            request.head
        );
        assert!(
            !head.contains("transfer-encoding"),
            "a length-declared body must not also be chunked: {}",
            request.head
        );
        assert!(head.contains("x-cache-key: deps-v1"), "{}", request.head);

        let mut names = Vec::new();
        let mut archive = tar::Archive::new(std::io::Cursor::new(&request.body));
        for entry in archive.entries().unwrap() {
            let entry = entry.unwrap();
            names.push(entry.path().unwrap().display().to_string());
        }
        assert!(
            names.iter().any(|name| name.contains("dep.rlib")),
            "the streamed archive lost its contents: {names:?}"
        );

        assert!(
            !crate::workspace::cache_archive_path(&workspace).exists(),
            "the packed cache archive must not stay on the runner's disk"
        );
    }

    /// An archive over the ceiling the route declares never reaches the wire,
    /// and the runner says why in a sentence the job log can carry.
    #[tokio::test]
    async fn an_over_ceiling_archive_is_refused_before_it_is_sent() {
        let server = spawn_recording_server().await;
        let archive = tempfile::NamedTempFile::new().unwrap();

        let error = upload_cache_archive(
            &reqwest::Client::new(),
            &server.url,
            4,
            7,
            "token",
            "deps-v1",
            archive.path(),
            UPLOAD_ARCHIVE_MAX_BYTES + 1,
        )
        .await
        .expect_err("an over-ceiling archive must not be uploaded");

        let rendered = format!("{error:#}");
        assert!(
            rendered.contains(&UPLOAD_ARCHIVE_MAX_BYTES.to_string()),
            "the refusal must name the ceiling: {rendered}"
        );
        assert!(
            rendered.contains("nothing was uploaded"),
            "the refusal must say the cache was left alone: {rendered}"
        );
        assert!(
            server.requests().is_empty(),
            "an archive the server would refuse was still put on the wire"
        );
    }
}

#[cfg(test)]
mod archive_download_tests {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    use super::{restore_cache, spool_response_body};

    /// Peak resident-set size of this process so far, in bytes.
    ///
    /// The *peak*, not the current one: a body that was collected and then
    /// dropped is back off the books by the time the call returns — a large
    /// allocation goes back to the kernel on free — so a snapshot taken
    /// afterwards cannot tell "buffered 256 MiB and released it" from "streamed
    /// 1 MiB at a time onto disk". `VmHWM` is the high-water mark, which is
    /// exactly the number a buffering regression moves. nextest runs every
    /// test in its own process, so the mark belongs to this test alone.
    fn peak_resident_bytes() -> Option<u64> {
        let status = std::fs::read_to_string("/proc/self/status").ok()?;
        let line = status
            .lines()
            .find(|line| line.starts_with("VmHWM:"))?
            .strip_prefix("VmHWM:")?;
        let kib: u64 = line.split_whitespace().next()?.parse().ok()?;
        Some(kib * 1024)
    }

    /// Drain one HTTP request head off the socket so the client always sees a
    /// reply, discarding the body (there is none on the download routes this
    /// suite fakes).
    async fn drain_request_head(stream: &mut tokio::net::TcpStream) -> std::io::Result<()> {
        let mut buf = Vec::new();
        let mut chunk = [0_u8; 4096];
        loop {
            let read = stream.read(&mut chunk).await?;
            if read == 0 {
                return Ok(());
            }
            buf.extend_from_slice(&chunk[..read]);
            if buf.windows(4).any(|window| window == b"\r\n\r\n") {
                return Ok(());
            }
        }
    }

    /// Answer every request with `total_bytes` bytes of `fill`, declared as
    /// `advertised_len` in `Content-Length` and pushed to the socket in
    /// `chunk_bytes` writes. Generates the body on the fly rather than holding
    /// it, so the fake server itself does not grow the process — the memory
    /// growth the VmHWM test measures then belongs to the code under test.
    async fn spawn_filler_download_server(
        total_bytes: usize,
        chunk_bytes: usize,
        advertised_len: usize,
        fill: u8,
    ) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                tokio::spawn(async move {
                    if drain_request_head(&mut stream).await.is_err() {
                        return;
                    }
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/x-tar\r\nContent-Length: \
                         {advertised_len}\r\nConnection: close\r\n\r\n"
                    );
                    if stream.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    let chunk = vec![fill; chunk_bytes];
                    let mut sent = 0;
                    while sent < total_bytes {
                        let want = (total_bytes - sent).min(chunk_bytes);
                        if stream.write_all(&chunk[..want]).await.is_err() {
                            return;
                        }
                        sent += want;
                    }
                    if stream.shutdown().await.is_err() {
                        // The client hung up before reading the reply — the
                        // response this server exists to serve is already out.
                    }
                });
            }
        });
        url
    }

    /// Answer every request with `payload` verbatim, declared as its own length
    /// in `Content-Length`. Sized for parity tests where a real tar has to
    /// arrive byte-identical on the other side.
    async fn spawn_payload_download_server(payload: Vec<u8>) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let payload = std::sync::Arc::new(payload);
        tokio::spawn(async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let payload = std::sync::Arc::clone(&payload);
                tokio::spawn(async move {
                    if drain_request_head(&mut stream).await.is_err() {
                        return;
                    }
                    let head = format!(
                        "HTTP/1.1 200 OK\r\nContent-Type: application/x-tar\r\nContent-Length: \
                         {}\r\nConnection: close\r\n\r\n",
                        payload.len()
                    );
                    if stream.write_all(head.as_bytes()).await.is_err() {
                        return;
                    }
                    if stream.write_all(&payload).await.is_err() {
                        return;
                    }
                    if stream.shutdown().await.is_err() {
                        // The client hung up before reading the reply — the
                        // response this server exists to serve is already out.
                    }
                });
            }
        });
        url
    }

    /// The measurement the fix exists for: spooling a cache-sized download must
    /// not grow the runner's peak RSS by the cache size. Same idea as the
    /// upload-side test and the `file_body_streams_a_large_file_without_...`
    /// test on the server side — the peak, because a `Vec` that was collected
    /// and then dropped is back off the books by the time the call returns.
    #[cfg_attr(
        not(target_os = "linux"),
        ignore = "reads VmHWM from /proc/self/status"
    )]
    #[tokio::test]
    async fn a_streamed_download_does_not_grow_the_process_by_its_own_size() {
        const CHUNK: usize = 1024 * 1024;
        const CHUNKS: usize = 256;
        const TOTAL: usize = CHUNK * CHUNKS;

        let url = spawn_filler_download_server(TOTAL, CHUNK, TOTAL, b'x').await;
        let response = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("connect to the fake download server");
        assert!(response.status().is_success());

        let spool_dir = tempfile::tempdir().unwrap();
        let spool = spool_dir.path().join("cache.download.tar");

        let before = peak_resident_bytes().expect("no /proc/self/status to measure against");
        let written = spool_response_body(response, &spool, "cache")
            .await
            .expect("spool must succeed");
        let after = peak_resident_bytes().expect("no /proc/self/status to measure against");

        assert_eq!(
            written as usize, TOTAL,
            "the spool must carry every byte the server sent"
        );
        let spooled = std::fs::metadata(&spool).unwrap().len();
        assert_eq!(spooled as usize, TOTAL, "the spool file must be complete");

        let grew = after.saturating_sub(before);
        let ceiling = (TOTAL / 4) as u64;
        assert!(
            grew < ceiling,
            "streaming a {TOTAL}-byte body grew VmHWM by {grew} bytes, over the {ceiling}-byte \
             ceiling; a buffering regression would grow the process by the full body size."
        );
    }

    /// A server that declares more bytes than it sends must be rejected, and
    /// the failure must reach the caller through the "download stream failed"
    /// channel — not a silent short read. A silent short read would look like
    /// a valid short tar to `unpack`, quietly omitting the entries that never
    /// arrived.
    ///
    /// reqwest itself refuses a short body against `Content-Length`, so the
    /// error surfaces from `bytes_stream()` and our own tail check
    /// (`declared != written`) is the second net: if reqwest ever stopped
    /// enforcing this the helper still would, on the same wording.
    #[tokio::test]
    async fn a_truncated_response_is_refused_rather_than_landing_as_a_short_body() {
        let url = spawn_filler_download_server(64, 32, 200, b'y').await;
        let response = reqwest::Client::new()
            .get(&url)
            .send()
            .await
            .expect("connect to the fake download server");
        assert!(response.status().is_success());
        let spool_dir = tempfile::tempdir().unwrap();
        let spool = spool_dir.path().join("cache.download.tar");

        let error = spool_response_body(response, &spool, "cache")
            .await
            .expect_err("a body shorter than its declared length must fail");
        let rendered = format!("{error:#}");
        assert!(
            rendered.contains("cache download"),
            "the refusal must name the download it came from: {rendered}"
        );
    }

    /// End-to-end: a real tar served with the correct `Content-Length` is
    /// spooled off the response, unpacked into the workspace, and every
    /// original file lands byte-identical. The parity check the buffering
    /// tests could take for granted — a streaming path that reorders or drops
    /// bytes is worse than a buffering one.
    #[tokio::test]
    async fn a_cache_download_unpacks_a_tar_streamed_off_the_wire() {
        let source_dir = tempfile::tempdir().unwrap();
        std::fs::write(source_dir.path().join("alpha.txt"), b"alpha-content").unwrap();
        std::fs::write(source_dir.path().join("beta.txt"), b"beta-content-2").unwrap();

        let mut buf = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut buf);
            builder
                .append_path_with_name(source_dir.path().join("alpha.txt"), "alpha.txt")
                .unwrap();
            builder
                .append_path_with_name(source_dir.path().join("beta.txt"), "beta.txt")
                .unwrap();
            builder.into_inner().unwrap();
        }

        let url = spawn_payload_download_server(buf).await;

        let workspace_root = tempfile::tempdir().unwrap();
        let workspace = workspace_root.path().join("job-42");
        std::fs::create_dir_all(&workspace).unwrap();

        let restored = restore_cache(
            &reqwest::Client::new(),
            &url,
            /* runner_id */ 3,
            /* job_id */ 42,
            "token",
            "deps-v1",
            &workspace,
        )
        .await
        .expect("cache restore must succeed");
        assert!(
            restored,
            "the server answered 200 so restore must report `true`"
        );

        let alpha = std::fs::read(workspace.join("alpha.txt")).unwrap();
        let beta = std::fs::read(workspace.join("beta.txt")).unwrap();
        assert_eq!(alpha, b"alpha-content");
        assert_eq!(beta, b"beta-content-2");

        // The spool lives beside the workspace and is cleaned up on every
        // exit path — a leaked archive here would double the disk footprint
        // of every successful cache restore.
        assert!(
            !crate::workspace::cache_download_spool_path(&workspace).exists(),
            "the download spool must not stay on the runner's disk"
        );
    }
}
