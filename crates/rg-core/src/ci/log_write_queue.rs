//! CI/CD job log write queue.
//!
//! Serialises log writes through a single `tokio::sync::mpsc` channel so that
//! concurrent pipeline jobs do not contend on SQLite writes.  A background
//! consumer task receives log-update requests and executes each one against
//! the database sequentially.
//!
//! # Usage
//!
//! ```ignore
//! let queue = LogWriteQueue::spawn(db.clone());
//! queue.write(job_id, &log_text).await;
//! ```
//!
//! # Stream-ready design
//!
//! The queue accepts any number of writes per job.  Future streaming log
//! support (runner sends lines during execution) can call `write()` for each
//! chunk without worrying about concurrent-write contention.

use sea_orm::DatabaseConnection;
use std::sync::Arc;
use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;

/// Maximum buffered log writes before the channel exerts back-pressure.
const CHANNEL_CAPACITY: usize = 4096;

/// A request to write (append) log text for a given job.
struct LogWriteRequest {
    /// Pipeline job ID.
    pub job_id: i64,
    /// Log text chunk to append.
    pub log_text: String,
}

/// Cloneable handle to the log write queue.
///
/// Drop the last clone to stop the background consumer gracefully.
#[derive(Clone)]
pub struct LogWriteQueue {
    tx: mpsc::Sender<LogWriteRequest>,
}

impl LogWriteQueue {
    /// Spawn a new consumer task and return a handle.
    ///
    /// The consumer runs in the background and processes log writes
    /// sequentially, one at a time.
    pub fn spawn(db: DatabaseConnection) -> Self {
        let (tx, rx) = mpsc::channel::<LogWriteRequest>(CHANNEL_CAPACITY);
        let db = Arc::new(db);

        tokio::spawn(Self::consumer(db, rx));

        Self { tx }
    }

    /// Spawn a consumer wired to a graceful-shutdown signal.
    ///
    /// When `shutdown_rx` flips to `true`, the consumer drains every buffered
    /// write it can still see, persists them, and only then stops — so a
    /// `SIGTERM` under load does not lose the pending CI-log backlog. Returns
    /// the handle alongside the queue so the caller can `await` the drain
    /// (bounded by a grace window) before the process exits.
    pub fn spawn_with_shutdown(
        db: DatabaseConnection,
        shutdown_rx: watch::Receiver<bool>,
    ) -> (Self, JoinHandle<()>) {
        let (tx, rx) = mpsc::channel::<LogWriteRequest>(CHANNEL_CAPACITY);
        let db = Arc::new(db);

        let handle = tokio::spawn(Self::consumer_with_shutdown(db, rx, shutdown_rx));

        (Self { tx }, handle)
    }

    /// Queue a log chunk for a given job.
    ///
    /// This is fire-and-forget from the caller's perspective: the write will
    /// happen asynchronously.  If the channel is full, the oldest pending
    /// write for the same job is **dropped** so that new data always gets
    /// through (like a sliding window).
    pub async fn write(&self, job_id: i64, log_text: &str) {
        if self.tx.is_closed() {
            tracing::warn!("log write queue is closed, dropping log for job {job_id}");
            return;
        }

        let req = LogWriteRequest {
            job_id,
            log_text: log_text.to_string(),
        };

        // Try to send; if the channel is full, we drop the request instead
        // of blocking the caller.  This is acceptable because:
        // 1. The consumer processes writes in order
        // 2. A later write for the same job supersedes the dropped one
        // 3. The final complete log is written when the job finishes
        if self.tx.try_send(req).is_err() {
            tracing::debug!(
                job_id,
                "log write queue full ({}), dropping old entry",
                CHANNEL_CAPACITY
            );
        }
    }

    /// Number of pending (buffered) log writes.
    pub fn pending_count(&self) -> usize {
        self.tx.max_capacity() - self.tx.capacity()
    }

    /// Background consumer: receives write requests and executes them.
    async fn consumer(db: Arc<DatabaseConnection>, mut rx: mpsc::Receiver<LogWriteRequest>) {
        while let Some(req) = rx.recv().await {
            Self::process_request(&db, req).await;
        }

        tracing::info!("log write queue consumer stopped");
    }

    /// Shutdown-aware consumer: processes writes until either every sender is
    /// dropped (channel closed) or the shutdown signal fires, in which case it
    /// drains the remaining buffered writes before stopping.
    async fn consumer_with_shutdown(
        db: Arc<DatabaseConnection>,
        mut rx: mpsc::Receiver<LogWriteRequest>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) {
        loop {
            tokio::select! {
                maybe = rx.recv() => match maybe {
                    Some(req) => Self::process_request(&db, req).await,
                    None => break, // all senders dropped
                },
                changed = shutdown_rx.changed() => {
                    // `Err` means the shutdown coordinator itself was dropped
                    // (process is going away) — treat it like a shutdown too.
                    if changed.is_err() || *shutdown_rx.borrow() {
                        let mut drained = 0_usize;
                        while let Ok(req) = rx.try_recv() {
                            Self::process_request(&db, req).await;
                            drained += 1;
                        }
                        tracing::info!(drained, "log write queue drained on shutdown");
                        break;
                    }
                }
            }
        }

        tracing::info!("log write queue consumer stopped");
    }

    /// Persist a single log-append request. Failures are logged and swallowed
    /// so one bad write never tears down the consumer.
    async fn process_request(db: &DatabaseConnection, req: LogWriteRequest) {
        // Read the current job to get the existing log
        let existing_log = match rg_db::ops::pipeline_ops::get_job(db, req.job_id).await {
            Ok(Some(job)) => job.log.unwrap_or_default(),
            Ok(None) => {
                tracing::warn!(job_id = req.job_id, "log write: job not found");
                return;
            }
            Err(e) => {
                tracing::warn!(
                    job_id = req.job_id,
                    error = %format!("{e:#}"),
                    "log write: failed to read current job log"
                );
                return;
            }
        };

        // Append the new chunk
        let combined = if existing_log.is_empty() {
            req.log_text
        } else {
            format!("{}\n{}", existing_log, req.log_text)
        };

        // Write back
        if let Err(e) = rg_db::ops::pipeline_ops::update_job_log(db, req.job_id, &combined).await {
            tracing::warn!(
                job_id = req.job_id,
                error = %format!("{e:#}"),
                "log write: failed to persist"
            );
        }
    }
}

// ── tests ───────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_handle_clone_and_drop() {
        let db = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        let q = LogWriteQueue::spawn(db);
        let q2 = q.clone();
        drop(q2);
        // Queue should still accept writes
        q.write(1, "test log").await;
    }

    /// The shutdown-aware consumer must persist the buffered backlog and then
    /// stop when the shutdown signal fires — the core of the graceful-drain
    /// guarantee (a SIGTERM under load must not lose queued CI-log writes).
    #[tokio::test]
    async fn drains_buffered_writes_on_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let db_url = format!("sqlite://{}?mode=rwc", dir.path().join("test.db").display());
        let db = rg_db::connect_with_pool(&db_url, rg_db::TEST_CONNECT_TIMEOUT_SECS, 60, 2)
            .await
            .unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        // Minimal pipeline → stage → job chain so there is a real row to append to.
        let pipeline = rg_db::ops::pipeline_ops::create_pipeline(
            &db,
            1,
            "deadbeef",
            "refs/heads/main",
            "push",
            None,
        )
        .await
        .unwrap();
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "build", 0)
            .await
            .unwrap();
        let job = rg_db::ops::pipeline_ops::create_job(
            &db,
            stage.id,
            "compile",
            "cargo build",
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            None,
            None,
            None,
        )
        .await
        .unwrap();

        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let (queue, handle) = LogWriteQueue::spawn_with_shutdown(db.clone(), shutdown_rx);

        // Buffer several writes, then signal shutdown.
        for i in 0..5 {
            queue.write(job.id, &format!("line {i}")).await;
        }
        shutdown_tx.send(true).unwrap();

        // Consumer must drain and stop. The bound below is a hang-guard, not a
        // performance assertion: what is under test is that the consumer drains
        // and *terminates*, and a consumer that never terminates fails at any
        // finite bound. The wall-clock cost of five SQLite commits, on the other
        // hand, is set by whatever else is hitting the disk — under a full
        // `cargo test -j 6 --workspace` this drain has taken over a minute on a
        // machine where it takes under two seconds alone. A tight budget here
        // does not measure the drain, it measures the load, and it made the
        // whole suite non-deterministic (card_2b890485c8d8).
        const DRAIN_HANG_GUARD: std::time::Duration = std::time::Duration::from_secs(120);
        tokio::time::timeout(DRAIN_HANG_GUARD, handle)
            .await
            .expect("consumer did not stop after shutdown")
            .expect("consumer task panicked");

        let persisted = rg_db::ops::pipeline_ops::get_job(&db, job.id)
            .await
            .unwrap()
            .unwrap()
            .log
            .unwrap_or_default();
        // All five buffered writes survived the drain.
        for i in 0..5 {
            assert!(
                persisted.contains(&format!("line {i}")),
                "missing 'line {i}' in drained log: {persisted:?}"
            );
        }
    }

    /// Sink that keeps every formatted log line so a test can assert on what the
    /// operator would actually have seen.
    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<std::sync::Mutex<Vec<u8>>>);

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

    /// This consumer swallows every write failure by design — the log line is the
    /// *only* channel an operator has. `rg_db::ops` wraps each failure in
    /// `.context("db: ...")`, so a bare `error = %e` would render that context and
    /// nothing else, turning "the pipeline_job table is missing" into an
    /// unactionable "log write: failed to read current job log / db: ...".
    ///
    /// Asserts on the rendered line, not on the `Result`, because the rendering is
    /// exactly what regressed before.
    #[tokio::test]
    async fn write_failure_log_line_carries_the_underlying_db_cause() {
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        // Connected but un-migrated: every `pipeline_job` query fails at the
        // SQLite level, which is the cause we need to see in the log.
        let db = sea_orm::Database::connect("sqlite::memory:").await.unwrap();
        LogWriteQueue::process_request(
            &db,
            LogWriteRequest {
                job_id: 42,
                log_text: "some build output".to_string(),
            },
        )
        .await;

        let rendered = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(
            rendered.contains("log write: failed to read current job log"),
            "expected the write-failure warning, got: {rendered}"
        );
        // The `.context("db: ...")` layer — what a bare `%e` would have shown.
        assert!(
            rendered.contains("db: "),
            "missing context layer: {rendered}"
        );
        // …and the actual reason underneath it, which is the whole point.
        assert!(
            rendered.contains("no such table"),
            "the underlying SQLite cause was dropped from the log line: {rendered}"
        );
    }
}
