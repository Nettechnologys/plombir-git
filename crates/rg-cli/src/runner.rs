//! `plombir-git runner` — a deprecated alias for `plombir-git-runner run`.
//!
//! This module used to hold a **second**, independent runner implementation
//! alongside the `rg-runner` crate, and the two had drifted apart badly:
//!
//!  - it knew nothing about `runner.toml` (no `--config`, no read of the file
//!    `plombir-git-runner register --save` writes), so every start auto-registered
//!    a *new* runner, printed `Save these credentials for future runs!` and left
//!    persistence to the operator's copy-paste — the server slowly filled with
//!    dead duplicate runner rows and tokens;
//!  - it never sent a heartbeat, so the server saw the runner as offline;
//!  - it executed job scripts in whatever directory the runner happened to be
//!    started in, instead of the per-job workspace snapshot of the assigned
//!    commit;
//!  - it had no job timeout and no cache round-trip.
//!
//! Two runners with different behaviour behind two spellings of the same command
//! is the defect, so the alias now delegates to the real agent rather than
//! reimplementing it. Everything below is argument plumbing plus one deprecation
//! warning.

/// Default path of the runner config file, mirroring `plombir-git-runner`'s own
/// `--config` default so both spellings of the command read and write the same
/// file. Lives here (not inline in [`crate::cli`]) so the two stay in one place.
pub(crate) const DEFAULT_RUNNER_CONFIG: &str = "~/.plombir-git/runner.toml";

/// Deprecation notice printed before the runner starts.
///
/// It names the replacement command *and* the reason, because the alias is not
/// merely renamed: `plombir-git-runner` is the binary that gets new runner
/// features, and it is what `deploy/docker-compose.yml`, the README and
/// `ARCHITECTURE.md` all document.
const DEPRECATION_NOTICE: &str = "`plombir-git runner` is deprecated and will be removed in a \
     future release — use `plombir-git-runner run` instead (same flags). This alias now delegates \
     to it, so behaviour is identical.";

/// Run as a CI runner by delegating to `plombir-git-runner run`.
pub(crate) async fn cmd_runner(command: rg_runner::RunCommand) -> anyhow::Result<()> {
    // The delegate reports through `tracing` (config-file diagnostics, a failed
    // config save, poll errors); without a subscriber those would go nowhere,
    // and "the runner silently ignores my config" is exactly the failure mode
    // this alias existed to produce.
    crate::commands::init_cli_logging();
    tracing::warn!("{}", DEPRECATION_NOTICE);

    rg_runner::cmd_run(command).await
}
