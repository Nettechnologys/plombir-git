//! Test-only fixtures shared by more than one service module.

use std::io::{BufRead, BufReader, Write};
use std::net::TcpListener;
use std::sync::{Arc, Mutex};

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
/// up, then 403 so `git` stops instead of retrying. Returns the bound address
/// and the list of credentials the remote actually received.
///
/// Both outbound-credential paths — mirror sync and repository import — prove
/// the same thing with it: that the secret we stored actually reaches the
/// remote, rather than being kept and then dropped on the floor.
pub(crate) fn spawn_authenticating_remote() -> (String, Arc<Mutex<Vec<String>>>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let address = listener.local_addr().expect("addr").to_string();
    let seen: Arc<Mutex<Vec<String>>> = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&seen);

    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { break };
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
                    "HTTP/1.1 401 Unauthorized\r\nWWW-Authenticate: Basic realm=\"forgekeep\"\
                     \r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                }
            };
            if stream.write_all(response.as_bytes()).is_ok() {
                drop(stream.flush());
            }
        }
    });

    (address, seen)
}
