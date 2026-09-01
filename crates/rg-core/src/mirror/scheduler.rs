//! Mirror sync scheduler — the tick that makes `mirrors.next_sync_at` mean
//! something.
//!
//! Without it the periodic half of the mirror feature was wiring on paper only
//! (card_d2fd29942436): `POST .../mirror` accepted `sync_interval_seconds` and
//! wrote `next_sync_at`, the settings UI rendered both, `mirror_ops::list_due_sync`
//! existed for exactly this sweep — and [`sync_due_mirrors`] had no caller
//! anywhere in the process. The only thing that ever refreshed a mirror was
//! someone pressing "Sync now", which also happened to be the only thing that
//! ever moved `next_sync_at`, so the field an operator reads as "next sync at
//! …" described a moment at which nothing was scheduled to happen.
//!
//! [`sync_due_mirrors`]: crate::mirror::service::sync_due_mirrors
//!
//! The shape follows [`crate::audit::archiver`] and [`crate::backup`]: one
//! long-lived loop, one pass per poll interval, and an exit at the next idle
//! point when `shutdown_rx` flips rather than an abort mid-run. The one
//! difference is where the work itself runs — each pass is spawned through
//! [`delivery_tracker`] and awaited, so a `git clone --mirror` that a `SIGTERM`
//! catches in flight is drained within the shutdown grace window instead of
//! being severed when the runtime is torn down.

use crate::task_tracker::{delivery_tracker, wait_optional_shutdown};
use sea_orm::DatabaseConnection;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time;

/// Seconds between two passes over the due-mirror list.
///
/// This is the *polling* granularity, not a mirror's schedule: each mirror
/// carries its own `sync_interval_seconds`, and a pass only touches the rows
/// whose `next_sync_at` has already passed. A minute is fine-grained enough
/// that the shortest interval an operator can set through the UI (one hour) is
/// honoured to within ~1%, and coarse enough that an idle instance costs one
/// indexed query per minute.
pub const DEFAULT_POLL_INTERVAL_SECS: u64 = 60;

/// Upper bound on mirrors refreshed in a single pass.
///
/// A pass is sequential and each mirror means a `git` subprocess against a
/// third-party remote, so the bound is what stops an instance with a hundred
/// due mirrors from turning one tick into an hour of back-to-back fetches. The
/// leftovers are still due on the next pass.
pub const DEFAULT_BATCH_SIZE: u64 = 10;

/// Tuning for [`spawn_mirror_sync_with_shutdown`].
#[derive(Clone, Debug)]
pub struct MirrorSyncConfig {
    /// Seconds between passes over the due-mirror list.
    pub poll_interval_secs: u64,
    /// Maximum mirrors refreshed per pass.
    pub batch_size: u64,
    /// Instance-owned exception for plaintext HTTP mirror remotes. Native
    /// `git://` remains disabled regardless of this policy value.
    pub transport_policy: super::transport::MirrorTransportPolicy,
}

impl Default for MirrorSyncConfig {
    fn default() -> Self {
        Self {
            poll_interval_secs: DEFAULT_POLL_INTERVAL_SECS,
            batch_size: DEFAULT_BATCH_SIZE,
            transport_policy: Default::default(),
        }
    }
}

impl MirrorSyncConfig {
    fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            self.poll_interval_secs > 0,
            "`[mirror].poll_interval_secs` must be positive (a zero interval is a busy loop, \
             not a schedule); set `[mirror].enabled = false` to turn scheduled mirror sync off"
        );
        anyhow::ensure!(
            self.batch_size > 0,
            "`[mirror].batch_size` must be positive; a batch of 0 syncs nothing forever — set \
             `[mirror].enabled = false` to turn scheduled mirror sync off"
        );
        Ok(())
    }
}

/// Start the background mirror sync loop, optionally wired to a graceful
/// shutdown signal.
///
/// The numeric knobs are range-checked here, so a `poll_interval_secs = 0` in
/// the config file fails the server start rather than becoming a hot loop that
/// nobody notices until the CPU graph does.
pub fn spawn_mirror_sync_with_shutdown(
    db: DatabaseConnection,
    repo_root: PathBuf,
    encryption_key: String,
    config: MirrorSyncConfig,
    shutdown_rx: Option<watch::Receiver<bool>>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    config.validate()?;
    let period = Duration::from_secs(config.poll_interval_secs);
    let batch_size = config.batch_size;
    let transport_policy = config.transport_policy;

    Ok(tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        // The first pass is a full period in, not immediate: a mirror that is
        // due at startup is still due a minute later, and a process that is
        // still opening its listeners should not simultaneously be cloning ten
        // third-party remotes. Same reasoning as `backup::STARTUP_GRACE`.
        let mut interval = time::interval_at(time::Instant::now() + period, period);
        // A pass can outlast its own period — each mirror is a `git` subprocess
        // bounded only by the git gateway's timeout. Skip the ticks that piled
        // up meanwhile instead of immediately running the same sweep again.
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);

        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = wait_optional_shutdown(&mut shutdown_rx) => {
                    tracing::info!("mirror sync scheduler received shutdown, stopping");
                    break;
                }
            }

            // Through the tracker, and awaited: awaiting keeps passes from
            // overlapping (two `git remote update`s in the same mirror
            // directory is a corrupt working copy), while the tracker is what
            // gives an in-flight fetch the shutdown grace window to finish in
            // instead of dying with the runtime.
            let pass = delivery_tracker().spawn({
                let db = db.clone();
                let repo_root = repo_root.clone();
                let encryption_key = encryption_key.clone();
                async move {
                    crate::mirror::service::sync_due_mirrors(
                        &db,
                        &repo_root,
                        batch_size,
                        transport_policy,
                        &encryption_key,
                    )
                    .await
                }
            });

            match pass.await {
                Ok(Ok(0)) => {}
                Ok(Ok(count)) => tracing::info!(count, "scheduled mirror sync refreshed mirrors"),
                // Per-mirror failures never get here — `sync_due_mirrors`
                // records those on the row and carries on. This is the listing
                // query itself failing, which means *no* mirror is being
                // refreshed, so say that rather than logging a bare error.
                Ok(Err(error)) => tracing::warn!(
                    "scheduled mirror sync could not list due mirrors, no mirror is being \
                     refreshed: {error:#}"
                ),
                Err(join_error) => {
                    tracing::error!(%join_error, "scheduled mirror sync pass did not complete")
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A zero interval or a zero batch is a config typo that would otherwise
    /// become a silent no-op (batch) or a busy loop (interval), so it has to
    /// fail the *spawn* — the same rule the audit archiver follows.
    #[test]
    fn a_zero_knob_refuses_to_start_the_scheduler() {
        for config in [
            MirrorSyncConfig {
                poll_interval_secs: 0,
                ..Default::default()
            },
            MirrorSyncConfig {
                batch_size: 0,
                ..Default::default()
            },
        ] {
            let error = config
                .validate()
                .expect_err("a zero knob must not produce a scheduler");
            assert!(
                format!("{error:#}").contains("[mirror]"),
                "the failure has to name the config key: {error:#}"
            );
        }
    }

    #[test]
    fn the_defaults_are_a_usable_schedule() {
        MirrorSyncConfig::default()
            .validate()
            .expect("the built-in defaults must be valid");
    }
}
