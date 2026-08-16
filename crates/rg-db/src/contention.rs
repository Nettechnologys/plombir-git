//! What to do about a transaction the backend refused because someone else was
//! writing — the wait, and the loop that uses it.
//!
//! [`is_retryable_transaction_error`](crate::is_retryable_transaction_error)
//! already answers *which* failures these are. This module answers the two
//! things every caller then has to decide the same way: how long to wait, and
//! how many times to try.
//!
//! # Why a read-then-write transaction needs this at all
//!
//! [`connect_with_pool`](crate::connect_with_pool) sets SQLite's
//! `busy_timeout`, and that covers the case people expect: a statement that
//! wants the write lock while someone else holds it waits instead of failing.
//! It does **not** cover a transaction that reads before it writes.
//!
//! SeaORM opens every transaction with a plain `BEGIN`, which in SQLite is
//! `BEGIN DEFERRED`: the transaction takes its read snapshot at the first
//! `SELECT` and only asks for the write lock at the first `UPDATE`. If another
//! connection committed in that gap, the snapshot it has already read from is
//! no longer the newest one, so upgrading it would let it write on top of data
//! it never saw. SQLite refuses with `SQLITE_BUSY_SNAPSHOT` (517) — and refuses
//! **immediately**, because waiting cannot help: the snapshot stays stale for
//! as long as the transaction lives, so `busy_timeout` is not even consulted.
//!
//! `BEGIN IMMEDIATE` would take the write lock up front and put the wait back
//! under `busy_timeout`, but SeaORM 1.1 cannot issue it — its `AccessMode` is
//! unsupported on SQLite and only logs a warning. So the remedy is the other
//! standard one: roll the transaction back and run it again from a fresh
//! snapshot. Where a transaction can write first instead, that is better still
//! and several already do (see the SQLite branch of
//! [`transfer_repository`](crate::ops::repo_ops::transfer_repository)).
//!
//! Re-running is safe precisely *because* the transaction rolled back: the
//! closure re-reads the state it decides on, so the next attempt sees the write
//! that displaced it. Callers must pass a closure that can run more than once.

use std::time::Duration;

use anyhow::{Context, Result};
use rand::Rng;

/// Ceiling of the wait after the first refusal, doubling per attempt.
const FIRST_BACKOFF: Duration = Duration::from_millis(1);

/// Where the doubling stops. With the attempt budgets the loops carry, this
/// bounds the total wait at well under a second — long enough to let a SQLite
/// writer finish, short enough that a request never looks hung.
const BACKOFF_CEILING: Duration = Duration::from_millis(25);

/// Attempts [`retry_transaction`] makes in total, including the first.
const MAX_ATTEMPTS: usize = 8;

/// How long to wait before attempt `attempt + 1` of a contended write.
///
/// Full jitter over an exponentially growing ceiling: uniform in
/// `0..=min(FIRST_BACKOFF << (attempt - 1), BACKOFF_CEILING)`.
///
/// Full jitter rather than a fixed fraction because the competitors are
/// contending for one lock — spreading them across the whole window is what
/// takes them off a common schedule, and the growing ceiling is what keeps
/// spreading them when the first window turns out to be too narrow. Without it
/// the competitors that collided wake together and collide again on the same
/// schedule.
pub fn contention_backoff(attempt: usize) -> Duration {
    let doublings = u32::try_from(attempt.saturating_sub(1)).unwrap_or(u32::MAX);
    let ceiling = FIRST_BACKOFF
        .checked_mul(1u32.checked_shl(doublings.min(31)).unwrap_or(u32::MAX))
        .unwrap_or(BACKOFF_CEILING)
        .min(BACKOFF_CEILING);

    // The generator is dropped before any await: `ThreadRng` is not `Send`, and
    // holding it across one would make every caller's future `!Send`.
    let micros = {
        let mut rng = rand::thread_rng();
        rng.gen_range(0..=ceiling.as_micros() as u64)
    };
    Duration::from_micros(micros)
}

/// Run `operation`, re-running it while the backend refuses it for contention.
///
/// For the self-contained transactions that have no caller-visible loop of
/// their own to hang a retry on. A loop that must also treat a *lost race*
/// (unique violation, moved compare-and-swap row) as retryable wants
/// `rg_core::db_retry` instead: that distinction is the caller's, because only
/// the caller knows whether re-reading changes what it writes.
///
/// `what` names the operation in the log line a budget that runs out produces.
/// Running out is a real failure and the caller still gets the error — the
/// count is the only signal that the database was contended rather than broken.
pub async fn retry_transaction<T, F, Fut>(what: &str, mut operation: F) -> Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = Result<T>>,
{
    for attempt in 1..=MAX_ATTEMPTS {
        match operation().await {
            Ok(value) => {
                if attempt > 1 {
                    tracing::debug!(
                        operation = what,
                        attempt,
                        "the operation went through once the contending writer had committed"
                    );
                }
                return Ok(value);
            }
            Err(error) if !crate::is_retryable_transaction_error_anyhow(&error) => {
                return Err(error)
            }
            Err(error) if attempt == MAX_ATTEMPTS => {
                return Err(error).context(format!(
                    "db: {what} after {MAX_ATTEMPTS} concurrent conflicts"
                ))
            }
            Err(_) => tokio::time::sleep(contention_backoff(attempt)).await,
        }
    }
    unreachable!("the final attempt returns from inside the loop")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use anyhow::anyhow;
    use sea_orm::{ConnectionTrait, TransactionTrait};

    async fn scratch_db(name: &str) -> (crate::DatabaseConnection, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let db = crate::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", dir.path().join(name).display()),
            crate::TEST_CONNECT_TIMEOUT_SECS,
            60,
            2,
        )
        .await
        .unwrap();
        db.execute_unprepared("CREATE TABLE t (id INTEGER PRIMARY KEY, n INTEGER NOT NULL)")
            .await
            .unwrap();
        db.execute_unprepared("INSERT INTO t (id, n) VALUES (1, 0)")
            .await
            .unwrap();
        (db, dir)
    }

    #[test]
    fn the_window_grows_and_then_stops_at_the_ceiling() {
        // Sampled rather than reasoned about: the shift is the part that
        // silently overflows, and the ceiling is the part that stops it.
        for attempt in 1..=64usize {
            let bound = FIRST_BACKOFF * (1u32 << (attempt - 1).min(31));
            for _ in 0..32 {
                let waited = contention_backoff(attempt);
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
        let early: Duration = (0..256)
            .map(|_| contention_backoff(1))
            .max()
            .expect("samples");
        let late: Duration = (0..256)
            .map(|_| contention_backoff(8))
            .max()
            .expect("samples");
        assert!(
            early < late,
            "the backoff stopped growing: {early:?} vs {late:?}"
        );
    }

    /// The mechanism the module documents, with no retry in sight: a
    /// transaction that reads and then writes is refused outright once another
    /// connection commits in between — and refused *instantly*, so no
    /// `busy_timeout` would have been long enough to prevent it.
    #[tokio::test]
    async fn a_deferred_transaction_that_reads_before_it_writes_is_refused_after_a_commit() {
        let (db, _dir) = scratch_db("deferred.db").await;

        let txn = db.begin().await.unwrap();
        // Takes the read snapshot.
        txn.execute_unprepared("SELECT n FROM t WHERE id = 1")
            .await
            .unwrap();
        // A different pooled connection moves the database on underneath it.
        db.execute_unprepared("UPDATE t SET n = 1 WHERE id = 1")
            .await
            .unwrap();

        let started = std::time::Instant::now();
        let refusal = txn
            .execute_unprepared("UPDATE t SET n = 2 WHERE id = 1")
            .await
            .expect_err("the stale snapshot must not be allowed to write");
        let waited = started.elapsed();

        assert!(
            crate::is_retryable_transaction_error(&refusal),
            "the refusal must be classified as contention, got {refusal:?}"
        );
        assert!(
            waited < Duration::from_secs(1),
            "`busy_timeout` is 5s; a refusal it covered could not have arrived in {waited:?}"
        );
    }

    /// The retry does not merely swallow the refusal: it runs the closure
    /// again, so the attempt that succeeds is a real one against fresh state.
    #[tokio::test]
    async fn a_contended_transaction_is_run_again_against_what_displaced_it() {
        let (db, _dir) = scratch_db("retry.db").await;

        let attempts = AtomicUsize::new(0);
        let winning_attempt = retry_transaction("read-then-write t", || async {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst) + 1;
            let txn = db.begin().await.context("db: begin")?;
            txn.execute_unprepared("SELECT n FROM t WHERE id = 1")
                .await
                .context("db: read t")?;
            if attempt == 1 {
                // Exactly the interleaving of the test above.
                db.execute_unprepared("UPDATE t SET n = 7 WHERE id = 1")
                    .await
                    .context("db: contending write")?;
            }
            txn.execute_unprepared("UPDATE t SET n = n + 100 WHERE id = 1")
                .await
                .context("db: update t")?;
            txn.commit().await.context("db: commit")?;
            Ok(attempt)
        })
        .await
        .expect("the retry must carry the operation past a stale snapshot");

        assert_eq!(
            winning_attempt, 2,
            "the first attempt must have been refused"
        );
        let n: i64 = db
            .query_one(sea_orm::Statement::from_string(
                db.get_database_backend(),
                "SELECT n FROM t WHERE id = 1",
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get_by(0)
            .unwrap();
        assert_eq!(
            n, 107,
            "the winning attempt must have built on the write that displaced it, not on the \
             snapshot it lost"
        );
    }

    /// A failure that is not contention is returned on the spot. Retrying a
    /// genuine error would turn one report into eight and delay it by the whole
    /// backoff budget.
    #[tokio::test]
    async fn an_ordinary_failure_is_not_retried() {
        let attempts = AtomicUsize::new(0);
        let error = retry_transaction("always fails", || async {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(anyhow!("FOREIGN KEY constraint failed")).context("db: write t")
        })
        .await
        .expect_err("the operation fails");

        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        assert!(format!("{error:#}").contains("FOREIGN KEY"));
    }
}
