//! Retention for the tables that only ever grow: webhook deliveries,
//! notifications and login attempts.
//!
//! Each of them is written on every event — a delivery per hook per push, a
//! notification per watcher, a row per sign-in — and nothing used to delete a
//! row of any of them. The pages that show them read the latest few dozen; the
//! rest is disk, backup size and, for login attempts, a growing record of
//! client IP addresses nobody consults. This sweep keeps each table to a
//! window the operator chooses (`[retention]` in the config file).
//!
//! The sweep deletes in bounded batches: every batch is two short statements
//! (read up to `batch_size` ids, delete them by key), so no single statement
//! holds SQLite's write lock for the whole backlog, and the first pass after an
//! upgrade — which may find years of rows — interleaves with live writers
//! instead of stalling them.

use crate::task_tracker::wait_optional_shutdown;
use chrono::{DateTime, Duration, Utc};
use sea_orm::DatabaseConnection;
use tokio::sync::watch;
use tokio::time;

/// Days a webhook delivery is kept, without `[retention].webhook_delivery_days`.
pub const DEFAULT_WEBHOOK_DELIVERY_DAYS: i64 = 30;

/// Days a *read* notification is kept, without
/// `[retention].notification_read_days`.
pub const DEFAULT_NOTIFICATION_READ_DAYS: i64 = 90;

/// Days an *unread* notification is kept, without
/// `[retention].notification_unread_days`. Longer than the read window on
/// purpose: an unread row may be the only record of something its recipient
/// has not seen yet.
pub const DEFAULT_NOTIFICATION_UNREAD_DAYS: i64 = 365;

/// Days a login attempt is kept, without `[retention].login_log_days`.
pub const DEFAULT_LOGIN_LOG_DAYS: i64 = 180;

/// Minutes between sweeps, without `[retention].interval_minutes`.
pub const DEFAULT_INTERVAL_MINUTES: u64 = 60;

/// Rows deleted per statement, without `[retention].batch_size`.
pub const DEFAULT_BATCH_SIZE: u64 = 1_000;

/// The largest `batch_size` accepted. The batch's ids are bound into one
/// `DELETE … WHERE id IN (…)`, and this keeps that list far below every
/// backend's parameter ceiling (SQLite's 32 766 is the lowest).
pub const MAX_BATCH_SIZE: u64 = 10_000;

/// Delay before the first sweep after a start. Never during the first minute:
/// the process is still opening listeners, and the first pass after an upgrade
/// may have years of rows to work through.
const STARTUP_GRACE: std::time::Duration = std::time::Duration::from_secs(60);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RetentionConfig {
    pub webhook_delivery_days: i64,
    pub notification_read_days: i64,
    pub notification_unread_days: i64,
    pub login_log_days: i64,
    pub interval_minutes: u64,
    pub batch_size: u64,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            webhook_delivery_days: DEFAULT_WEBHOOK_DELIVERY_DAYS,
            notification_read_days: DEFAULT_NOTIFICATION_READ_DAYS,
            notification_unread_days: DEFAULT_NOTIFICATION_UNREAD_DAYS,
            login_log_days: DEFAULT_LOGIN_LOG_DAYS,
            interval_minutes: DEFAULT_INTERVAL_MINUTES,
            batch_size: DEFAULT_BATCH_SIZE,
        }
    }
}

impl RetentionConfig {
    /// Refuse a configuration the sweep cannot honour, at startup rather than
    /// an hour later inside the loop.
    pub fn validate(&self) -> anyhow::Result<()> {
        for (key, days) in [
            ("webhook_delivery_days", self.webhook_delivery_days),
            ("notification_read_days", self.notification_read_days),
            ("notification_unread_days", self.notification_unread_days),
            ("login_log_days", self.login_log_days),
        ] {
            anyhow::ensure!(
                days > 0,
                "[retention].{key} must be a positive number of days (got {days}); set \
                 `[retention].enabled = false` to keep every row instead"
            );
        }
        anyhow::ensure!(
            self.interval_minutes > 0,
            "[retention].interval_minutes must be positive"
        );
        anyhow::ensure!(
            (1..=MAX_BATCH_SIZE).contains(&self.batch_size),
            "[retention].batch_size must be between 1 and {MAX_BATCH_SIZE} (got {})",
            self.batch_size
        );
        Ok(())
    }
}

/// How many rows one pass removed from each table.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct RetentionReport {
    pub webhook_deliveries: u64,
    pub notifications: u64,
    pub login_logs: u64,
}

/// Run one batch function until a batch comes back short, yielding between
/// batches so a long backlog shares the runtime and the write lock.
async fn drain<F, Fut>(batch_size: u64, mut batch: F) -> anyhow::Result<u64>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<u64>>,
{
    let mut total = 0;
    loop {
        let deleted = batch().await?;
        total += deleted;
        if deleted < batch_size {
            return Ok(total);
        }
        tokio::task::yield_now().await;
    }
}

/// One full pass over the three tables, as of `now`.
///
/// A failed batch ends the pass with its error. What the earlier batches
/// deleted stays deleted, and the next pass picks up where this one stopped:
/// every batch is idempotent.
pub async fn run_once(
    db: &DatabaseConnection,
    config: &RetentionConfig,
    now: DateTime<Utc>,
) -> anyhow::Result<RetentionReport> {
    let batch = config.batch_size;
    let webhook_cutoff = now - Duration::days(config.webhook_delivery_days);
    let read_cutoff = now - Duration::days(config.notification_read_days);
    let unread_cutoff = now - Duration::days(config.notification_unread_days);
    let login_cutoff = now - Duration::days(config.login_log_days);

    let webhook_deliveries = drain(batch, || {
        rg_db::ops::webhook_ops::delete_deliveries_before(db, webhook_cutoff, batch)
    })
    .await?;
    let notifications = drain(batch, || {
        rg_db::ops::notification_ops::delete_stale(db, read_cutoff, unread_cutoff, batch)
    })
    .await?;
    let login_logs = drain(batch, || async move {
        Ok(rg_db::ops::login_log_ops::delete_before(db, login_cutoff, batch).await?)
    })
    .await?;

    Ok(RetentionReport {
        webhook_deliveries,
        notifications,
        login_logs,
    })
}

/// Start the periodic sweep. Validates the configuration first, so a value the
/// sweep cannot honour fails the server start.
pub fn spawn_retention_with_shutdown(
    db: DatabaseConnection,
    config: RetentionConfig,
    shutdown_rx: Option<watch::Receiver<bool>>,
) -> anyhow::Result<tokio::task::JoinHandle<()>> {
    config.validate()?;
    let period = std::time::Duration::from_secs(config.interval_minutes * 60);
    Ok(tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        let mut interval = time::interval_at(time::Instant::now() + STARTUP_GRACE, period);
        loop {
            tokio::select! {
                _ = interval.tick() => {}
                _ = wait_optional_shutdown(&mut shutdown_rx) => {
                    tracing::info!("retention sweep received shutdown, stopping");
                    break;
                }
            }
            match run_once(&db, &config, Utc::now()).await {
                Ok(report) if report == RetentionReport::default() => {
                    tracing::debug!("retention sweep: nothing past its window");
                }
                Ok(report) => tracing::info!(
                    webhook_deliveries = report.webhook_deliveries,
                    notifications = report.notifications,
                    login_logs = report.login_logs,
                    "retention sweep removed rows past their window"
                ),
                // `{:#}` unwinds the whole chain; this warning is the only
                // channel between a sweep that stopped working and the disk.
                Err(error) => {
                    tracing::warn!("retention sweep failed, the next pass retries: {error:#}")
                }
            }
        }
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::migrated_memory_database;
    use rg_db::entities::{login_log, notification, webhook_delivery};
    use sea_orm::{ActiveModelTrait, EntityTrait, PaginatorTrait, Set};

    fn config(batch_size: u64) -> RetentionConfig {
        RetentionConfig {
            batch_size,
            ..RetentionConfig::default()
        }
    }

    async fn user(db: &DatabaseConnection, name: &str) -> i64 {
        rg_db::ops::user_ops::create_user(db, name, &format!("{name}@example.test"), "", "")
            .await
            .expect("create user")
            .id
    }

    async fn delivery(db: &DatabaseConnection, webhook_id: i64, at: DateTime<Utc>) -> i64 {
        webhook_delivery::ActiveModel {
            webhook_id: Set(webhook_id),
            event: Set("push".to_string()),
            delivery_id: Set(uuid::Uuid::new_v4().to_string()),
            response_status: Set(Some(200)),
            request_payload: Set(Some("{}".to_string())),
            response_body: Set(None),
            duration_ms: Set(Some(1)),
            created_at: Set(at),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert delivery")
        .id
    }

    async fn webhook(db: &DatabaseConnection, owner: i64) -> i64 {
        let repo = rg_db::ops::repo_ops::create(
            db,
            rg_db::entities::repository::ActiveModel {
                owner_id: Set(owner),
                name: Set("retained".to_string()),
                is_private: Set(false),
                default_branch: Set("main".to_string()),
                stars_count: Set(0),
                forks_count: Set(0),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            },
        )
        .await
        .expect("create repo");
        rg_db::ops::webhook_ops::create_webhook(
            db,
            rg_db::entities::webhook::ActiveModel {
                repo_id: Set(repo.id),
                url: Set("https://example.test/hook".to_string()),
                content_type: Set("json".to_string()),
                secret_encrypted: Set(None),
                active: Set(true),
                events: Set("push".to_string()),
                created_at: Set(Utc::now()),
                updated_at: Set(Utc::now()),
                ..Default::default()
            },
        )
        .await
        .expect("create webhook")
        .id
    }

    #[allow(clippy::too_many_arguments)]
    async fn notification(
        db: &DatabaseConnection,
        user_id: i64,
        is_read: bool,
        email_pending: bool,
        created_at: DateTime<Utc>,
        updated_at: Option<DateTime<Utc>>,
    ) -> i64 {
        notification::ActiveModel {
            user_id: Set(user_id),
            event_type: Set("push".to_string()),
            title: Set("t".to_string()),
            body: Set(None),
            repo_id: Set(None),
            is_read: Set(is_read),
            created_at: Set(created_at),
            updated_at: Set(updated_at),
            email_pending: Set(email_pending),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert notification")
        .id
    }

    async fn login(db: &DatabaseConnection, at: DateTime<Utc>) -> i64 {
        login_log::ActiveModel {
            user_id: Set(None),
            username: Set("someone".to_string()),
            auth_provider: Set("local".to_string()),
            ip_address: Set(Some("192.0.2.1".to_string())),
            user_agent: Set(None),
            success: Set(false),
            failure_reason: Set(None),
            created_at: Set(at),
            ..Default::default()
        }
        .insert(db)
        .await
        .expect("insert login log")
        .id
    }

    async fn ids<E>(db: &DatabaseConnection) -> Vec<i64>
    where
        E: EntityTrait,
        E::Model: HasId,
    {
        let mut ids: Vec<i64> = E::find()
            .all(db)
            .await
            .expect("list rows")
            .iter()
            .map(HasId::id)
            .collect();
        ids.sort_unstable();
        ids
    }

    trait HasId {
        fn id(&self) -> i64;
    }
    impl HasId for webhook_delivery::Model {
        fn id(&self) -> i64 {
            self.id
        }
    }
    impl HasId for notification::Model {
        fn id(&self) -> i64 {
            self.id
        }
    }
    impl HasId for login_log::Model {
        fn id(&self) -> i64 {
            self.id
        }
    }

    /// Each table loses exactly the rows past its own window — and the
    /// notification rules keep what still has a use: an unread row inside the
    /// longer unread window, a row a recent event folded into, and any row the
    /// mail dispatcher still owes a message for.
    #[tokio::test]
    async fn a_pass_removes_exactly_what_is_past_each_window() {
        let db = migrated_memory_database().await;
        let now = Utc::now();
        let days = |n: i64| now - Duration::days(n);
        let owner = user(&db, "retention-owner").await;
        let hook = webhook(&db, owner).await;

        let old_delivery = delivery(&db, hook, days(DEFAULT_WEBHOOK_DELIVERY_DAYS + 1)).await;
        let fresh_delivery = delivery(&db, hook, days(DEFAULT_WEBHOOK_DELIVERY_DAYS - 1)).await;

        let read_old = notification(&db, owner, true, false, days(100), None).await;
        let read_fresh = notification(&db, owner, true, false, days(10), None).await;
        let unread_inside_window = notification(&db, owner, false, false, days(100), None).await;
        let unread_old = notification(&db, owner, false, false, days(400), None).await;
        let folded_recently = notification(&db, owner, true, false, days(100), Some(days(1))).await;
        let mail_owed = notification(&db, owner, true, true, days(400), None).await;

        let login_old = login(&db, days(DEFAULT_LOGIN_LOG_DAYS + 1)).await;
        let login_fresh = login(&db, days(1)).await;

        let report = run_once(&db, &config(DEFAULT_BATCH_SIZE), now)
            .await
            .expect("retention pass");
        assert_eq!(
            report,
            RetentionReport {
                webhook_deliveries: 1,
                notifications: 2,
                login_logs: 1,
            }
        );

        assert_eq!(
            ids::<webhook_delivery::Entity>(&db).await,
            vec![fresh_delivery]
        );
        assert!(!ids::<webhook_delivery::Entity>(&db)
            .await
            .contains(&old_delivery));
        let mut kept = vec![read_fresh, unread_inside_window, folded_recently, mail_owed];
        kept.sort_unstable();
        assert_eq!(ids::<notification::Entity>(&db).await, kept);
        assert!(!kept.contains(&read_old) && !kept.contains(&unread_old));
        assert_eq!(ids::<login_log::Entity>(&db).await, vec![login_fresh]);
        assert_ne!(login_old, login_fresh);
    }

    /// A backlog larger than one batch is drained in one pass, batch by batch:
    /// the batch bounds each statement, not how much a pass may remove.
    #[tokio::test]
    async fn a_backlog_larger_than_a_batch_is_drained_in_bounded_batches() {
        let db = migrated_memory_database().await;
        let now = Utc::now();
        for _ in 0..7 {
            login(&db, now - Duration::days(DEFAULT_LOGIN_LOG_DAYS + 5)).await;
        }
        let fresh = login(&db, now).await;

        let report = run_once(&db, &config(3), now)
            .await
            .expect("retention pass");
        assert_eq!(report.login_logs, 7);
        assert_eq!(ids::<login_log::Entity>(&db).await, vec![fresh]);
        assert_eq!(
            login_log::Entity::find().count(&db).await.expect("count"),
            1
        );
    }

    #[test]
    fn configuration_the_sweep_cannot_honour_is_refused() {
        assert!(RetentionConfig::default().validate().is_ok());
        for broken in [
            RetentionConfig {
                login_log_days: 0,
                ..RetentionConfig::default()
            },
            RetentionConfig {
                notification_unread_days: -1,
                ..RetentionConfig::default()
            },
            RetentionConfig {
                interval_minutes: 0,
                ..RetentionConfig::default()
            },
            RetentionConfig {
                batch_size: 0,
                ..RetentionConfig::default()
            },
            RetentionConfig {
                batch_size: MAX_BATCH_SIZE + 1,
                ..RetentionConfig::default()
            },
        ] {
            assert!(broken.validate().is_err(), "{broken:?} must be refused");
        }
    }
}
