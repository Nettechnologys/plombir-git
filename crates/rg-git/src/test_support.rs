//! Fixtures shared by unit tests of several protocol modules.

use std::sync::{Arc, Mutex};

/// Sink that keeps formatted log lines, so a path that deliberately does not
/// surface a failure can prove that an operator still sees it.
///
/// Three modules need exactly this: every one of them answers the client with
/// a fixed text and puts the real cause in a `tracing` event, and the half of
/// that contract a test can only check from the log side is the same in all
/// three.
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
    /// Capture `WARN` and above for as long as the returned guard lives.
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

/// A bare repository whose `main`, a tag, and one ref in each of the server's
/// private namespaces all point at one stored object — the shape every
/// advertisement test needs to tell "shown" from "hidden".
pub(crate) fn repository_with_server_private_refs(dir: &std::path::Path) -> std::path::PathBuf {
    let repo_path = dir.join("private-refs.git");
    // Through the server's own two points — the git gateway and
    // `repository::open` — so the fixture is not a repository opened on the
    // host's terms (`repository_open_ownership_guard` reads this file as
    // production: the `cfg(test)` sits on its `mod` line in `lib.rs`).
    crate::cli_gateway::global_gateway()
        .as_ref()
        .expect("git gateway")
        .run(
            &["init", "-q", "--bare", &repo_path.to_string_lossy()],
            None,
        )
        .expect("run git init")
        .ensure_success()
        .expect("git init --bare");
    let repo = crate::repository::open(&repo_path).expect("open bare repository");
    let id = repo.write_blob(b"advertised").expect("write blob").detach();
    for refname in [
        "refs/heads/main",
        "refs/tags/v1",
        "refs/forks/alice/feature",
        "refs/merge-queue/7",
    ] {
        let path = repo_path.join(refname);
        std::fs::create_dir_all(path.parent().expect("ref directory")).expect("ref directory");
        std::fs::write(path, format!("{id}\n")).expect("write ref");
    }
    std::fs::write(repo_path.join("HEAD"), "ref: refs/heads/main\n").expect("write HEAD");
    repo_path
}

/// Every server-private ref name `text` mentions.
pub(crate) fn server_private_refs_in(text: &str) -> Vec<&'static str> {
    ["refs/forks/", "refs/merge-queue/"]
        .into_iter()
        .filter(|namespace| text.contains(namespace))
        .collect()
}
