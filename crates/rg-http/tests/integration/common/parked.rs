//! Asserting that a spawned request is still parked at a boundary.
//!
//! `assert!(timeout(window, &mut request).await.is_err())` reads `Err` as
//! "still parked". A request task that returned an error, and one that
//! panicked, both resolve the future *immediately* — so `is_err()` is false and
//! the assertion reports the opposite of the truth: "it crossed the boundary"
//! when in fact it never got in, with the real failure left in the task's own
//! output, a screen above what the harness shows.

use std::fmt::Debug;
use std::time::Duration;

use tokio::task::JoinHandle;

/// How long a test watches a spawned request before concluding it is still
/// parked at the boundary under test.
const HELD_REQUEST_WINDOW: Duration = Duration::from_millis(200);

/// Assert a spawned request is still parked at the boundary under test — and
/// say what it actually did when it is not.
///
/// `crossed` names the boundary the request would have crossed, so the contract
/// still reads as the test's subject; the outcome or the panic is appended when
/// the request did something else entirely.
pub async fn assert_request_stays_blocked<T>(request: &mut JoinHandle<T>, crossed: &str)
where
    T: Debug,
{
    match tokio::time::timeout(HELD_REQUEST_WINDOW, request).await {
        Err(_still_parked) => {}
        Ok(Ok(outcome)) => {
            panic!("{crossed} — the request finished with {outcome:?} instead of waiting")
        }
        Ok(Err(panic)) => {
            panic!("{crossed} — the request task panicked instead of waiting: {panic}")
        }
    }
}
