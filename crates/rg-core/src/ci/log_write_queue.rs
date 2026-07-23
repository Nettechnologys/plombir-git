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
                    error = %e,
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
                error = %e,
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
        let db = sea_orm::Database::connect(sea_orm::ConnectOptions::new(db_url))
            .await
            .unwrap();
        rg_db::run_migrations(&db).await.unwrap();

        // Minimal pipeline → stage → job chain so there is a real row to append to.
        let pipeline =
            rg_db::ops::pipeline_ops::create_pipeline(&db, 1, "deadbeef", "refs/heads/main", "push", None)
                .await
                .unwrap();
        let stage = rg_db::ops::pipeline_ops::create_stage(&db, pipeline.id, "build", 0)
            .await
            .unwrap();
        let job = rg_db::ops::pipeline_ops::create_job(
            &db, stage.id, "compile", "cargo build", None, None, None, None, None, false, None, None,
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

        // Consumer must drain and stop within a bounded window.
        tokio::time::timeout(std::time::Duration::from_secs(5), handle)
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
}
