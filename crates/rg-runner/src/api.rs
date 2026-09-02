//! HTTP client calls against the ForgeKeep server's runner API
//! (registration, job polling, heartbeats, workspace/cache/artifact transfer,
//! status).

use std::path::PathBuf;

use anyhow::{Context, Result};

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

/// Attempts for [`finish_job`] — the one report whose loss corrupts server state
/// irreversibly (nothing else ever moves that job out of `running`).
const FINISH_JOB_ATTEMPTS: u32 = 3;

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

/// Notify the server that job execution has started.
pub async fn start_job(
    client: &reqwest::Client,
    server: &str,
    runner_id: i64,
    job_id: i64,
    token: &str,
) {
    let request = client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/start",
            server, runner_id, job_id
        ))
        .header("Authorization", format!("Bearer {}", token));
    send_report(request, START_JOB_REPORT, runner_id, Some(job_id)).await;
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
        "[forgekeep-runner] log truncated: {cut} of {} bytes dropped, the tail follows\n",
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
    let request = client
        .post(format!(
            "{}/api/v1/runners/{}/jobs/{}/log",
            server, runner_id, job_id
        ))
        .header("Authorization", format!("Bearer {}", token))
        .body(trim_log_for_upload(log).into_owned());
    send_report(request, UPLOAD_LOG_REPORT, runner_id, Some(job_id)).await;
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
    let archive = response.bytes().await?;
    let workspace = std::env::temp_dir()
        .join("forgekeep-runner")
        .join("jobs")
        .join(job_id.to_string());
    let unpack_path = workspace.clone();
    tokio::task::spawn_blocking(move || unpack_workspace(&archive, &unpack_path)).await??;
    Ok(workspace)
}

/// Replace `unpack_path` with the contents of a workspace tar archive.
///
/// Every message names the path: the workspace lives under `TMPDIR`, so in a
/// container the failure is usually "that directory is read-only / owned by
/// another uid", and a bare `Permission denied (os error 13)` doesn't tell the
/// operator which directory to fix — or that `TMPDIR` is the knob.
fn unpack_workspace(archive: &[u8], unpack_path: &std::path::Path) -> Result<()> {
    if unpack_path.exists() {
        std::fs::remove_dir_all(unpack_path).with_context(|| {
            format!(
                "failed to remove stale runner workspace `{}`",
                unpack_path.display()
            )
        })?;
    }
    std::fs::create_dir_all(unpack_path).with_context(|| {
        format!(
            "failed to create runner workspace `{}` (set TMPDIR to a writable directory)",
            unpack_path.display()
        )
    })?;
    tar::Archive::new(std::io::Cursor::new(archive))
        .unpack(unpack_path)
        .with_context(|| {
            format!(
                "failed to unpack runner workspace into `{}`",
                unpack_path.display()
            )
        })?;
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
    let archive = response.bytes().await?;
    let workspace = workspace.to_path_buf();
    tokio::task::spawn_blocking(move || {
        tar::Archive::new(std::io::Cursor::new(archive))
            .unpack(workspace)
            .context("unpack job cache")
    })
    .await??;
    Ok(true)
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
    let paths = paths.to_vec();
    let workspace = workspace.to_path_buf();
    let archive = tokio::task::spawn_blocking(move || -> Result<Vec<u8>> {
        let mut bytes = Vec::new();
        {
            let mut builder = tar::Builder::new(&mut bytes);
            for path in paths {
                let source = workspace.join(&path);
                if source.is_dir() {
                    builder.append_dir_all(&path, source)?;
                } else if source.is_file() {
                    builder.append_path_with_name(source, &path)?;
                }
            }
            builder.finish()?;
        }
        Ok(bytes)
    })
    .await??;
    if archive.is_empty() {
        return Ok(());
    }
    let response = client
        .put(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/cache"
        ))
        .bearer_auth(token)
        .header("x-cache-key", key)
        .body(archive)
        .send()
        .await?;
    if !response.status().is_success() {
        anyhow::bail!(
            "cache upload failed: {}",
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
    let bytes = tokio::fs::read(archive).await.with_context(|| {
        format!(
            "failed to read the packed artifact archive `{}`",
            archive.display()
        )
    })?;
    if bytes.is_empty() {
        anyhow::bail!("packed artifact archive is empty");
    }
    let response = client
        .put(format!(
            "{server}/api/v1/runners/{runner_id}/jobs/{job_id}/artifacts/staging"
        ))
        .bearer_auth(token)
        .header(reqwest::header::CONTENT_TYPE, "application/x-tar")
        .body(bytes)
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
        let Some(failure) = describe_send_failure(request).await else {
            return;
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
        body_excerpt, error_chain, finish_job, send_heartbeat, start_job, trim_log_for_upload,
        unpack_workspace, upload_log, FINISH_JOB_ATTEMPTS, LOG_UPLOAD_MAX_BYTES, MAX_LOGGED_BODY,
    };

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
            trimmed.starts_with("[forgekeep-runner] log truncated:"),
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

        let error = unpack_workspace(b"", &unpack_path)
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

        let error = unpack_workspace(b"this is not a tar archive", &unpack_path)
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
    async fn start_job_reports_a_rejected_start_and_stays_silent_on_success() {
        let rejecting = spawn_fake_server("401 Unauthorized", "runner token expired").await;
        let (logs, _guard) = capture_logs();
        start_job(&reqwest::Client::new(), &rejecting.url, 3, 9, "token").await;
        let rejected = logs.text();
        assert!(rejected.contains("runner_id=3"), "{rejected}");
        assert!(rejected.contains("job_id=9"), "{rejected}");
        assert!(rejected.contains("runner token expired"), "{rejected}");
        assert!(rejected.contains("stays pending"), "{rejected}");

        let accepting = spawn_fake_server("200 OK", "").await;
        let (logs, _guard) = capture_logs();
        start_job(&reqwest::Client::new(), &accepting.url, 3, 9, "token").await;
        assert_eq!(logs.text(), "");
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
