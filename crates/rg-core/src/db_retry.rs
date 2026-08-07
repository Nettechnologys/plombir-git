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
//! is that they stop being synchronized.

use std::time::Duration;

use rand::Rng;
use sea_orm::DbErr;

/// Ceiling of the wait after the first busy backend, doubling per attempt.
const FIRST_BACKOFF: Duration = Duration::from_millis(1);

/// Where the doubling stops. With the 32-attempt budget the loops carry, this
/// bounds the total wait at well under a second — long enough to let a SQLite
/// writer finish, short enough that a request never looks hung.
const BACKOFF_CEILING: Duration = Duration::from_millis(25);

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

/// Full jitter over an exponentially growing ceiling: uniform in
/// `0..=min(FIRST_BACKOFF << (attempt - 1), BACKOFF_CEILING)`.
///
/// Full jitter rather than a fixed fraction because the competitors are
/// contending for one lock — spreading them across the whole window is what
/// takes them off a common schedule, and the growing ceiling is what keeps
/// spreading them when the first window turns out to be too narrow.
fn backoff_for(attempt: usize) -> Duration {
    let doublings = u32::try_from(attempt.saturating_sub(1)).unwrap_or(u32::MAX);
    let ceiling = FIRST_BACKOFF
        .checked_mul(1u32.checked_shl(doublings.min(31)).unwrap_or(u32::MAX))
        .unwrap_or(BACKOFF_CEILING)
        .min(BACKOFF_CEILING);

    // The generator is dropped before the await: `ThreadRng` is not `Send`, and
    // holding it across one would make every caller's future `!Send`.
    let micros = {
        let mut rng = rand::thread_rng();
        rng.gen_range(0..=ceiling.as_micros() as u64)
    };
    Duration::from_micros(micros)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_grows_and_then_stops_at_the_ceiling() {
        // Sampled rather than reasoned about: the shift is the part that
        // silently overflows, and the ceiling is the part that stops it.
        for attempt in 1..=64usize {
            let bound = FIRST_BACKOFF * (1u32 << (attempt - 1).min(31));
            for _ in 0..32 {
                let waited = backoff_for(attempt);
                assert!(
                    waited <= BACKOFF_CEILING,
                    "attempt {attempt} waited {waited:?}, past the ceiling"
                );
                assert!(
                    waited <= bound.max(BACKOFF_CEILING),
                    "attempt {attempt} waited {waited:?}, past its own window"
                );
            }
        }
    }

    #[test]
    fn the_early_windows_are_narrower_than_the_late_ones() {
        // The guarantee is on the window, not on any single draw, so compare
        // the widest draw each window can produce.
        let early: Duration = (0..256).map(|_| backoff_for(1)).max().expect("samples");
        let late: Duration = (0..256).map(|_| backoff_for(8)).max().expect("samples");
        assert!(
            early < late,
            "the backoff stopped growing: {early:?} vs {late:?}"
        );
    }

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
