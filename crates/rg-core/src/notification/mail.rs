//! Legacy digest compatibility and pending-bit drain.
//!
//! New thread notifications capture channel jobs in their publication commit.
//! Production uses only `drain_pending` here; all network delivery goes through
//! the durable channel dispatcher. Historical pending rows lack a captured
//! recipient, so they are retired with a safe outcome instead of redirected to
//! a potentially changed address. Their in-app notifications remain intact.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use crate::db::DbPools;
use anyhow::Result;

use rg_db::entities::notification::Model as Notification;
use rg_db::ops::{notification_ops, notification_setting_ops};

use super::thread::Reason;

/// How long the dispatcher collects after the first wake-up before it sends.
pub const BATCH_WINDOW: Duration = Duration::from_secs(30);

/// A pass also runs this often without a wake-up — rows left by a process
/// that stopped inside its window are sent by the next one.
const IDLE_PASS: Duration = Duration::from_secs(300);

/// Rows one pass reads at most; the rest wait for the next pass.
const PASS_LIMIT: u64 = 500;

/// Wake-ups for the dispatcher. Process-wide like the delivery tracker:
/// [`super::thread::deliver`] runs in whatever task the event happened in.
fn wakeups() -> &'static tokio::sync::Notify {
    static WAKEUPS: OnceLock<tokio::sync::Notify> = OnceLock::new();
    WAKEUPS.get_or_init(tokio::sync::Notify::new)
}

/// Tell the dispatcher that a row is waiting for its mail.
pub fn wake() {
    wakeups().notify_one();
}

/// One line of a mail.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DigestEntry {
    pub title: String,
    pub body: Option<String>,
    /// Absolute when the instance knows its public URL.
    pub url: Option<String>,
}

/// One mail to one person.
#[derive(Clone, Debug, PartialEq)]
pub struct Digest {
    pub subject: String,
    pub entries: Vec<DigestEntry>,
    /// Where the recipient turns these mails off.
    pub settings_url: Option<String>,
    // Internal source versions for the durable adapter; never rendered as mail.
    sources: Vec<Notification>,
}

/// Compatibility digest sink; production uses the non-network durable adapter.
pub trait Outbox: Send + Sync {
    fn send<'a>(
        &'a self,
        to: &'a str,
        mail: &'a Digest,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>>;
}

impl Outbox for crate::email::SmtpConfig {
    fn send<'a>(
        &'a self,
        to: &'a str,
        mail: &'a Digest,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(crate::email::send_digest(self, to, mail))
    }
}

fn absolute(base_url: Option<&str>, path: &str) -> Option<String> {
    base_url.map(|base| format!("{}{path}", base.trim_end_matches('/')))
}

/// The mail for one recipient's rows.
pub fn compose(rows: &[Notification], base_url: Option<&str>) -> Digest {
    let entries: Vec<DigestEntry> = rows
        .iter()
        .map(|row| DigestEntry {
            title: row.title.clone(),
            body: row.body.clone(),
            url: row
                .link
                .as_deref()
                .and_then(|link| absolute(base_url, link)),
        })
        .collect();
    let subject = match entries.as_slice() {
        [only] => format!("[Plombir Git] {}", only.title),
        many => format!("[Plombir Git] {} new notifications", many.len()),
    };
    Digest {
        subject,
        entries,
        settings_url: absolute(base_url, "/settings/notifications"),
        sources: rows.to_vec(),
    }
}

/// One pass: send every recipient the mail their pending rows add up to.
/// Returns how many mails went out.
pub async fn deliver_pending(
    pools: &impl DbPools,
    outbox: &dyn Outbox,
    base_url: Option<&str>,
) -> Result<usize> {
    let db = pools.read();
    let rows = notification_ops::list_email_pending(db, PASS_LIMIT).await?;
    let mut sent = 0;
    let mut start = 0;
    while start < rows.len() {
        let user_id = rows[start].user_id;
        let end = rows[start..]
            .iter()
            .position(|row| row.user_id != user_id)
            .map_or(rows.len(), |offset| start + offset);
        let batch = &rows[start..end];
        start = end;

        let settings = notification_setting_ops::get(db, user_id).await?;
        let wanted: Vec<Notification> = batch
            .iter()
            .filter(|row| {
                row.reason
                    .as_deref()
                    .and_then(Reason::parse)
                    .is_some_and(|reason| reason.mailed(&settings))
            })
            .cloned()
            .collect();
        let user = rg_db::ops::user_ops::find_by_id(db, user_id).await?;
        let address = user
            .filter(|user| user.is_active && user.deleted_at.is_none() && !user.is_bot())
            .map(|user| user.email)
            .filter(|email| !email.trim().is_empty());
        if let (false, Some(address)) = (wanted.is_empty(), address) {
            let digest = compose(&wanted, base_url);
            match outbox.send(&address, &digest).await {
                Ok(()) => sent += 1,
                Err(_) => {
                    tracing::warn!(
                        user_id,
                        notifications = wanted.len(),
                        "notification mail was not delivered"
                    );
                    continue;
                }
            }
        }
        notification_ops::clear_email_pending(pools.write(), batch).await?;
    }
    Ok(sent)
}

/// Drain the old pending marker without sending or recreating unknown targets.
/// The conditional clear cannot consume a concurrently folded thread version.
pub async fn drain_pending(db: &sea_orm::DatabaseConnection) -> Result<usize> {
    deliver_pending(db, &DurableOutbox(db), None).await
}

struct DurableOutbox<'a>(&'a sea_orm::DatabaseConnection);
impl Outbox for DurableOutbox<'_> {
    fn send<'a>(
        &'a self,
        _to: &'a str,
        mail: &'a Digest,
    ) -> Pin<Box<dyn Future<Output = Result<()>> + Send + 'a>> {
        Box::pin(async move { retire_sources(self.0, &mail.sources).await })
    }
}

async fn retire_sources(db: &sea_orm::DatabaseConnection, notes: &[Notification]) -> Result<()> {
    use rg_db::entities::notification_delivery as delivery;
    use rg_db::ops::notification_delivery_ops as queue;
    use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, Set, TransactionTrait};
    let ids: Vec<_> = notes.iter().map(queue::thread_event_id).collect();
    let existing: std::collections::HashSet<_> = delivery::Entity::find()
        .filter(delivery::Column::EventId.is_in(ids))
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.event_id)
        .collect();
    let tx = db.begin().await?;
    let now = chrono::Utc::now();
    for note in notes {
        let event_id = queue::thread_event_id(note);
        if !existing.contains(&event_id) {
            queue::insert(
                &tx,
                delivery::ActiveModel {
                    id: sea_orm::NotSet,
                    event_id: Set(event_id),
                    user_id: Set(note.user_id),
                    event_key: Set(note
                        .reason
                        .clone()
                        .unwrap_or_else(|| "participating".into())),
                    channel: Set("email".into()),
                    metadata: Set("{}".into()),
                    repo_id: Set(note.repo_id),
                    admin_only: Set(false),
                    generation: Set(0),
                    destination_hash: Set(String::new()),
                    previous_destination_encrypted: Set(None),
                    previous_token_encrypted: Set(None),
                    prior_locale: Set(None),
                    is_test: Set(false),
                    status: Set("cancelled".into()),
                    attempts: Set(0),
                    next_attempt_at: Set(now),
                    lease_token: Set(None),
                    lease_until: Set(None),
                    last_error: Set(Some("legacy_destination_unknown".into())),
                    created_at: Set(now),
                    expires_at: Set(now),
                    completed_at: Set(Some(now)),
                },
            )
            .await?;
        }
    }
    notification_ops::clear_email_pending(&tx, notes).await?;
    tx.commit().await?;
    Ok(())
}

/// Run the dispatcher until `shutdown` fires: a pass [`BATCH_WINDOW`] after a
/// wake-up, and one every [`IDLE_PASS`] regardless.
pub fn spawn_dispatcher(
    db: crate::db::Db,
    shutdown_rx: Option<tokio::sync::watch::Receiver<bool>>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut shutdown_rx = shutdown_rx;
        loop {
            let woken = tokio::select! {
                _ = wakeups().notified() => true,
                _ = tokio::time::sleep(IDLE_PASS) => false,
                _ = crate::task_tracker::wait_optional_shutdown(&mut shutdown_rx) => false,
            };
            if woken {
                tokio::select! {
                    _ = tokio::time::sleep(BATCH_WINDOW) => {}
                    _ = crate::task_tracker::wait_optional_shutdown(&mut shutdown_rx) => {}
                }
            }
            // One last pass on shutdown too: what is pending now would wait for
            // the next start otherwise.
            match drain_pending(db.write()).await {
                Ok(0) => {}
                Ok(rows) => tracing::debug!(rows, "legacy notification pending flags drained"),
                Err(_) => tracing::warn!("notification pending drain failed"),
            }
            if shutdown_rx
                .as_ref()
                .is_some_and(|rx| *rx.borrow() || rx.has_changed().is_err())
            {
                tracing::info!("notification mail dispatcher received shutdown, stopping");
                break;
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: i64, title: &str, link: &str) -> Notification {
        Notification {
            id,
            user_id: 1,
            event_type: "issue".into(),
            title: title.into(),
            body: Some("alice commented".into()),
            repo_id: Some(1),
            is_read: false,
            created_at: chrono::Utc::now(),
            reason: Some("participating".into()),
            subject_type: Some("issue".into()),
            subject_id: Some(id),
            link: Some(link.into()),
            updated_at: None,
            email_pending: true,
        }
    }

    #[test]
    fn one_row_is_its_own_subject_and_many_are_counted() {
        let one = compose(
            &[row(1, "o/r #1: Bug", "/o/r/issues/1")],
            Some("https://git.example/"),
        );
        assert_eq!(one.subject, "[Plombir Git] o/r #1: Bug");
        assert_eq!(
            one.entries[0].url.as_deref(),
            Some("https://git.example/o/r/issues/1")
        );
        assert_eq!(
            one.settings_url.as_deref(),
            Some("https://git.example/settings/notifications")
        );

        let many = compose(
            &[row(1, "a", "/o/r/issues/1"), row(2, "b", "/o/r/pulls/2")],
            None,
        );
        assert_eq!(many.subject, "[Plombir Git] 2 new notifications");
        assert!(many.entries.iter().all(|entry| entry.url.is_none()));
    }
}
