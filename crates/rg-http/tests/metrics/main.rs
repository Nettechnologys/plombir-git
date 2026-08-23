//! The `rg-http` tests that install the process-global Prometheus registry.
//!
//! A second test binary next to `tests/integration`, and the only reason for it
//! is the word *process-global*. The registry, every metric handle under it and
//! the `rg_core::metrics_hook` observers that feed it are one per process, by
//! design: a server scrapes one registry. A test that installs them therefore
//! changes the world for every other test sharing the executable, and the
//! counters it then reads carry whatever the neighbours did in the meantime.
//!
//! Kept in the integration binary, that produced three failures that appeared
//! only in the full run (card_00b2bd65060e): a funnel test that expected
//! `[3, 1, 2, 1]` and read `[18, 4, 2, 1]`, a second registry installation
//! rejected as `REQUEST_COUNT already set`, and a route-access sweep whose
//! `GET /metrics` row was excused on the grounds that "the harness never
//! installs the registry" — which had stopped being true.
//!
//! `cargo test` runs the tests of one binary in parallel threads, so isolation
//! here means a *process*, and the cost is one more link of the server stack.
//! That is the price of the property; the alternative was three tests that pass
//! only when run alone, which is a green run that proves nothing.
//!
//! Within this binary the tests still share the process, so they take
//! [`METRIC_SERIAL`] and run one at a time. The two here happen to move
//! disjoint counters — that is luck, not a property, and the next one added
//! would not be so lucky.

use std::sync::LazyLock;

#[allow(dead_code)]
#[path = "../integration/common/mod.rs"]
mod common;

mod import_metric_funnel_tests;
mod repo_deletion_metric_tests;

/// Held for the length of any test that reads a process-global counter.
///
/// `tokio::sync::Mutex` rather than `std`: the guard is held across `.await`
/// points for the whole body of an async test.
pub(crate) static METRIC_SERIAL: LazyLock<tokio::sync::Mutex<()>> =
    LazyLock::new(|| tokio::sync::Mutex::new(()));
