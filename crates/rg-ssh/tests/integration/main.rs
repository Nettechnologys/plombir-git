//! Every `rg-ssh` integration test lives in this one binary.
//!
//! The same arithmetic that consolidated `rg-http`'s suite applies here, and
//! for the same reason: Cargo builds one test executable per file directly
//! under `tests/`, and each of those statically links the whole server stack —
//! `russh`, `rg-core`, `rg-db`, `sea-orm`, `gix`. The six files this directory
//! used to hold were six full links of that graph on every change to anything
//! below them, plus six copies of it in `target`. As submodules of a single
//! target they link once.
//!
//! Adding a test file means adding it here as a `mod`, otherwise it is not
//! compiled and not run — a file that nothing declares is silently dead.
//!
//! Test names are now prefixed with their module, which is where the old binary
//! name went: `--test ssh_lockout_tests` becomes
//! `-E 'test(ssh_lockout_tests::)'`. Isolation is unaffected — nextest, which is
//! the gate, already runs every test in its own process.

mod deactivated_ssh_tests;
mod ssh_failure_semantics_tests;
mod ssh_lockout_tests;
mod ssh_mfa_password_tests;
mod ssh_push_hook_tests;
mod ssh_push_tests;
