//! Process-global tracker for detached fire-and-forget delivery tasks.
//!
//! Some request-triggered work is deliberately spawned *detached* so the HTTP
//! handler can return without blocking on it:
//! - webhook delivery (`webhook::service::trigger_event`) — the outbound POST
//!   plus the `webhook_delivery` audit-row write;
//! - real-time WebSocket notification (`rg_http::ws::push_notification`) — the
//!   channel push plus the persisted notification row;
//! - post-push hooks (`rg_http::git_http::handle_git_receive_pack`) — the
//!   open-PR head-SHA refresh, the CI trigger and the webhook fan-out that run
//!   after the client already has its `200 OK`.
//!
//! The main graceful-shutdown path drains in-flight HTTP requests and the
//! long-lived loop workers, but a bare `tokio::spawn` is owned by nobody: on
//! `SIGTERM` it is severed mid-flight, leaving an undelivered webhook or a
//! half-written row. Routing those spawns through this shared [`TaskTracker`]
//! lets `rg_http::run` [`close`](TaskTracker::close) it and await the still
//! outstanding deliveries — bounded by the shutdown grace window — before the
//! Tokio runtime is torn down.
//!
//! It is a single process-global tracker created lazily on first use, so it
//! behaves identically whether a full server started it or a unit test spawned
//! a single task. Only `rg_http::run` ever closes/awaits it; every other caller
//! just spawns.

use std::sync::OnceLock;

pub use tokio_util::task::TaskTracker;

static DELIVERY_TRACKER: OnceLock<TaskTracker> = OnceLock::new();

/// The shared tracker for detached delivery tasks.
///
/// Spawn through `delivery_tracker().spawn(fut)` instead of `tokio::spawn(fut)`
/// wherever a request-triggered task must survive graceful shutdown. The
/// semantics are otherwise identical to [`tokio::spawn`] (a Tokio runtime must
/// be active).
pub fn delivery_tracker() -> &'static TaskTracker {
    DELIVERY_TRACKER.get_or_init(TaskTracker::new)
}

static CI_TRACKER: OnceLock<TaskTracker> = OnceLock::new();

/// The shared tracker for embedded CI pipeline execution.
///
/// Its own tracker rather than [`delivery_tracker`], because the two are drained
/// under different promises. A delivery is short and `rg_http::run` waits for it
/// the moment the HTTP server stops; a pipeline can run for minutes, so waiting
/// for it *there* would either hold the HTTP drain open for the whole grace
/// window or mean nothing. What the stop path actually waits for here is the
/// runner's **unwind** — the interrupted job's container removed and its row
/// handed back to `pending` — which is short, and only because the runner is
/// given the same shutdown signal and is already on its way out
/// (card_34368880dc20).
///
/// Before this existed, `spawn_internal_runner` used a bare `tokio::spawn`: on
/// `SIGTERM` the pipeline was severed wherever it stood, and the `pipeline_jobs`
/// row stayed `running` until the stuck-job sweep reclaimed it ten minutes
/// later. That is the default configuration — an instance with no external
/// runner has no other executor.
///
/// Only `rg-cli`'s `run_serve` ever closes/awaits it; every other caller just
/// spawns.
pub fn ci_tracker() -> &'static TaskTracker {
    CI_TRACKER.get_or_init(TaskTracker::new)
}

/// Await a shutdown signal if present, otherwise never resolve.
///
/// Lets a `tokio::select!` arm be conditionally armed on an `Option<Receiver>`,
/// which is what every long-lived loop worker here needs: the same loop runs
/// under a server that fans out a `SIGTERM` and under a test that has no
/// coordinator at all. It lives next to the tracker because both are the same
/// concern — how background work learns that the process is going down.
pub async fn wait_optional_shutdown(shutdown_rx: &mut Option<tokio::sync::watch::Receiver<bool>>) {
    match shutdown_rx {
        Some(rx) => {
            if rx.changed().await.is_err() {
                // Sender dropped: treat it the same as an explicit shutdown.
            }
        }
        None => std::future::pending::<()>().await,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::Duration;

    /// The shutdown contract `rg_http::run` relies on: a task spawned through
    /// the tracker is outstanding until it finishes, and `close()` + `wait()`
    /// blocks until that detached work has run to completion — the difference
    /// between a drained webhook delivery and one severed mid-write on SIGTERM.
    #[tokio::test]
    async fn close_then_wait_drains_a_spawned_task() {
        let tracker = TaskTracker::new();
        let done = Arc::new(AtomicBool::new(false));
        let done_in_task = done.clone();

        tracker.spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            done_in_task.store(true, Ordering::SeqCst);
        });

        // Still in flight immediately after spawn (the task sleeps first).
        assert!(!done.load(Ordering::SeqCst));

        tracker.close();
        // Hang-guard, not a deadline — see the same reasoning in
        // `ci::log_write_queue` (card_2b890485c8d8). The tracked task sleeps
        // 50ms; a tracker that never drains fails at any finite bound, while a
        // tight bound turns machine load into a red suite.
        tokio::time::timeout(Duration::from_secs(120), tracker.wait())
            .await
            .expect("delivery tracker drained within timeout");

        assert!(
            done.load(Ordering::SeqCst),
            "detached task ran to completion before wait() returned"
        );
    }
}
