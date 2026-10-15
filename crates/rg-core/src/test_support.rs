//! Test-only fixtures shared by more than one service module.

use std::fmt::Debug;
use std::future::Future;
use std::io::{BufRead, BufReader, Write};
use std::net::{IpAddr, Ipv4Addr, TcpListener};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};
use std::time::{Duration, Instant};

use tokio::task::JoinHandle;

use crate::db_retry::{classify_anyhow, Retry};

/// Sink that keeps formatted warning lines so best-effort paths can prove that
/// an operator sees the failure they deliberately do not return to the caller.
#[derive(Clone, Default)]
pub(crate) struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

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

impl CapturedLogs {
    pub(crate) fn capture() -> (Self, tracing::subscriber::DefaultGuard) {
        let logs = Self::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let guard = tracing::subscriber::set_default(subscriber);
        (logs, guard)
    }

    pub(crate) fn rendered(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().expect("log lock")).into_owned()
    }
}

/// A migrated, single-connection SQLite database for tests that break exactly
/// one table. One connection keeps PRAGMA changes and in-memory state aligned.
pub(crate) async fn migrated_memory_database() -> sea_orm::DatabaseConnection {
    let db = rg_db::connect_with_pool("sqlite::memory:", rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 1)
        .await
        .expect("connect test database");
    rg_db::run_migrations(&db)
        .await
        .expect("run test migrations");
    db
}

/// A remote that speaks HTTP Basic: 401 until an `Authorization` header shows
/// up, then 403 so `git` stops instead of retrying. Returns the bound address,
/// number of requests, and the credentials the remote actually received.
///
/// Both outbound-credential paths — mirror sync and repository import — prove
/// the same thing with it: that the secret we stored actually reaches the
/// remote, rather than being kept and then dropped on the floor.
pub(crate) fn spawn_authenticating_remote() -> (String, Arc<AtomicUsize>, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let requests = Arc::new(AtomicUsize::new(0));
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let request_counter = Arc::clone(&requests);
    let recorder = Arc::clone(&seen);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            request_counter.fetch_add(1, Ordering::SeqCst);
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut authorization = None;
            loop {
                let mut line = String::new();
                match reader.read_line(&mut line) {
                    Ok(0) => break,
                    Ok(_) => {}
                    Err(_) => break,
                }
                if line == "\r\n" || line == "\n" {
                    break;
                }
                if let Some(value) = line
                    .strip_prefix("Authorization: ")
                    .or_else(|| line.strip_prefix("authorization: "))
                {
                    authorization = Some(value.trim().to_string());
                }
            }
            let response = match authorization {
                Some(value) => {
                    recorder.lock().expect("lock").push(value);
                    "HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                }
                None => {
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"plombir-git\"\
                     \r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                }
            };
            if stream.write_all(response.as_bytes()).is_ok() {
                drop(stream.flush());
            }
        }
    });

    (address, requests, seen)
}

/// Two HTTP git sinks on one port: the address admitted by a scripted DNS
/// answer and the address the system resolver returns for `localhost`.
///
/// Loopback aliases stand in for a public first answer and a forbidden rebound
/// answer so the test stays live and deterministic without contacting DNS or a
/// metadata endpoint. A correctly bound git invocation reaches `127.0.0.2`;
/// deleting the `http.curloptResolve` hand-off makes it leave the checked answer
/// and resolve `localhost` independently.
pub(crate) struct RebindingGitRemotes {
    pub(crate) url: String,
    pub(crate) checked_ip: IpAddr,
    pub(crate) checked_requests: Arc<AtomicUsize>,
    pub(crate) rebound_requests: Arc<AtomicUsize>,
}

pub(crate) fn spawn_rebinding_git_remotes() -> RebindingGitRemotes {
    let rebound_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind rebound sink");
    let port = rebound_listener
        .local_addr()
        .expect("rebound address")
        .port();
    let checked_ip = Ipv4Addr::new(127, 0, 0, 2);
    let checked_listener =
        TcpListener::bind((checked_ip, port)).expect("bind checked-answer git sink");
    let checked_requests = Arc::new(AtomicUsize::new(0));
    let rebound_requests = Arc::new(AtomicUsize::new(0));

    spawn_refusing_git_remote(checked_listener, Arc::clone(&checked_requests));
    spawn_refusing_git_remote(rebound_listener, Arc::clone(&rebound_requests));

    RebindingGitRemotes {
        url: format!("http://localhost:{port}/upstream.git"),
        checked_ip: checked_ip.into(),
        checked_requests,
        rebound_requests,
    }
}

/// A checked git endpoint that redirects to a host/port outside its resolve
/// rule, plus the sink that would receive that redirected request.
pub(crate) fn spawn_redirecting_git_remotes() -> RebindingGitRemotes {
    let rebound_listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).expect("bind redirect sink");
    let rebound_port = rebound_listener
        .local_addr()
        .expect("redirect sink address")
        .port();
    let checked_ip = Ipv4Addr::new(127, 0, 0, 2);
    let checked_listener =
        TcpListener::bind((checked_ip, 0)).expect("bind checked redirecting git sink");
    let checked_port = checked_listener
        .local_addr()
        .expect("checked redirect address")
        .port();
    let checked_requests = Arc::new(AtomicUsize::new(0));
    let rebound_requests = Arc::new(AtomicUsize::new(0));
    let checked_counter = Arc::clone(&checked_requests);

    std::thread::spawn(move || {
        for stream in checked_listener.incoming() {
            let Ok(mut stream) = stream else { break };
            checked_counter.fetch_add(1, Ordering::SeqCst);
            let response = format!(
                "HTTP/1.1 302 Found\r\nLocation: http://localhost:{rebound_port}/metadata\r\n\
                 Content-Length: 0\r\nConnection: close\r\n\r\n"
            );
            if stream.write_all(response.as_bytes()).is_ok() {
                drop(stream.flush());
            }
        }
    });
    spawn_refusing_git_remote(rebound_listener, Arc::clone(&rebound_requests));

    RebindingGitRemotes {
        url: format!("http://localhost:{checked_port}/upstream.git"),
        checked_ip: checked_ip.into(),
        checked_requests,
        rebound_requests,
    }
}

fn spawn_refusing_git_remote(listener: TcpListener, requests: Arc<AtomicUsize>) {
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            requests.fetch_add(1, Ordering::SeqCst);
            if stream
                .write_all(
                    b"HTTP/1.1 403 Forbidden\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .is_ok()
            {
                drop(stream.flush());
            }
        }
    });
}

// ── A task parked behind a boundary ──────────────────────────────────────

/// How long a test watches a spawned task before concluding it is still parked
/// behind the boundary under test.
pub(crate) const HELD_TASK_WINDOW: Duration = Duration::from_millis(200);

/// How long a parked writer keeps re-running its whole operation before it
/// reports contention as a failure.
///
/// Far past anything [`HELD_TASK_WINDOW`] plus the surrounding queries can
/// reach, so a loaded machine's scheduling can no longer masquerade as a
/// boundary that let a writer through.
const PARKED_WRITER_DEADLINE: Duration = Duration::from_secs(120);

/// Run one write until the backend stops refusing it for contention.
///
/// The twin of [`rg_db::contention::retry_transaction`] for a *test* writer
/// that a sibling test deliberately parks behind a held lock. Being refused
/// there says nothing about the boundary under test: SQLite refuses a blocked
/// writer either after its `busy_timeout` or — when waiting could deadlock —
/// immediately, and which one a given attempt meets is decided by the machine
/// rather than by the code under test. A production writer already survives
/// both by re-running its transaction; a test writer that does not is testing
/// how busy the machine was.
///
/// `attempt` must be a *whole* operation, because that is the unit being
/// re-run. The deadline is the point of the helper: no observation window can
/// reach it, so an exhausted budget can no longer be read as "the writer got
/// through".
pub(crate) async fn write_while_the_lock_is_held<F, Fut>(mut attempt: F) -> anyhow::Result<()>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = anyhow::Result<()>>,
{
    let deadline = Instant::now() + PARKED_WRITER_DEADLINE;
    let mut refusals = 0usize;
    loop {
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(error)
                if classify_anyhow(&error) != Retry::AfterWaiting || Instant::now() >= deadline =>
            {
                return Err(error)
            }
            Err(_) => {
                refusals += 1;
                tokio::time::sleep(rg_db::contention::contention_backoff(refusals)).await;
            }
        }
    }
}

/// Assert a spawned task is still parked behind the boundary under test — and
/// say what it actually did when it is not.
///
/// `assert!(timeout(window, &mut task).await.is_err())` reads `Err` as "still
/// parked". A task that returned an error, and a task that panicked, both
/// resolve the future *immediately* — so `is_err()` is false and the assertion
/// reports the opposite of the truth: "it crossed the boundary" when in fact it
/// never got in, with the real failure left in the task's own output. Keep the
/// outcome in the message instead.
///
/// `crossed` names the boundary the task would have crossed, so the contract
/// still reads as the test's subject.
pub(crate) async fn assert_task_stays_blocked<T>(task: &mut JoinHandle<T>, crossed: &str)
where
    T: Debug,
{
    match tokio::time::timeout(HELD_TASK_WINDOW, task).await {
        Err(_still_parked) => {}
        Ok(Ok(outcome)) => {
            panic!("{crossed} — the task finished with {outcome:?} instead of waiting")
        }
        Ok(Err(panic)) => {
            panic!("{crossed} — the task panicked instead of waiting: {panic}")
        }
    }
}

/// A git remote that accepts pushes: `git http-backend` behind a minimal
/// HTTP/1.1 front, serving every bare repository under `root`.
///
/// HTTP Basic is required — 401 until an `Authorization` header shows up — and
/// every header received is recorded, so a push mirror test can prove both
/// that the push landed and which credential carried it. The remote's user is
/// the login half of that header, which is what lets `http-backend` accept
/// `git-receive-pack` at all.
///
/// It is an LFS server too, the way a forge is: `<repo>.git/info/lfs/objects/batch`
/// answers `upload` with an `upload` and a `verify` action for each object it
/// does not hold and with no action for one it does, and keeps what is
/// uploaded in [`FakeLfs`].
pub(crate) struct GitHttpBackend {
    pub(crate) url: String,
    pub(crate) authorizations: Arc<Mutex<Vec<String>>>,
    pub(crate) lfs: Arc<Mutex<FakeLfs>>,
}

/// The LFS half of a [`GitHttpBackend`].
#[derive(Default)]
pub(crate) struct FakeLfs {
    /// Objects the remote holds, by oid.
    pub(crate) objects: std::collections::HashMap<String, Vec<u8>>,
    /// The oids each `upload` batch asked about, in order.
    pub(crate) upload_batches: Vec<Vec<String>>,
    /// Every `PUT` of an object, by oid.
    pub(crate) uploads: Vec<String>,
    /// Every `verify` of an object, by oid.
    pub(crate) verified: Vec<String>,
}

/// Answer an LFS request of a [`GitHttpBackend`], or `None` for a git one.
fn fake_lfs_response(
    lfs: &Mutex<FakeLfs>,
    address: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: &[u8],
) -> Option<String> {
    let respond = |status: &str, content_type: &str, body: String| {
        format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\n\
             Connection: close\r\n\r\n{body}",
            body.len()
        )
    };
    let mut lfs = lfs.lock().expect("lfs");
    if path.ends_with("/info/lfs/objects/batch") {
        let request: serde_json::Value = serde_json::from_slice(body).expect("batch body");
        assert_eq!(request["operation"], "upload", "the fake serves uploads");
        let objects = request["objects"].as_array().expect("batch objects");
        let oids = objects
            .iter()
            .map(|object| object["oid"].as_str().expect("oid").to_string())
            .collect::<Vec<_>>();
        lfs.upload_batches.push(oids);
        let answer = objects
            .iter()
            .map(|object| {
                let oid = object["oid"].as_str().expect("oid");
                if lfs.objects.contains_key(oid) {
                    serde_json::json!({"oid": oid, "size": object["size"]})
                } else {
                    serde_json::json!({
                        "oid": oid,
                        "size": object["size"],
                        "actions": {
                            "upload": {"href": format!("http://{address}/lfs-objects/{oid}")},
                            "verify": {"href": format!("http://{address}/lfs-verify")},
                        },
                    })
                }
            })
            .collect::<Vec<_>>();
        let answer = serde_json::json!({"transfer": "basic", "objects": answer}).to_string();
        return Some(respond("200 OK", "application/vnd.git-lfs+json", answer));
    }
    if let Some(oid) = path.strip_prefix("/lfs-objects/") {
        assert_eq!(method, "PUT");
        lfs.uploads.push(oid.to_string());
        lfs.objects.insert(oid.to_string(), body.to_vec());
        return Some(respond("200 OK", "text/plain", String::new()));
    }
    if path == "/lfs-verify" {
        let request: serde_json::Value = serde_json::from_slice(body).expect("verify body");
        let oid = request["oid"].as_str().expect("oid").to_string();
        let held = lfs
            .objects
            .get(&oid)
            .is_some_and(|bytes| Some(bytes.len() as u64) == request["size"].as_u64());
        lfs.verified.push(oid);
        return Some(if held {
            respond("200 OK", "application/vnd.git-lfs+json", "{}".to_string())
        } else {
            respond(
                "404 Not Found",
                "application/vnd.git-lfs+json",
                "{}".to_string(),
            )
        });
    }
    None
}

pub(crate) fn spawn_git_http_backend(root: &std::path::Path) -> GitHttpBackend {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use std::io::Read;

    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr");
    let authorizations: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&authorizations);
    let lfs: Arc<Mutex<FakeLfs>> = Arc::default();
    let lfs_state = Arc::clone(&lfs);
    let root = root.to_path_buf();

    std::thread::spawn(move || {
        // The gateway's streamed spawn is the one that takes stdin, and it is
        // async; one small runtime serves every request this remote answers.
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("backend runtime");
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
            let mut reader = BufReader::new(stream.try_clone().expect("clone"));
            let mut request_line = String::new();
            if reader.read_line(&mut request_line).unwrap_or(0) == 0 {
                continue;
            }
            let mut parts = request_line.split_whitespace();
            let method = parts.next().unwrap_or_default().to_string();
            let target = parts.next().unwrap_or_default().to_string();
            let mut headers = Vec::new();
            loop {
                let mut line = String::new();
                if reader.read_line(&mut line).unwrap_or(0) == 0 {
                    break;
                }
                let line = line.trim_end().to_string();
                if line.is_empty() {
                    break;
                }
                if let Some((name, value)) = line.split_once(':') {
                    headers.push((name.trim().to_ascii_lowercase(), value.trim().to_string()));
                }
            }
            let header = |name: &str| {
                headers
                    .iter()
                    .find(|(key, _)| key == name)
                    .map(|(_, value)| value.clone())
            };

            let mut body = Vec::new();
            if let Some(length) = header("content-length").and_then(|v| v.parse::<usize>().ok()) {
                body.resize(length, 0);
                if reader.read_exact(&mut body).is_err() {
                    continue;
                }
            } else if header("transfer-encoding").is_some_and(|v| v.contains("chunked")) {
                loop {
                    let mut size = String::new();
                    if reader.read_line(&mut size).unwrap_or(0) == 0 {
                        break;
                    }
                    let size = usize::from_str_radix(size.trim(), 16).unwrap_or(0);
                    let mut chunk = vec![0; size + 2];
                    if reader.read_exact(&mut chunk).is_err() {
                        break;
                    }
                    if size == 0 {
                        break;
                    }
                    body.extend_from_slice(&chunk[..size]);
                }
            }

            let Some(authorization) = header("authorization") else {
                let response = "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic \
                                realm=\"plombir-git\"\r\nContent-Length: 0\r\nConnection: \
                                close\r\n\r\n";
                drop(stream.write_all(response.as_bytes()));
                continue;
            };
            recorder.lock().expect("lock").push(authorization.clone());
            let user = authorization
                .strip_prefix("Basic ")
                .and_then(|encoded| STANDARD.decode(encoded).ok())
                .and_then(|decoded| String::from_utf8(decoded).ok())
                .and_then(|pair| pair.split_once(':').map(|(user, _)| user.to_string()))
                .unwrap_or_default();

            let (path, query) = target.split_once('?').unwrap_or((target.as_str(), ""));
            if let Some(response) = fake_lfs_response(&lfs_state, address, &method, path, &body) {
                if stream.write_all(response.as_bytes()).is_ok() {
                    drop(stream.flush());
                }
                continue;
            }
            let content_type = header("content-type").unwrap_or_default();
            let content_length = body.len().to_string();
            let project_root = root.to_string_lossy().into_owned();
            let env = [
                ("GIT_PROJECT_ROOT", project_root.as_str()),
                ("GIT_HTTP_EXPORT_ALL", "1"),
                ("REQUEST_METHOD", method.as_str()),
                ("PATH_INFO", path),
                ("QUERY_STRING", query),
                ("CONTENT_TYPE", content_type.as_str()),
                ("CONTENT_LENGTH", content_length.as_str()),
                ("REMOTE_USER", user.as_str()),
                ("REMOTE_ADDR", "127.0.0.1"),
            ];
            let output = runtime.block_on(async {
                use tokio::io::AsyncWriteExt;
                let mut child = rg_git::cli_gateway::global_gateway()
                    .as_ref()
                    .expect("git gateway")
                    .spawn_async_with_env(&["http-backend"], None, &env)
                    .await
                    .expect("spawn git http-backend");
                let mut stdin = child.stdin.take().expect("stdin");
                let writer = tokio::spawn(async move {
                    drop(stdin.write_all(&body).await);
                });
                let output = child.wait_with_output().await.expect("http-backend output");
                drop(writer.await);
                output
            });

            let split = output
                .stdout
                .windows(4)
                .position(|window| window == b"\r\n\r\n")
                .map(|at| (at, 4))
                .or_else(|| {
                    output
                        .stdout
                        .windows(2)
                        .position(|window| window == b"\n\n")
                        .map(|at| (at, 2))
                });
            let (cgi_headers, cgi_body) = match split {
                Some((at, skip)) => (
                    String::from_utf8_lossy(&output.stdout[..at]).into_owned(),
                    output.stdout[at + skip..].to_vec(),
                ),
                None => (String::new(), output.stdout.clone()),
            };
            let mut status = "200 OK".to_string();
            let mut response_headers = String::new();
            for line in cgi_headers.lines() {
                let line = line.trim_end();
                match line.split_once(':') {
                    Some((name, value)) if name.eq_ignore_ascii_case("status") => {
                        status = value.trim().to_string();
                    }
                    Some((name, _)) if name.eq_ignore_ascii_case("content-length") => {}
                    Some(_) => {
                        response_headers.push_str(line);
                        response_headers.push_str("\r\n");
                    }
                    None => {}
                }
            }
            let head = format!(
                "HTTP/1.1 {status}\r\n{response_headers}Content-Length: {}\r\nConnection: \
                 close\r\n\r\n",
                cgi_body.len()
            );
            if stream.write_all(head.as_bytes()).is_ok() && stream.write_all(&cgi_body).is_ok() {
                drop(stream.flush());
            }
        }
    });

    GitHttpBackend {
        url: format!("http://{address}"),
        authorizations,
        lfs,
    }
}

#[cfg(test)]
mod parked_task_tests {
    use super::*;

    /// The window is what "still parked" means, so a task that outlives it must
    /// be read as parked and nothing else.
    #[tokio::test]
    async fn a_task_that_outlives_the_window_is_read_as_parked() {
        let mut parked = tokio::spawn(async {
            tokio::time::sleep(Duration::from_secs(30)).await;
        });
        assert_task_stays_blocked(&mut parked, "the parked task crossed the boundary").await;
        parked.abort();
    }

    /// The inversion this helper exists for: a task that was *refused* resolves
    /// immediately, and the old `.is_err()` spelling reported that as the task
    /// having crossed the boundary — the opposite of the truth, with the real
    /// failure left in the task's own output.
    #[tokio::test]
    #[should_panic(expected = "the task finished with Err(\"refused\") instead of waiting")]
    async fn a_refused_task_is_not_reported_as_having_crossed() {
        let mut refused = tokio::spawn(async { Err::<(), &str>("refused") });
        assert_task_stays_blocked(&mut refused, "the writer crossed the boundary").await;
    }

    /// A panic drops whatever the task was going to signal through, so it also
    /// resolves the handle immediately. Same inversion, same requirement.
    #[tokio::test]
    #[should_panic(expected = "the task panicked instead of waiting")]
    async fn a_panicking_task_is_not_reported_as_having_crossed() {
        let mut panicking = tokio::spawn(async { panic!("the writer never got in") });
        assert_task_stays_blocked(&mut panicking, "the writer crossed the boundary").await;
    }

    /// Which failures are contention is `rg_db`'s predicate and is tested there
    /// against real backend error codes — a hand-built `sqlx` error here would
    /// only test the fixture. What this helper owes is the other half: anything
    /// that is *not* contention must escape on the first attempt, or a test
    /// writer spins for two minutes on a failure re-running cannot fix.
    #[tokio::test]
    async fn a_failure_that_is_not_contention_escapes_un_retried() {
        let attempts = std::cell::Cell::new(0usize);
        let error = write_while_the_lock_is_held(|| {
            attempts.set(attempts.get() + 1);
            async { Err(anyhow::anyhow!("the row does not exist")) }
        })
        .await
        .expect_err("a failure that is not contention must escape");
        assert_eq!(attempts.get(), 1, "a real failure must not be re-run");
        assert!(format!("{error:#}").contains("the row does not exist"));
    }
}
