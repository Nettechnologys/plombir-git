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
