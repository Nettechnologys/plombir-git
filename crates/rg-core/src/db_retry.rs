//! One retry policy for the bounded database-write loops, so the same two
//! situations are not answered by whatever policy each loop happened to grow.
//!
//! Every one of those loops retries on two outcomes that look alike in the
//! error type and want opposite treatment:
//!
//! * **a lost race** — a UNIQUE violation, or a compare-and-swap that found the
//!   row already moved. The attempt did useful work: someone committed, so
//!   `MAX(number) + 1` and the row version this attempt will re-read have both
//!   moved on. Retrying immediately is right, and waiting would only slow down
//!   a request that already knows what to do next.
//!
//! * **a busy backend** — `database is locked` on SQLite, a serialization
//!   failure on PostgreSQL. Nothing moved and nothing will until the holder
//!   commits. Retrying immediately is a busy-spin: every competitor hammers the
//!   same held lock, none of them waits, and eight concurrent creates burn a
//!   32-attempt budget in milliseconds while the writer is still writing. That
//!   turns the runaway guard into a concurrency budget — which is exactly what
//!   the loops' own comments say it is not — and hands a 5xx to correct callers
//!   (card_f0fd0aaa87b5).
//!
//! The jitter is not decoration. Without it the competitors that collided wake
//! together and collide again on the same schedule; the whole point of the wait
//! is that they stop being synchronized. How long that wait is belongs to
//! [`rg_db::contention`], which the retrying transactions inside `rg_db` share:
//! this module decides *whether* to wait, not *how long*.

use std::time::Duration;

use sea_orm::DbErr;

/// What a failed attempt at a database write deserves next.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Retry {
    /// Someone else committed. Re-read and write again straight away.
    Now,
    /// The backend is busy. Wait — jittered — before adding to the contention.
    AfterWaiting,
    /// Not a race. Re-reading cannot fix it, so the loop must not spin on it.
    Never,
}

impl Retry {
    /// Whether another attempt is worth making at all.
    pub(crate) fn is_worthwhile(self) -> bool {
        !matches!(self, Retry::Never)
    }

    /// Wait as long as this verdict calls for. A no-op for everything but
    /// [`Retry::AfterWaiting`], so a call site can await it unconditionally.
    pub(crate) async fn wait(self, attempt: usize) {
        if self == Retry::AfterWaiting {
            tokio::time::sleep(backoff_for(attempt)).await;
        }
    }
}

/// Classify a failure that arrived as a bare [`DbErr`] — a `BEGIN`, a `COMMIT`,
/// or any op that has not been wrapped in context yet.
pub(crate) fn classify(error: &DbErr) -> Retry {
    if rg_db::is_unique_violation(error) {
        Retry::Now
    } else if rg_db::is_retryable_transaction_error(error) {
        Retry::AfterWaiting
    } else {
        Retry::Never
    }
}

/// Classify a failure that has been through `anyhow` context, which is how
/// every `ops::` call returns.
pub(crate) fn classify_anyhow(error: &anyhow::Error) -> Retry {
    if rg_db::is_unique_violation_anyhow(error) {
        Retry::Now
    } else if rg_db::is_retryable_transaction_error_anyhow(error) {
        Retry::AfterWaiting
    } else {
        Retry::Never
    }
}

/// The wait itself is [`rg_db::contention::contention_backoff`] — one growing,
/// jittered window shared with the retrying transactions inside `rg_db`, so a
/// contended write is not answered by two different policies depending on which
/// crate the loop happens to live in. What stays here is *when* to wait at all,
/// which is this module's whole subject.
fn backoff_for(attempt: usize) -> Duration {
    rg_db::contention::contention_backoff(attempt)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// That the window grows and stops at a ceiling is the backoff function's
    /// own guarantee and is tested where it lives, in `rg_db::contention`.
    /// What this module owes is that its verdicts route through it at all.
    #[tokio::test]
    async fn only_a_busy_backend_waits() {
        // A lost race must not pay for the wait a held lock needs — the
        // distinction this module exists for.
        assert!(Retry::Now.is_worthwhile());
        assert!(Retry::AfterWaiting.is_worthwhile());
        assert!(!Retry::Never.is_worthwhile());

        let started = std::time::Instant::now();
        Retry::Now.wait(32).await;
        Retry::Never.wait(32).await;
        assert!(
            started.elapsed() < Duration::from_millis(5),
            "a lost race slept for {:?}",
            started.elapsed()
        );
    }

    /// Which failures are the two race classes is `rg_db`'s question and is
    /// tested there against real backend error codes — building a `sqlx`
    /// database error by hand here would only test the fixture. What is this
    /// module's own is the default: anything neither predicate claims must not
    /// be retried at all, or a bounded loop spins on a failure that re-reading
    /// cannot fix.
    #[test]
    fn a_failure_neither_predicate_claims_is_never_retried() {
        assert_eq!(
            classify(&DbErr::Custom("no connection".into())),
            Retry::Never
        );
        assert_eq!(
            classify(&DbErr::RecordNotFound("issue".into())),
            Retry::Never
        );
        assert_eq!(
            classify_anyhow(&anyhow::anyhow!("FOREIGN KEY constraint failed")),
            Retry::Never
        );
    }
}
