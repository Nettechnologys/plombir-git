//! ForgeKeep CI runner agent — the register / poll / execute implementation.
//!
//! The crate ships both a library and the `forgekeep-runner` binary on purpose:
//! `forgekeep runner` (the deprecated subcommand of the main `forgekeep` binary)
//! used to carry its **own**, much older copy of this loop, which drifted badly —
//! it never read `runner.toml`, so it registered a fresh runner on every start,
//! and it also lacked the heartbeat, the per-job workspace snapshot, the job
//! timeout and the cache round-trip. Exposing the real implementation here lets
//! that subcommand delegate instead of duplicate, so there is exactly one runner
//! behaviour to reason about.

mod api;
mod commands;
mod config;
mod executor;

pub use commands::{cmd_register, cmd_run};
