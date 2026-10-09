//! Plombir Git CI runner agent — the register / poll / execute implementation.
//!
//! The crate ships both a library and the `plombir-git-runner` binary on purpose:
//! `plombir-git runner` (the deprecated subcommand of the main `plombir-git` binary)
//! used to carry its **own**, much older copy of this loop, which drifted badly —
//! it never read `runner.toml`, so it registered a fresh runner on every start,
//! and it also lacked the heartbeat, the per-job workspace snapshot, the job
//! timeout and the cache round-trip. Exposing the real implementation here lets
//! that subcommand delegate instead of duplicate, so there is exactly one runner
//! behaviour to reason about.

/// The HTTP calls this agent makes against the server's runner API.
///
/// Public for one reason: every URL in here is one of the server's own routes,
/// spelled out a second time in a crate that cannot see the route table. The
/// sweep that proves each one is still mounted lives in `rg-http`, where the
/// router exists, and it drives these very functions — see that crate's
/// `runner_route_coverage_tests`.
pub mod api;
mod commands;
mod config;
mod executor;
mod workspace;

pub use commands::{cmd_register, cmd_run, RegisterCommand, RunCommand};

/// The poll-and-execute loop with its stop signal as an argument.
///
/// Public for the same reason [`api`] is: the half that proves it lives in
/// another crate. A runner being stopped has to reach
/// `POST /runners/{id}/deregister` so the job it was holding comes straight back
/// to the pool, and only `rg-http` has the router and the database to observe
/// that. Driving it from a real signal would take the test binary down with the
/// runner, so the signal is a parameter — `cmd_run` supplies the real one.
pub use commands::{run_jobs_until_shutdown, run_jobs_until_shutdown_checking_every};
