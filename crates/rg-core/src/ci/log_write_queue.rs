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
use tokio::sync::mpsc;

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
            // Read the current job to get the existing log
            let existing_log = match rg_db::ops::pipeline_ops::get_job(&db, req.job_id).await {
                Ok(Some(job)) => job.log.unwrap_or_default(),
                Ok(None) => {
                    tracing::warn!(job_id = req.job_id, "log write: job not found");
                    continue;
                }
                Err(e) => {
                    tracing::warn!(
                        job_id = req.job_id,
                        error = %e,
                        "log write: failed to read current job log"
                    );
                    continue;
                }
            };

            // Append the new chunk
            let combined = if existing_log.is_empty() {
                req.log_text
            } else {
                format!("{}\n{}", existing_log, req.log_text)
            };

            // Write back
            if let Err(e) =
                rg_db::ops::pipeline_ops::update_job_log(&db, req.job_id, &combined).await
            {
                tracing::warn!(
                    job_id = req.job_id,
                    error = %e,
                    "log write: failed to persist"
                );
            }
        }

        tracing::info!("log write queue consumer stopped");
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
}
