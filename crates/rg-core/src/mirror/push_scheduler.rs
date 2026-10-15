//! The loop that sends due push mirrors out.
//!
//! Same shape as [`crate::mirror::scheduler`] — one long-lived loop, passes awaited
//! through [`crate::task_tracker::delivery_tracker`] so a `git push` that a
//! `SIGTERM` catches in flight gets the shutdown grace window — with one
//! difference: besides its tick it wakes when a ref move or "Sync now" made a
//! mirror due, so a push reaches the remote seconds after it reached this
//! server rather than up to a poll interval later.
//!
//! It does not follow `[mirror].enabled`, which switches off the scheduled
//! *pull* of upstreams: a push mirror is the owner's copy of their own work,
//! and on-commit pushes are not a schedule.

use crate::task_tracker::{delivery_tracker, wait_optional_shutdown};
use sea_orm::DatabaseConnection;
use std::path::PathBuf;
use std::time::Duration;
use tokio::sync::watch;
use tokio::time;

/// Seconds between two sweeps when nothing woke the loop: the interval-only
/// mirrors and the retries of failed passes are picked up here.
pub const POLL_INTERVAL_SECS: u64 = 60;

/// Mirrors pushed in one pass. The leftovers are still due on the next one,
/// which a busy instance reaches right away: the loop runs again as soon as a
/// pass ends with mirrors still due.
pub const BATCH_SIZE: u64 = 10;

/// Start the push mirror loop.
pub fn spawn_push_mirrors_with_shutdown(
    db: DatabaseConnection,
    repo_root: PathBuf,
    encryption_key: String,
    transport_policy: super::transport::MirrorTransportPolicy,
    shutdown_rx: Option<watch::Receiver<bool>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        let period = Duration::from_secs(POLL_INTERVAL_SECS);
        let mut interval = time::interval_at(time::Instant::now() + period, period);
        interval.set_missed_tick_behavior(time::MissedTickBehavior::Skip);
        let mut more_due = false;

        loop {
            if more_due {
                if shutdown_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
                    tracing::info!("push mirror scheduler received shutdown, stopping");
                    break;
                }
            } else {
                tokio::select! {
                    _ = interval.tick() => {}
                    _ = super::push::woken() => {}
                    _ = wait_optional_shutdown(&mut shutdown_rx) => {
                        tracing::info!("push mirror scheduler received shutdown, stopping");
                        break;
                    }
                }
            }

            let pass = delivery_tracker().spawn({
                let db = db.clone();
                let repo_root = repo_root.clone();
                let encryption_key = encryption_key.clone();
                async move {
                    super::push::push_due_mirrors(
                        &db,
                        &repo_root,
                        BATCH_SIZE,
                        transport_policy,
                        &encryption_key,
                    )
                    .await
                }
            });
            more_due = match pass.await {
                Ok(Ok(ran)) => {
                    if ran > 0 {
                        tracing::info!(count = ran, "push mirror pass pushed mirrors");
                    }
                    ran as u64 >= BATCH_SIZE
                }
                Ok(Err(error)) => {
                    tracing::warn!(
                        "push mirror pass could not list due mirrors, no mirror is being pushed: \
                         {error:#}"
                    );
                    false
                }
                Err(join_error) => {
                    tracing::error!(%join_error, "push mirror pass did not complete");
                    false
                }
            };
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rg_db::entities::push_mirror::ActiveModel;
    use rg_db::entities::repository;
    use sea_orm::{NotSet, Set};

    /// The loop's whole reason to exist beside its tick: a ref move wakes it,
    /// so the pass runs in seconds, not up to a poll interval later. The mirror
    /// points at an address the SSRF guard refuses, so the pass ends at once
    /// with a recorded failure and nothing leaves the machine.
    #[tokio::test]
    async fn a_ref_move_wakes_the_loop_well_before_its_tick() {
        let db = crate::test_support::migrated_memory_database().await;
        let owner = rg_db::ops::user_ops::create_user(
            &db,
            "push-loop-owner",
            "push-loop-owner@example.invalid",
            "",
            "Owner",
        )
        .await
        .unwrap();
        let now = chrono::Utc::now();
        let repo = rg_db::ops::repo_ops::create(
            &db,
            repository::ActiveModel {
                id: NotSet,
                owner_id: Set(owner.id),
                name: Set("looped".to_string()),
                description: Set(None),
                is_private: Set(true),
                default_branch: Set("main".to_string()),
                fork_id: Set(None),
                stars_count: Set(0),
                forks_count: Set(0),
                org_id: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                deleted_at: Set(None),
                origin_repo_id: Set(None),
            },
        )
        .await
        .unwrap();
        let mirror = rg_db::ops::push_mirror_ops::create(
            &db,
            ActiveModel {
                repo_id: Set(repo.id),
                url: Set("https://127.0.0.1:9/target.git".to_string()),
                username: Set(None),
                password_encrypted: Set(None),
                sync_on_commit: Set(true),
                sync_interval_seconds: Set(0),
                force: Set(false),
                enabled: Set(true),
                next_sync_at: Set(None),
                pending_since: Set(None),
                pending_revision: Set(0),
                last_sync_at: Set(None),
                last_success_at: Set(None),
                last_sync_error: Set(None),
                lease_token: Set(None),
                lease_acquired_at: Set(None),
                created_at: Set(now),
                updated_at: Set(now),
                ..Default::default()
            },
        )
        .await
        .unwrap();
        let repo_root = tempfile::tempdir().unwrap();
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let handle = spawn_push_mirrors_with_shutdown(
            db.clone(),
            repo_root.path().to_path_buf(),
            "push-loop-key".to_string(),
            Default::default(),
            Some(shutdown_rx),
        );

        super::super::push::after_ref_move(&db, repo.id).await;
        let attempted = tokio::time::timeout(Duration::from_secs(15), async {
            loop {
                let row = rg_db::ops::push_mirror_ops::find_for_repo(&db, repo.id, mirror.id)
                    .await
                    .unwrap()
                    .unwrap();
                if row.last_sync_at.is_some() {
                    return row;
                }
                time::sleep(Duration::from_millis(50)).await;
            }
        })
        .await
        .expect("the loop slept through the wake-up; the push would wait for the next tick");
        assert!(attempted.last_sync_error.is_some(), "{attempted:?}");

        shutdown_tx.send(true).unwrap();
        tokio::time::timeout(Duration::from_secs(10), handle)
            .await
            .expect("the loop did not stop on shutdown")
            .unwrap();
    }
}
