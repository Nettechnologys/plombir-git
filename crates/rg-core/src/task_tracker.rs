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

use tokio_util::task::TaskTracker;

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
        let done = Arc::new(AtomicBool::new(false));
        let done_in_task = done.clone();

        delivery_tracker().spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            done_in_task.store(true, Ordering::SeqCst);
        });

        // Still in flight immediately after spawn (the task sleeps first).
        assert!(!done.load(Ordering::SeqCst));

        let tracker = delivery_tracker();
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

        // Restore global state so closing here doesn't leak into other tests.
        tracker.reopen();
    }
}
