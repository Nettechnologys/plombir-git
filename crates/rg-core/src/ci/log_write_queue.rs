//! CI/CD job log write queue.
//!
//! Serialises log writes through a single `tokio::sync::mpsc` channel so that
//! concurrent pipeline jobs do not contend on SQLite writes.  A background
//! consumer task receives log-update requests and commits them sequentially.
//!
//! # Usage
//!
//! ```ignore
//! let queue = LogWriteQueue::spawn(db.clone());
//! queue.write(job_id, &log_text).await?;
//! ```
//!
//! # Stream-ready design
//!
//! The queue accepts any number of writes per job.  Future streaming log
//! support (runner sends lines during execution) can call `write()` for each
//! chunk. A full queue returns an error to the caller for retry.

use sea_orm::DatabaseConnection;
use std::sync::Arc;
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;

/// Maximum buffered log writes before the channel exerts back-pressure.
const CHANNEL_CAPACITY: usize = 4096;
/// Maximum aggregate bytes held by requests waiting in the queue.
const PENDING_BYTE_CAPACITY: usize = 32 * 1024 * 1024;

/// A request to write (append) log text for a given job.
struct LogWriteRequest {
    /// Pipeline job ID.
    pub job_id: i64,
    /// Log text chunk to append.
    pub log_text: String,
    pub replace: bool,
    pub reply: oneshot::Sender<anyhow::Result<()>>,
    _pending_bytes: OwnedSemaphorePermit,
}

#[derive(Debug, thiserror::Error)]
pub enum LogWriteError {
    #[error("log write queue is full")]
    Full,
    #[error("log write queue is closed")]
    Closed,
    #[error("job log could not be persisted: {0:#}")]
    Persistence(#[source] anyhow::Error),
}

/// Cloneable handle to the log write queue.
///
/// Drop the last clone to stop the background consumer gracefully.
#[derive(Clone)]
pub struct LogWriteQueue {
    tx: mpsc::Sender<LogWriteRequest>,
    pending_bytes: Arc<Semaphore>,
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

        Self {
            tx,
            pending_bytes: Arc::new(Semaphore::new(PENDING_BYTE_CAPACITY)),
        }
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

        (
            Self {
                tx,
                pending_bytes: Arc::new(Semaphore::new(PENDING_BYTE_CAPACITY)),
            },
            handle,
        )
    }

    /// Queue a log chunk for a given job.
    ///
    /// A successful return means the chunk is committed, not merely buffered.
    /// A full queue is reported to the caller for a retry.
    pub async fn write(&self, job_id: i64, log_text: &str) -> Result<(), LogWriteError> {
        self.submit(job_id, log_text, false).await
    }

    /// Serialize a full-log replacement behind already accepted appends.
    pub async fn replace(&self, job_id: i64, log_text: &str) -> Result<(), LogWriteError> {
        self.submit(job_id, log_text, true).await
    }

    async fn submit(
        &self,
        job_id: i64,
        log_text: &str,
        replace: bool,
    ) -> Result<(), LogWriteError> {
        let bytes = u32::try_from(log_text.len().max(1)).map_err(|_| LogWriteError::Full)?;
        let permit = self
            .pending_bytes
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| LogWriteError::Full)?;
        let (reply, received) = oneshot::channel();
        let req = LogWriteRequest {
            job_id,
            log_text: log_text.to_string(),
            replace,
            reply,
            _pending_bytes: permit,
        };
        self.tx.try_send(req).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => LogWriteError::Full,
            mpsc::error::TrySendError::Closed(_) => LogWriteError::Closed,
        })?;
        received
            .await
            .map_err(|_| LogWriteError::Closed)?
            .map_err(LogWriteError::Persistence)
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

    /// Persist one request and report its result to the waiting HTTP handler.
    async fn process_request(db: &DatabaseConnection, req: LogWriteRequest) {
        let result = if req.replace {
            rg_db::ops::pipeline_ops::update_job_log(db, req.job_id, &req.log_text).await
        } else {
            rg_db::ops::pipeline_ops::append_job_log(db, req.job_id, &req.log_text).await
        };
        if let Err(ref e) = result {
            tracing::warn!(
                job_id = req.job_id,
                error = %format!("{e:#}"),
                "log write: failed to persist"
            );
        }
        if req.reply.send(result).is_err() {
            tracing::debug!(
                job_id = req.job_id,
                "log write: requester disconnected before acknowledgement"
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
        assert!(matches!(
            q.write(1, "test log").await,
            Err(LogWriteError::Persistence(_))
        ));
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

        // Every successful return already means durable. Shutdown then closes
        // the consumer without dropping any acknowledged entry.
        for i in 0..5 {
            queue.write(job.id, &format!("line {i}")).await.unwrap();
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

        let persisted = rg_db::ops::pipeline_ops::get_job_log_slice(&db, job.id, 0, 1000)
            .await
            .unwrap()
            .unwrap()
            .content
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

    /// A failed write reaches the caller and the operator log with its cause.
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
        let permit = Arc::new(Semaphore::new(100))
            .try_acquire_many_owned(17)
            .unwrap();
        let (reply, received) = oneshot::channel();
        LogWriteQueue::process_request(
            &db,
            LogWriteRequest {
                job_id: 42,
                log_text: "some build output".to_string(),
                replace: false,
                reply,
                _pending_bytes: permit,
            },
        )
        .await;
        assert!(received.await.unwrap().is_err());

        let rendered = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(
            rendered.contains("log write: failed to persist"),
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

    #[tokio::test]
    async fn full_queue_reports_pressure_without_acknowledging_a_chunk() {
        let (tx, _rx) = mpsc::channel(1);
        let queue = LogWriteQueue {
            tx,
            pending_bytes: Arc::new(Semaphore::new(PENDING_BYTE_CAPACITY)),
        };
        let permit = queue
            .pending_bytes
            .clone()
            .try_acquire_many_owned(5)
            .unwrap();
        let (reply, _received) = oneshot::channel();
        queue
            .tx
            .try_send(LogWriteRequest {
                job_id: 1,
                log_text: "first".to_owned(),
                replace: false,
                reply,
                _pending_bytes: permit,
            })
            .unwrap();
        assert!(matches!(
            queue.write(1, "second").await,
            Err(LogWriteError::Full)
        ));
        assert_eq!(queue.pending_count(), 1);
    }

    #[tokio::test]
    async fn byte_budget_reports_pressure_before_the_entry_limit() {
        let (tx, _rx) = mpsc::channel(2);
        let queue = LogWriteQueue {
            tx,
            pending_bytes: Arc::new(Semaphore::new(4)),
        };
        let permit = queue
            .pending_bytes
            .clone()
            .try_acquire_many_owned(4)
            .unwrap();
        let (reply, _received) = oneshot::channel();
        queue
            .tx
            .try_send(LogWriteRequest {
                job_id: 1,
                log_text: "four".to_owned(),
                replace: false,
                reply,
                _pending_bytes: permit,
            })
            .unwrap();
        assert!(matches!(
            queue.write(1, "x").await,
            Err(LogWriteError::Full)
        ));
        assert_eq!(queue.pending_count(), 1);
    }
}
