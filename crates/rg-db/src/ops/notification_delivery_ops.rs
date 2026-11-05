//! Durable notification enqueue and lease/CAS operations. Call enqueue in the
//! SAME transaction as the fact it reports; never after a speculative write.
use std::collections::BTreeMap;

use anyhow::{Context, Result};
use sea_orm::{
    sea_query::{Expr, OnConflict},
    *,
};

use crate::{
    entities::{notification_delivery as delivery, notification_profile as profile, user},
    notification::{Channels, Event, Metadata},
};

pub async fn profile(db: &impl ConnectionTrait, user_id: i64) -> Result<profile::Model> {
    Ok(profile::Entity::find()
        .filter(profile::Column::UserId.eq(user_id))
        .one(db)
        .await?
        .unwrap_or_else(|| profile::Model::defaults(user_id)))
}

pub fn choices(
    profile: &profile::Model,
    legacy: &crate::entities::notification_setting::Model,
) -> Result<BTreeMap<Event, Channels>> {
    let overrides: BTreeMap<Event, Channels> = serde_json::from_str(&profile.choices)
        .context("invalid notification preference storage")?;
    Ok(Event::ALL
        .iter()
        .copied()
        .map(|event| {
            let mut value = overrides.get(&event).copied().unwrap_or(Channels {
                email: event.default_email(),
                telegram: false,
            });
            if let Some(email) = event.legacy_email(legacy) {
                value.email = email;
            }
            (event, value)
        })
        .collect())
}

/// The first statement in a sensitive settings transaction: take the account
/// write lock without changing auth/sudo state, then read current state. This
/// also serializes absent-profile/default-row writers on every backend. A
/// MySQL unchanged-update count is not a missing-account verdict.
pub async fn lock_account(db: &impl ConnectionTrait, user_id: i64) -> Result<Option<user::Model>> {
    user::Entity::update_many()
        .col_expr(
            user::Column::SessionVersion,
            Expr::col(user::Column::SessionVersion).into(),
        )
        .filter(user::Column::Id.eq(user_id))
        .exec(db)
        .await?;
    Ok(user::Entity::find_by_id(user_id)
        .one(db)
        .await?
        .filter(|user| user.is_usable() && !user.is_bot()))
}

pub async fn put_profile(
    db: &impl ConnectionTrait,
    value: profile::Model,
) -> Result<profile::Model> {
    let new = value.id == 0;
    let mut active: profile::ActiveModel = value.into();
    if new {
        active.id = NotSet;
        Ok(active.insert(db).await?)
    } else {
        Ok(active.reset_all().update(db).await?)
    }
}

/// Safe queue input; IDs for immutable business events provide dedup across
/// retries. For one-off successful changes use a fresh UUID inside the txn.
pub struct QueueEvent {
    pub id: String,
    pub user_id: i64,
    pub event: Event,
    pub metadata: Metadata,
    pub repo_id: Option<i64>,
    pub admin_only: bool,
    pub is_test: bool,
}

impl QueueEvent {
    pub fn security(user_id: i64, event: Event, name: Option<String>) -> Self {
        let mut metadata = crate::notification::current_metadata();
        metadata.name = name;
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            user_id,
            event,
            metadata,
            repo_id: None,
            admin_only: false,
            is_test: false,
        }
    }
}

/// Construct a single row. Prior-destination fields are filled only by the
/// authorized settings service; ordinary producers cannot redirect delivery.
pub fn row(
    event: &QueueEvent,
    user: &user::Model,
    profile: &profile::Model,
    channel: &str,
) -> Result<delivery::ActiveModel> {
    // Supported MySQL temporal columns have second precision. Avoid rounding
    // an immediately queued event into the future or extending its 24h expiry.
    let now = chrono::DateTime::from_timestamp(chrono::Utc::now().timestamp(), 0)
        .expect("current UTC timestamp");
    Ok(delivery::ActiveModel {
        id: NotSet,
        event_id: Set(event.id.clone()),
        user_id: Set(user.id),
        event_key: Set(event.event.key().to_owned()),
        channel: Set(channel.to_owned()),
        metadata: Set(serde_json::to_string(&event.metadata.clone().bounded())?),
        repo_id: Set(event.repo_id),
        admin_only: Set(event.admin_only),
        generation: Set(profile.generation),
        destination_hash: Set(crate::token_digest(&user.email)),
        previous_destination_encrypted: Set(None),
        previous_token_encrypted: Set(None),
        prior_locale: Set(None),
        is_test: Set(event.is_test),
        status: Set("queued".into()),
        attempts: Set(0),
        next_attempt_at: Set(now),
        lease_token: Set(None),
        lease_until: Set(None),
        last_error: Set(None),
        created_at: Set(now),
        expires_at: Set(now + chrono::Duration::hours(24)),
        completed_at: Set(None),
    })
}

pub async fn insert(db: &impl ConnectionTrait, row: delivery::ActiveModel) -> Result<()> {
    delivery::Entity::insert(row)
        .on_conflict(
            OnConflict::columns([
                delivery::Column::EventId,
                delivery::Column::UserId,
                delivery::Column::Channel,
            ])
            .do_nothing_on([delivery::Column::Id])
            .to_owned(),
        )
        .exec_without_returning(db)
        .await?;
    Ok(())
}

pub async fn enqueue(db: &impl ConnectionTrait, event: &QueueEvent) -> Result<()> {
    let Some(user) = user::Entity::find_by_id(event.user_id)
        .one(db)
        .await?
        .filter(|user| user.is_usable() && !user.is_bot())
    else {
        return Ok(());
    };
    let profile = profile(db, user.id).await?;
    let legacy = super::notification_setting_ops::get(db, user.id).await?;
    let channels = choices(&profile, &legacy)?[&event.event];
    if channels.email && !user.email.trim().is_empty() {
        insert(db, row(event, &user, &profile, "email")?).await?;
    }
    if channels.telegram && profile.telegram_configured() {
        insert(db, row(event, &user, &profile, "telegram")?).await?;
    }
    Ok(())
}

/// One bounded fanout page, without a per-recipient query/transaction. Capture
/// recipients and destination generations in the thread's publication commit.
pub async fn enqueue_many(db: &impl ConnectionTrait, events: &[QueueEvent]) -> Result<()> {
    if events.is_empty() {
        return Ok(());
    }
    let ids: Vec<i64> = events.iter().map(|event| event.user_id).collect();
    let users: std::collections::HashMap<_, _> = user::Entity::find()
        .filter(user::Column::Id.is_in(ids.iter().copied()))
        .all(db)
        .await?
        .into_iter()
        .filter(|user| user.is_usable() && !user.is_bot())
        .map(|user| (user.id, user))
        .collect();
    let profiles: std::collections::HashMap<_, _> = profile::Entity::find()
        .filter(profile::Column::UserId.is_in(ids.iter().copied()))
        .all(db)
        .await?
        .into_iter()
        .map(|profile| (profile.user_id, profile))
        .collect();
    let legacy = super::notification_setting_ops::get_many(db, &ids).await?;
    let mut rows = Vec::new();
    for event in events {
        let Some(user) = users.get(&event.user_id) else {
            continue;
        };
        let default = profile::Model::defaults(user.id);
        let profile = profiles.get(&user.id).unwrap_or(&default);
        let channels = choices(profile, &legacy[&user.id])?[&event.event];
        if channels.email && !user.email.trim().is_empty() {
            rows.push(row(event, user, profile, "email")?);
        }
        if channels.telegram && profile.telegram_configured() {
            rows.push(row(event, user, profile, "telegram")?);
        }
    }
    for page in rows.chunks(500) {
        delivery::Entity::insert_many(page.to_vec())
            .on_conflict(
                OnConflict::columns([
                    delivery::Column::EventId,
                    delivery::Column::UserId,
                    delivery::Column::Channel,
                ])
                .do_nothing_on([delivery::Column::Id])
                .to_owned(),
            )
            .exec_without_returning(db)
            .await?;
    }
    Ok(())
}

/// Fingerprint the persisted source version, including its text/reason. Some
/// MySQL schemas store second precision, so timestamps alone would swallow a
/// stronger reason or renamed subject folded within the same second.
pub fn thread_event_id(note: &crate::entities::notification::Model) -> String {
    let version = serde_json::to_string(&(
        note.created_at,
        note.updated_at,
        &note.title,
        &note.body,
        &note.reason,
        &note.link,
    ))
    .expect("notification source strings serialize");
    format!(
        "thread:{}:{}",
        note.id,
        &crate::token_digest(&version)[..40]
    )
}

/// Credential-owner routing is resolved inside the mutation transaction.
/// Bots never receive personal security mail: their human owner does.
pub async fn account_event(
    db: &impl ConnectionTrait,
    owner_id: i64,
    event: Event,
    bot_event: Option<Event>,
    name: Option<String>,
    fingerprint: Option<String>,
) -> Result<()> {
    let Some(owner) = user::Entity::find_by_id(owner_id).one(db).await? else {
        return Ok(());
    };
    let (recipient, event) = if owner.is_bot() {
        let Some(recipient) = owner.bot_owner_id else {
            return Ok(());
        };
        (recipient, bot_event.unwrap_or(event))
    } else {
        (owner.id, event)
    };
    let mut event = QueueEvent::security(recipient, event, name);
    event.metadata.fingerprint = fingerprint;
    enqueue(db, &event).await
}

/// Retain database error identity for retryable transactions, without rendering
/// an arbitrary error (which could include data) into another error string.
pub fn db_error(error: anyhow::Error) -> DbErr {
    error
        .downcast::<DbErr>()
        .unwrap_or_else(|_| DbErr::Custom("notification enqueue failed".into()))
}

pub async fn event_rows(
    db: &impl ConnectionTrait,
    user_id: i64,
    event_id: &str,
) -> Result<Vec<delivery::Model>> {
    Ok(delivery::Entity::find()
        .filter(delivery::Column::UserId.eq(user_id))
        .filter(delivery::Column::EventId.eq(event_id))
        .order_by_asc(delivery::Column::Id)
        .all(db)
        .await?)
}

/// Account lock must already be held by the test endpoint. A per-event
/// minute cooldown plus six tests/account/minute prevents concurrent spam.
pub async fn test_allowed(db: &impl ConnectionTrait, user_id: i64, event: Event) -> Result<bool> {
    let recent = delivery::Entity::find()
        .filter(delivery::Column::UserId.eq(user_id))
        .filter(delivery::Column::IsTest.eq(true))
        .filter(delivery::Column::CreatedAt.gt(chrono::Utc::now() - chrono::Duration::minutes(1)))
        .all(db)
        .await?;
    let count = recent
        .iter()
        .map(|row| &row.event_id)
        .collect::<std::collections::HashSet<_>>()
        .len();
    if count >= 6 || recent.iter().any(|row| row.event_key == event.key()) {
        return Ok(false);
    }
    Ok(!delivery::Entity::find()
        .filter(delivery::Column::UserId.eq(user_id))
        .filter(delivery::Column::IsTest.eq(true))
        .filter(delivery::Column::EventKey.eq(event.key()))
        .filter(delivery::Column::Status.is_in(["queued", "inflight"]))
        .filter(delivery::Column::ExpiresAt.gt(chrono::Utc::now()))
        .one(db)
        .await?
        .is_some())
}

fn claimable(now: chrono::DateTime<chrono::Utc>) -> Condition {
    Condition::any()
        .add(delivery::Column::Status.eq("queued"))
        .add(
            Condition::all()
                .add(delivery::Column::Status.eq("inflight"))
                .add(delivery::Column::LeaseUntil.lt(now)),
        )
}

/// Atomic lease acquisition, including recovery of a dead worker's lease.
pub async fn claim(db: &impl ConnectionTrait, id: i64) -> Result<Option<delivery::Model>> {
    let now = chrono::Utc::now();
    let lease = uuid::Uuid::new_v4().to_string();
    let changed = delivery::Entity::update_many()
        .col_expr(delivery::Column::Status, Expr::value("inflight"))
        .col_expr(delivery::Column::LeaseToken, Expr::value(lease.clone()))
        .col_expr(
            delivery::Column::LeaseUntil,
            Expr::value(now + chrono::Duration::seconds(60)),
        )
        .col_expr(
            delivery::Column::Attempts,
            Expr::col(delivery::Column::Attempts).add(1),
        )
        .filter(delivery::Column::Id.eq(id))
        .filter(claimable(now))
        .filter(delivery::Column::NextAttemptAt.lte(now))
        .filter(delivery::Column::ExpiresAt.gt(now))
        .filter(delivery::Column::Attempts.lt(6))
        .exec(db)
        .await?;
    if changed.rows_affected != 1 {
        return Ok(None);
    }
    Ok(delivery::Entity::find_by_id(id)
        .filter(delivery::Column::LeaseToken.eq(lease))
        .one(db)
        .await?)
}

pub async fn due(db: &impl ConnectionTrait, limit: u64) -> Result<Vec<i64>> {
    let now = chrono::Utc::now();
    Ok(delivery::Entity::find()
        .filter(claimable(now))
        .filter(delivery::Column::NextAttemptAt.lte(now))
        .filter(delivery::Column::ExpiresAt.gt(now))
        .filter(delivery::Column::Attempts.lt(6))
        .order_by_asc(delivery::Column::NextAttemptAt)
        .order_by_asc(delivery::Column::Id)
        .limit(limit)
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect())
}

/// CAS the lease before acknowledging a result; a late expired worker cannot
/// overwrite another worker's result. Terminal rows retain no payload/secrets.
pub async fn finish(
    db: &impl ConnectionTrait,
    held: &delivery::Model,
    status: &str,
    code: Option<&str>,
    retry_at: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<()> {
    let Some(lease) = held.lease_token.as_deref() else {
        anyhow::bail!("notification delivery has no lease");
    };
    let mut update = delivery::Entity::update_many()
        .col_expr(delivery::Column::Status, Expr::value(status))
        .col_expr(
            delivery::Column::LastError,
            Expr::value(code.map(str::to_owned)),
        )
        .col_expr(
            delivery::Column::LeaseToken,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            delivery::Column::LeaseUntil,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        );
    if let Some(next) = retry_at {
        update = update.col_expr(delivery::Column::NextAttemptAt, Expr::value(next));
    } else {
        update = clear_payload(update).col_expr(
            delivery::Column::CompletedAt,
            Expr::value(chrono::Utc::now()),
        );
    }
    update
        .filter(delivery::Column::Id.eq(held.id))
        .filter(delivery::Column::LeaseToken.eq(lease))
        .exec(db)
        .await?;
    Ok(())
}

fn clear_payload(update: UpdateMany<delivery::Entity>) -> UpdateMany<delivery::Entity> {
    update
        .col_expr(
            delivery::Column::PreviousDestinationEncrypted,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            delivery::Column::PreviousTokenEncrypted,
            Expr::value(Option::<String>::None),
        )
        .col_expr(delivery::Column::Metadata, Expr::value("{}"))
        .col_expr(delivery::Column::DestinationHash, Expr::value(""))
}

pub async fn cleanup(db: &impl ConnectionTrait) -> Result<()> {
    let now = chrono::Utc::now();
    clear_payload(delivery::Entity::update_many())
        .col_expr(delivery::Column::Status, Expr::value("failed"))
        .col_expr(delivery::Column::LastError, Expr::value("delivery_expired"))
        .col_expr(delivery::Column::CompletedAt, Expr::value(now))
        .col_expr(
            delivery::Column::LeaseToken,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            delivery::Column::LeaseUntil,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
        .filter(delivery::Column::Status.is_in(["queued", "inflight"]))
        .filter(
            Condition::any()
                .add(delivery::Column::ExpiresAt.lte(now))
                .add(
                    Condition::all()
                        .add(delivery::Column::Attempts.gte(6))
                        .add(claimable(now)),
                ),
        )
        .exec(db)
        .await?;
    delivery::Entity::delete_many()
        .filter(delivery::Column::CompletedAt.lt(now - chrono::Duration::days(30)))
        .exec(db)
        .await?;
    crate::entities::known_browser::Entity::delete_many()
        .filter(crate::entities::known_browser::Column::ExpiresAt.lte(now))
        .exec(db)
        .await?;
    Ok(())
}

/// Successful test/real send confirms ONLY the exact still-current generation.
pub async fn confirm_telegram(
    db: &impl ConnectionTrait,
    user_id: i64,
    generation: i64,
) -> Result<()> {
    profile::Entity::update_many()
        .col_expr(profile::Column::TelegramConfirmed, Expr::value(true))
        .filter(profile::Column::UserId.eq(user_id))
        .filter(profile::Column::Generation.eq(generation))
        .filter(profile::Column::TelegramTokenEncrypted.is_not_null())
        .exec(db)
        .await?;
    Ok(())
}

/// Account retirement must destroy retained destinations even while a worker
/// is down. Logout/session revocation deliberately does not call this.
pub async fn cancel_account(db: &impl ConnectionTrait, user_id: i64) -> Result<()> {
    clear_payload(delivery::Entity::update_many())
        .col_expr(delivery::Column::Status, Expr::value("cancelled"))
        .col_expr(
            delivery::Column::CompletedAt,
            Expr::value(chrono::Utc::now()),
        )
        .filter(delivery::Column::UserId.eq(user_id))
        .filter(delivery::Column::Status.is_in(["queued", "inflight"]))
        .exec(db)
        .await?;
    Ok(())
}
