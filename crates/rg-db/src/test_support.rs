//! What a test needs when it parks a source writer behind a held SQLite write
//! lock: a writer that survives being refused, and an assertion that says what
//! really happened.
//!
//! Three regression tests in this crate share one shape. An instrumented
//! transaction takes SQLite's single writer slot and pauses; a spawned writer
//! is expected to sit behind it; the test reads the database to prove nothing
//! partial escaped, then releases the pause. Both halves of that shape have a
//! sharp edge, and both only show up under load — which is why the shape flaked
//! roughly once in four full `--lib` runs rather than visibly.
//!
//! # "The writer waits" is not what SQLite promises
//!
//! A blocked writer is refused in two different ways, and which one it meets is
//! decided by the machine rather than by the code under test:
//!
//! * It **waits** for the per-connection `busy_timeout` that
//!   [`connect_with_pool`](crate::connect_with_pool) sets — five seconds — and
//!   is then refused with `database is locked`. Five seconds is a policy for a
//!   live server, not a budget for a test that holds the lock on purpose while
//!   running its own queries and a fixed observation window; a loaded `--lib`
//!   run puts dozens of multi-threaded runtimes on the same cores and outgrows
//!   it.
//! * Or it is refused **immediately**, in well under a millisecond, without the
//!   busy handler being consulted at all. SQLite does that whenever waiting
//!   could deadlock — a connection that already holds a read inside its
//!   transaction cannot be made to sleep while it upgrades to a write — and
//!   whether a given pooled connection is in that state depends on what the
//!   pool handed out.
//!
//! Both are ordinary contention ([`is_retryable_transaction_error`](crate::is_retryable_transaction_error)
//! counts them), and a production writer already survives both by re-running
//! its transaction through [`retry_transaction`](crate::contention::retry_transaction).
//! A test writer that does not is not testing the boundary — it is testing how
//! busy the machine was. [`write_while_the_lock_is_held`] gives it the same
//! survival, on a deadline no observation window can reach.
//!
//! # A writer that failed looks exactly like a writer that was never blocked
//!
//! `assert!(timeout(window, &mut writer).await.is_err())` reads `Err` as "still
//! parked". A writer that returned a `DbErr`, and a writer that panicked (which
//! also drops any `oneshot` it was going to signal through), both resolve the
//! future *immediately* — so `is_err()` is false and the assertion reports the
//! opposite of the truth: "the writer crossed the boundary" when in fact it
//! never got in. [`assert_writer_stays_blocked`] keeps the real outcome in the
//! message.

use std::fmt::Debug;
use std::future::Future;
use std::time::{Duration, Instant};

use sea_orm::DbErr;
use tokio::task::JoinHandle;

/// How long a test watches a writer before concluding it is still parked.
pub(crate) const HELD_WRITER_WINDOW: Duration = Duration::from_millis(200);

/// How long a parked writer keeps re-trying before it reports contention as a
/// failure.
///
/// Far past anything [`HELD_WRITER_WINDOW`] plus the surrounding queries can
/// reach, so a loaded machine's scheduling can no longer masquerade as a
/// boundary that let a writer through.
const PARKED_WRITER_DEADLINE: Duration = Duration::from_secs(120);

/// Run one source write until the backend stops refusing it for contention.
///
/// `attempt` must be a *whole* transaction, because that is the unit being
/// re-run: a half-applied batch would come back to a second attempt as a
/// duplicate row rather than as contention. See the module docs for why a
/// refusal here says nothing about the boundary under test.
pub(crate) async fn write_while_the_lock_is_held<F, Fut>(mut attempt: F) -> Result<(), DbErr>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<(), DbErr>>,
{
    let deadline = Instant::now() + PARKED_WRITER_DEADLINE;
    let mut refusals = 0usize;
    loop {
        match attempt().await {
            Ok(()) => return Ok(()),
            Err(error)
                if !crate::is_retryable_transaction_error(&error) || Instant::now() >= deadline =>
            {
                return Err(error)
            }
            Err(_) => {
                refusals += 1;
                tokio::time::sleep(crate::contention::contention_backoff(refusals)).await;
            }
        }
    }
}

/// Assert a spawned writer is still parked behind the held lock — and say what
/// it actually did when it is not.
///
/// `crossed` names the boundary the writer would have crossed, so the contract
/// still reads as the test's subject; the outcome or the panic is appended when
/// the writer did something else entirely.
pub(crate) async fn assert_writer_stays_blocked<T>(writer: &mut JoinHandle<T>, crossed: &str)
where
    T: Debug,
{
    match tokio::time::timeout(HELD_WRITER_WINDOW, writer).await {
        Err(_still_parked) => {}
        Ok(Ok(outcome)) => {
            panic!("{crossed} — the writer finished with {outcome:?} instead of waiting")
        }
        Ok(Err(panic)) => {
            panic!("{crossed} — the writer task panicked instead of waiting: {panic}")
        }
    }
}
