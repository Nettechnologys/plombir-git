//! One serialized settings writer for new and legacy APIs. Authorization is
//! supplied by the HTTP layer's EXISTING current-factor guard, on the account
//! snapshot locked in this transaction, never on an earlier handler read.
use std::collections::BTreeMap;

use anyhow::Result;
use sea_orm::{DatabaseConnection, Set, TransactionTrait};
use serde::{Deserialize, Serialize};

use rg_db::{
    entities::{notification_profile, notification_setting, user},
    notification::{Channels, Event, Locale, Metadata},
    ops::{notification_delivery_ops as queue, notification_setting_ops as legacy},
};

#[cfg(test)]
#[path = "settings_tests.rs"]
mod tests;

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChannelPatch {
    pub email: Option<bool>,
    pub telegram: Option<bool>,
}

/// A secret-bearing input deliberately has no Debug or Serialize implementation.
#[derive(Clone, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case", deny_unknown_fields)]
pub enum TelegramChange {
    Keep,
    Set {
        #[serde(default)]
        token: Option<String>,
        chat_id: i64,
        #[serde(default)]
        message_thread_id: Option<i32>,
    },
    Remove,
}

#[derive(Clone, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Patch {
    #[serde(default)]
    pub email: BTreeMap<Event, bool>,
    #[serde(default)]
    pub events: BTreeMap<Event, ChannelPatch>,
    pub locale: Option<Locale>,
    pub telegram: Option<TelegramChange>,
}

pub struct Settings {
    pub profile: notification_profile::Model,
    pub legacy: notification_setting::Model,
    pub events: BTreeMap<Event, Channels>,
}

pub async fn get(db: &impl sea_orm::ConnectionTrait, user_id: i64) -> Result<Settings> {
    let profile = queue::profile(db, user_id).await?;
    let legacy = legacy::get(db, user_id).await?;
    let events = queue::choices(&profile, &legacy)?;
    Ok(Settings {
        profile,
        legacy,
        events,
    })
}

/// The recipient snapshot contains NO raw bot token: its ciphertext is copied
/// into a separate registered encrypted column. Both columns rekey directly.
#[derive(Serialize, Deserialize)]
pub(crate) struct PreviousDestination {
    pub email: Option<String>,
    pub chat_id: Option<i64>,
    pub message_thread_id: Option<i32>,
}

pub struct SaveContext<'a> {
    pub encryption_secret: &'a str,
    pub smtp_available: bool,
    pub metadata: Metadata,
}

/// `authorize` is invoked on EVERY retry with the locked current account and
/// the freshly computed sensitivity. Its errors are propagated unwrapped so
/// the HTTP layer retains session_revoked vs sudo_required semantics.
pub async fn save(
    db: &DatabaseConnection,
    user_id: i64,
    patch: &Patch,
    context: &SaveContext<'_>,
    authorize: impl Fn(&user::Model, bool) -> Result<()>,
) -> Result<Settings> {
    rg_db::contention::retry_transaction("notification settings", || async {
        let transaction = db.begin().await?;
        let user = queue::lock_account(&transaction, user_id)
            .await?
            .ok_or_else(|| crate::error::not_found("account"))?;
        let old = get(&transaction, user_id).await?;
        let mut profile = old.profile.clone();
        let mut legacy = old.legacy.clone();
        let mut events = old.events.clone();
        for (&event, &email) in &patch.email {
            events.get_mut(&event).expect("known event").email = email;
        }
        for (&event, changes) in &patch.events {
            if let Some(email) = changes.email {
                if patch.email.get(&event).is_some_and(|value| *value != email) {
                    return Err(crate::error::invalid_request(
                        "conflicting notification email choices",
                    ));
                }
                events.get_mut(&event).expect("known event").email = email;
            }
            if let Some(telegram) = changes.telegram {
                events.get_mut(&event).expect("known event").telegram = telegram;
            }
        }
        if matches!(patch.telegram, Some(TelegramChange::Remove)) {
            for channels in events.values_mut() {
                channels.telegram = false;
            }
        }
        let mut changed = Vec::new();
        let mut reduced = false;
        for (&event, next) in &events {
            let before = old.events[&event];
            for (channel, from, to) in [
                ("email", before.email, next.email),
                ("telegram", before.telegram, next.telegram),
            ] {
                if from != to {
                    changed.push(format!("{}.{}", event.key(), channel));
                }
                reduced |= from && !to;
            }
            event.set_legacy_email(&mut legacy, next.email);
        }
        let destination_changed = match &patch.telegram {
            Some(TelegramChange::Set {
                token,
                chat_id,
                message_thread_id,
            }) => {
                if token.is_some() {
                    changed.push("telegram.token".into());
                }
                if profile.chat_id != Some(*chat_id) {
                    changed.push("telegram.chat_id".into());
                }
                if profile.message_thread_id != *message_thread_id {
                    changed.push("telegram.message_thread_id".into());
                }
                token.is_some()
                    || profile.chat_id != Some(*chat_id)
                    || profile.message_thread_id != *message_thread_id
            }
            Some(TelegramChange::Remove) => {
                let changed_destination = profile.telegram_token_encrypted.is_some()
                    || profile.chat_id.is_some()
                    || profile.message_thread_id.is_some();
                if changed_destination {
                    changed.push("telegram.removed".into());
                }
                changed_destination
            }
            _ => false,
        };
        if let Some(locale) = patch.locale {
            if profile.locale != locale.key() {
                changed.push("locale".into());
            }
            profile.locale = locale.key().into();
        }
        authorize(&user, reduced || destination_changed)?;
        if changed.is_empty() {
            transaction.commit().await?;
            return Ok(old);
        }

        match &patch.telegram {
            Some(TelegramChange::Set {
                token,
                chat_id,
                message_thread_id,
            }) => {
                super::telegram::validate_recipient(*chat_id, *message_thread_id)
                    .map_err(|error| crate::error::invalid_request(error.code()))?;
                if let Some(token) = token {
                    super::telegram::validate_token(token)
                        .map_err(|error| crate::error::invalid_request(error.code()))?;
                    profile.telegram_token_encrypted = Some(crate::auth::encryption::encrypt(
                        token,
                        &crate::auth::encryption::derive_key(context.encryption_secret),
                    )?);
                }
                if profile.telegram_token_encrypted.is_none() {
                    return Err(crate::error::invalid_request("telegram token is required"));
                }
                profile.chat_id = Some(*chat_id);
                profile.message_thread_id = *message_thread_id;
            }
            Some(TelegramChange::Remove) => {
                profile.telegram_token_encrypted = None;
                profile.chat_id = None;
                profile.message_thread_id = None;
            }
            _ => {}
        }
        // Turning Telegram on requires usable saved configuration. A removal
        // clears its per-event choices in the same protected operation.
        if matches!(patch.telegram, Some(TelegramChange::Remove)) {
            for channels in events.values_mut() {
                channels.telegram = false;
            }
        } else if events.values().any(|channels| channels.telegram)
            && !profile.telegram_configured()
        {
            return Err(crate::error::invalid_request(
                "configure Telegram before enabling delivery",
            ));
        }
        if destination_changed {
            profile.generation = profile
                .generation
                .checked_add(1)
                .ok_or_else(|| anyhow::anyhow!("notification generation exhausted"))?;
            profile.telegram_confirmed = false;
        }
        profile.revision = profile
            .revision
            .checked_add(1)
            .ok_or_else(|| anyhow::anyhow!("notification revision exhausted"))?;
        profile.updated_at = chrono::Utc::now();
        profile.choices = serde_json::to_string(&events)?;
        let mut event =
            queue::QueueEvent::security(user_id, Event::NotificationSettingsChanged, None);
        event.metadata = context.metadata.clone();
        event.metadata.changed = changed;
        if reduced || destination_changed {
            enqueue_previous(&transaction, &user, &old.profile, &event, context).await?;
        }
        queue::put_profile(&transaction, profile).await?;
        legacy::put(&transaction, legacy).await?;
        if !reduced && !destination_changed {
            queue::enqueue(&transaction, &event).await?;
        }
        let saved = get(&transaction, user_id).await?;
        transaction.commit().await?;
        Ok(saved)
    })
    .await
}

async fn enqueue_previous(
    db: &impl sea_orm::ConnectionTrait,
    user: &user::Model,
    old: &notification_profile::Model,
    event: &queue::QueueEvent,
    context: &SaveContext<'_>,
) -> Result<()> {
    let key = crate::auth::encryption::derive_key(context.encryption_secret);
    let eligible = [
        (
            "email",
            context.smtp_available
                && user.email_verified_at.is_some()
                && !user.email.trim().is_empty(),
        ),
        (
            "telegram",
            old.telegram_configured() && old.telegram_confirmed,
        ),
    ];
    let mut queued = false;
    for (channel, enabled) in eligible {
        if !enabled {
            continue;
        }
        let target = PreviousDestination {
            email: (channel == "email").then(|| user.email.clone()),
            chat_id: if channel == "telegram" {
                old.chat_id
            } else {
                None
            },
            message_thread_id: if channel == "telegram" {
                old.message_thread_id
            } else {
                None
            },
        };
        let mut row = queue::row(event, user, old, channel)?;
        if channel == "telegram" {
            // A retired chat may belong to somebody else now. Its one warning
            // gets no actor/IP/UA, only safe changed-field keys and timestamp.
            row.metadata = Set(serde_json::to_string(
                &Metadata {
                    changed: event.metadata.changed.clone(),
                    ..Default::default()
                }
                .bounded(),
            )?);
            row.previous_token_encrypted = Set(old.telegram_token_encrypted.clone());
        }
        row.previous_destination_encrypted = Set(Some(crate::auth::encryption::encrypt(
            &serde_json::to_string(&target)?,
            &key,
        )?));
        row.prior_locale = Set(Some(old.locale.clone()));
        queue::insert(db, row).await?;
        queued = true;
    }
    if !queued {
        let mut row = queue::row(event, user, old, "none")?;
        row.status = Set("unavailable".into());
        row.last_error = Set(Some("no_prior_destination".into()));
        row.metadata = Set("{}".into());
        row.destination_hash = Set(String::new());
        row.completed_at = Set(Some(chrono::Utc::now()));
        queue::insert(db, row).await?;
    }
    Ok(())
}
