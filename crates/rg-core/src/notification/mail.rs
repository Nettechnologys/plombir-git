//! Mailing notifications, batched per person (card_349c2b6a0d7c).
//!
//! [`super::thread::deliver`] never sends mail itself. It marks a notification
//! row `email_pending` when its recipient asked to be mailed about that kind of
//! thing, and wakes the dispatcher. The dispatcher waits [`BATCH_WINDOW`] —
//! the review request, the comment and the CI failure of one push land in that
//! window together — and then sends each recipient one mail with everything
//! that piled up for them.
//!
//! The recipient's settings are read again at send time: turning a category
//! off stops the mail that is still waiting, not only the next one.
//!
//! A row is cleared whether or not its mail went out. A failed SMTP round trip
//! is logged; retrying it every pass would turn an outage into a storm of the
//! same mails once the server comes back, and the notification itself is still
//! in the inbox.

use std::future::Future;
use std::pin::Pin;
use std::sync::OnceLock;
use std::time::Duration;

use anyhow::Result;
use sea_orm::DatabaseConnection;

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
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Digest {
    pub subject: String,
    pub entries: Vec<DigestEntry>,
    /// Where the recipient turns these mails off.
    pub settings_url: Option<String>,
}

/// Where mails go — SMTP in production, a recorder in tests.
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
    }
}

/// One pass: send every recipient the mail their pending rows add up to.
/// Returns how many mails went out.
pub async fn deliver_pending(
    db: &DatabaseConnection,
    outbox: &dyn Outbox,
    base_url: Option<&str>,
) -> Result<usize> {
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
        let ids: Vec<i64> = batch.iter().map(|row| row.id).collect();

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
                Err(error) => tracing::warn!(
                    user_id,
                    notifications = wanted.len(),
                    error = %format!("{error:#}"),
                    "notification mail was not delivered"
                ),
            }
        }
        notification_ops::clear_email_pending(db, &ids).await?;
    }
    Ok(sent)
}

/// Run the dispatcher until `shutdown` fires: a pass [`BATCH_WINDOW`] after a
/// wake-up, and one every [`IDLE_PASS`] regardless.
pub fn spawn_dispatcher(
    db: DatabaseConnection,
    smtp: crate::email::SmtpConfig,
    base_url: Option<String>,
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
            match deliver_pending(&db, &smtp, base_url.as_deref()).await {
                Ok(0) => {}
                Ok(mails) => tracing::debug!(mails, "notification mails sent"),
                Err(error) => tracing::warn!(
                    error = %format!("{error:#}"),
                    "notification mail pass failed"
                ),
            }
            if shutdown_rx.as_ref().is_some_and(|rx| *rx.borrow()) {
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
