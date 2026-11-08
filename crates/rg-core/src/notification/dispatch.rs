//! Durable post-commit delivery. Every attempt rechecks the authoritative
//! account, destination and repository permissions; transport errors are finite
//! codes only. A remote accept followed by a crash can still duplicate delivery.
use super::{settings, telegram, template};
use anyhow::Result;
use rg_db::{
    entities::{notification_delivery, user},
    notification::{Event, Locale, Metadata},
    ops::notification_delivery_ops as queue,
};
use sea_orm::{DatabaseConnection, EntityTrait};
use std::{future::Future, pin::Pin, time::Duration};

pub enum Target {
    Email(String),
    Telegram(telegram::Destination),
}

#[derive(Clone, Copy)]
pub struct Failure {
    pub code: &'static str,
    pub retry_after: Option<Duration>,
}

impl From<telegram::Error> for Failure {
    fn from(error: telegram::Error) -> Self {
        Self {
            code: error.code(),
            retry_after: error.retry_after(),
        }
    }
}

pub trait Sender: Send + Sync {
    fn send<'a>(
        &'a self,
        target: &'a Target,
        message: &'a template::Message,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), Failure>> + Send + 'a>>;
}

pub struct Transport {
    telegram: telegram::Client,
    smtp: Option<crate::email::SmtpConfig>,
}

impl Transport {
    pub fn new(smtp: Option<crate::email::SmtpConfig>) -> Result<Self, telegram::Error> {
        Ok(Self {
            telegram: telegram::Client::new()?,
            smtp,
        })
    }
}

impl Sender for Transport {
    fn send<'a>(
        &'a self,
        target: &'a Target,
        message: &'a template::Message,
    ) -> Pin<Box<dyn Future<Output = std::result::Result<(), Failure>> + Send + 'a>> {
        Box::pin(async move {
            match target {
                Target::Telegram(destination) => self
                    .telegram
                    .send(destination, &message.text)
                    .await
                    .map_err(Failure::from),
                Target::Email(address) => {
                    let Some(smtp) = &self.smtp else {
                        return Err(Failure {
                            code: "smtp_unavailable",
                            retry_after: None,
                        });
                    };
                    match tokio::time::timeout(
                        Duration::from_secs(10),
                        crate::email::send_notification_email(
                            smtp,
                            address,
                            &message.subject,
                            &message.html,
                        ),
                    )
                    .await
                    {
                        Ok(Ok(())) => Ok(()),
                        // Never retain or print lettre's error: it can include
                        // an address/server reply. Bound all SMTP retries too.
                        _ => Err(Failure {
                            code: "smtp_delivery_failed",
                            retry_after: Some(Duration::from_secs(30)),
                        }),
                    }
                }
            }
        })
    }
}

pub struct Config<'a> {
    pub encryption_key: &'a str,
    pub base_url: Option<&'a str>,
    pub smtp_available: bool,
}

enum Prepared {
    Send(Target, template::Message),
    Stop(&'static str, &'static str),
}

fn decrypt(value: Option<&str>, key: &[u8; 32]) -> Option<String> {
    value.and_then(|value| crate::auth::encryption::decrypt(value, key).ok())
}

async fn prepare(
    db: &DatabaseConnection,
    held: &notification_delivery::Model,
    config: &Config<'_>,
) -> Result<Prepared> {
    let Some(user) = user::Entity::find_by_id(held.user_id)
        .one(db)
        .await?
        .filter(|u| u.is_usable() && !u.is_bot())
    else {
        return Ok(Prepared::Stop("cancelled", "account_unavailable"));
    };
    let Some(event) = Event::parse(&held.event_key) else {
        return Ok(Prepared::Stop("failed", "unknown_event"));
    };
    let prior = held.previous_destination_encrypted.is_some();
    if prior
        && (event != Event::NotificationSettingsChanged || held.is_test || held.repo_id.is_some())
    {
        return Ok(Prepared::Stop("failed", "invalid_prior_notice"));
    }
    if let Some(source) = held.event_id.strip_prefix("thread:") {
        let id = source
            .split(':')
            .next()
            .and_then(|id| id.parse::<i64>().ok());
        let note = if let Some(id) = id {
            rg_db::entities::notification::Entity::find_by_id(id)
                .one(db)
                .await?
        } else {
            None
        };
        if !note.is_some_and(|note| {
            note.user_id == held.user_id && queue::thread_event_id(&note) == held.event_id
        }) {
            return Ok(Prepared::Stop("cancelled", "thread_removed_or_superseded"));
        }
    }
    if let Some(source) = held.event_id.strip_prefix("pipeline:") {
        let id = source
            .split(':')
            .next()
            .and_then(|id| id.parse::<i64>().ok());
        let pipeline = if let Some(id) = id {
            rg_db::ops::pipeline_ops::get_pipeline(db, id).await?
        } else {
            None
        };
        if !pipeline.is_some_and(|pipeline| {
            Some(pipeline.repo_id) == held.repo_id && pipeline.status != "cancelled"
        }) {
            return Ok(Prepared::Stop("cancelled", "pipeline_unavailable"));
        }
    }
    if let Some(repo_id) = held.repo_id {
        let Some(repo) = rg_db::ops::repo_ops::find_by_id(db, repo_id).await? else {
            return Ok(Prepared::Stop("cancelled", "repository_unavailable"));
        };
        // readers_among bypasses the normal permission cache deliberately.
        let allowed = if held.admin_only {
            crate::repo::service::can_admin_repo(db, &repo, Some(user.id)).await?
        } else {
            crate::repo::service::readers_among(db, &repo, &[user.id])
                .await?
                .contains(&user.id)
        };
        if !allowed {
            return Ok(Prepared::Stop("cancelled", "repository_access_removed"));
        }
    } else if held.admin_only && !user.is_admin {
        return Ok(Prepared::Stop("cancelled", "administrator_access_removed"));
    }
    let current = settings::get(db, user.id).await?;
    let channels = current.events[&event];
    if !prior {
        if (held.channel == "email" && !channels.email)
            || (held.channel == "telegram" && !channels.telegram)
        {
            return Ok(Prepared::Stop("cancelled", "channel_disabled"));
        }
        if held.channel == "email" && held.destination_hash != rg_db::token_digest(&user.email) {
            return Ok(Prepared::Stop("cancelled", "destination_changed"));
        }
        if held.channel == "telegram" && held.generation != current.profile.generation {
            return Ok(Prepared::Stop("cancelled", "destination_changed"));
        }
    }
    let key = crate::auth::encryption::derive_key(config.encryption_key);
    let previous = if prior {
        let Some(json) = decrypt(held.previous_destination_encrypted.as_deref(), &key) else {
            return Ok(Prepared::Stop("failed", "encryption_unavailable"));
        };
        match serde_json::from_str::<settings::PreviousDestination>(&json) {
            Ok(value) => Some(value),
            Err(_) => return Ok(Prepared::Stop("failed", "invalid_destination")),
        }
    } else {
        None
    };
    let target = match held.channel.as_str() {
        "email" => {
            if !config.smtp_available {
                return Ok(Prepared::Stop("unavailable", "smtp_unavailable"));
            }
            let email = if let Some(previous) = &previous {
                previous.email.clone().unwrap_or_default()
            } else {
                user.email.clone()
            };
            if email.trim().is_empty() {
                return Ok(Prepared::Stop("unavailable", "email_unavailable"));
            }
            Target::Email(email)
        }
        "telegram" => {
            let (encrypted, chat, topic) = if let Some(previous) = &previous {
                (
                    held.previous_token_encrypted.as_deref(),
                    previous.chat_id,
                    previous.message_thread_id,
                )
            } else {
                (
                    current.profile.telegram_token_encrypted.as_deref(),
                    current.profile.chat_id,
                    current.profile.message_thread_id,
                )
            };
            let Some(chat) = chat else {
                return Ok(Prepared::Stop("unavailable", "telegram_not_configured"));
            };
            let Some(token) = decrypt(encrypted, &key) else {
                return Ok(Prepared::Stop("failed", "encryption_unavailable"));
            };
            match telegram::Destination::new(token, chat, topic) {
                Ok(destination) => Target::Telegram(destination),
                Err(error) => return Ok(Prepared::Stop("failed", error.code())),
            }
        }
        _ => return Ok(Prepared::Stop("unavailable", "channel_unavailable")),
    };
    let mut metadata: Metadata = match serde_json::from_str(&held.metadata) {
        Ok(metadata) => metadata,
        Err(_) => return Ok(Prepared::Stop("failed", "invalid_event")),
    };
    metadata = metadata.bounded();
    if let Some(org_id) = metadata.organization_owner_id {
        let member = rg_db::ops::org_ops::find_org_member(db, org_id, user.id).await?;
        let active = rg_db::ops::org_ops::get_org(db, org_id)
            .await?
            .is_some_and(|org| org.deleted_at.is_none());
        if !active || !member.is_some_and(|member| member.role == "owner") {
            return Ok(Prepared::Stop("cancelled", "organization_access_removed"));
        }
    }
    if prior && held.channel == "telegram" {
        // Enforce minimization again at send, including imported/corrupt jobs.
        metadata = Metadata {
            changed: metadata.changed,
            ..Default::default()
        };
    }
    let locale = Locale::parse(if prior {
        held.prior_locale.as_deref().unwrap_or("en")
    } else {
        &current.profile.locale
    })
    .unwrap_or_default();
    Ok(Prepared::Send(
        target,
        template::render(
            event,
            locale,
            &metadata,
            held.created_at,
            held.is_test,
            prior,
            config.base_url,
        ),
    ))
}

fn retry_at(
    held: &notification_delivery::Model,
    error: Failure,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<chrono::DateTime<chrono::Utc>> {
    let transport = error.retry_after?;
    if held.attempts >= 6 {
        return None;
    }
    let backoff = Duration::from_secs(30 * (1_u64 << held.attempts.clamp(0, 5) as u32));
    let delay = transport.max(backoff);
    let remaining = (held.expires_at - now).to_std().ok()?;
    // Do not shorten Telegram's retry_after or extend the fixed job lifetime.
    // Compare Duration first, before any conversion/addition which could overflow.
    if delay >= remaining {
        return None;
    }
    let next = now.checked_add_signed(chrono::Duration::from_std(delay).ok()?)?;
    // Round upward for second-precision SQL backends: never retry earlier than
    // Telegram asked, and never push a retry beyond the fixed expiry.
    let seconds = next
        .timestamp()
        .checked_add(i64::from(next.timestamp_subsec_nanos() != 0))?;
    let next = chrono::DateTime::from_timestamp(seconds, 0)?;
    (next < held.expires_at).then_some(next)
}

pub async fn deliver_one(
    db: &DatabaseConnection,
    id: i64,
    config: &Config<'_>,
    sender: &dyn Sender,
) -> Result<()> {
    let Some(held) = queue::claim(db, id).await? else {
        return Ok(());
    };
    match prepare(db, &held, config).await? {
        Prepared::Stop(status, code) => queue::finish(db, &held, status, Some(code), None).await?,
        Prepared::Send(target, message) => match sender.send(&target, &message).await {
            Ok(()) => {
                if held.channel == "telegram" && held.previous_destination_encrypted.is_none() {
                    queue::confirm_telegram(db, held.user_id, held.generation).await?;
                }
                queue::finish(db, &held, "sent", None, None).await?;
            }
            Err(error) => {
                let retry = retry_at(&held, error, chrono::Utc::now());
                queue::finish(
                    db,
                    &held,
                    if retry.is_some() { "queued" } else { "failed" },
                    Some(error.code),
                    retry,
                )
                .await?;
            }
        },
    }
    Ok(())
}

pub async fn pass(db: &DatabaseConnection, config: &Config<'_>, sender: &dyn Sender) -> Result<()> {
    queue::cleanup(db).await?;
    for id in queue::due(db, 50).await? {
        if deliver_one(db, id, config, sender).await.is_err() {
            // The lease recovers on the next pass. Never log a payload/recipient
            // or database bind arguments through an arbitrary error source.
            tracing::warn!(
                delivery_id = id,
                "notification delivery database operation failed"
            );
        }
    }
    Ok(())
}

pub fn spawn_dispatcher(
    db: DatabaseConnection,
    key: String,
    smtp: Option<crate::email::SmtpConfig>,
    base_url: Option<String>,
    mut shutdown: tokio::sync::watch::Receiver<bool>,
) -> Result<tokio::task::JoinHandle<()>> {
    let smtp_available = smtp.is_some();
    let sender = Transport::new(smtp)?;
    Ok(tokio::spawn(async move {
        let config = Config {
            encryption_key: &key,
            base_url: base_url.as_deref(),
            smtp_available,
        };
        loop {
            if *shutdown.borrow() {
                break;
            }
            if pass(&db, &config, &sender).await.is_err() {
                tracing::warn!("notification delivery queue pass failed");
            }
            tokio::select! {
                _ = tokio::time::sleep(Duration::from_secs(5)) => {},
                changed = shutdown.changed() => { if changed.is_err() || *shutdown.borrow() { break; } },
            }
        }
    }))
}

#[cfg(test)]
#[path = "dispatch_tests.rs"]
mod tests;
