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
            let inherited = matches!(
                event,
                Event::RepositoryAccessChanged
                    | Event::OrganizationAccessChanged
                    | Event::RepositorySecurityChanged
                    | Event::OrganizationSecurityChanged
                    | Event::WebhookChanged
                    | Event::MirrorChanged
                    | Event::CiSecurityChanged
                    | Event::AdminSecurityChanged
            )
            .then(|| {
                overrides
                    .get(&Event::NotificationSettingsChanged)
                    .map(|old| Channels {
                        email: event.default_email(),
                        telegram: profile.telegram_configured()
                            && profile.telegram_confirmed
                            && old.telegram,
                    })
            })
            .flatten();
            let mut value = overrides
                .get(&event)
                .copied()
                .or(inherited)
                .unwrap_or(Channels {
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
        // Known actor context belongs only to that account. Anonymous account
        // ceremonies (reset/initial password/email confirmation) describe the
        // recipient's own proof and retain the request context.
        if crate::notification::current_actor_id().is_some_and(|actor| actor != user_id) {
            metadata.ip = None;
            metadata.device = None;
        }
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
        priority: Set(event.event.priority()),
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

/// Queued or in-flight security rows of one kind one recipient may hold.
pub const QUEUED_PER_KIND: u64 = 5;
/// Queued or in-flight security rows of one event one recipient may hold,
/// whatever their destination, channel or repository. Kinds are not bounded
/// in number — every change of destination makes a new one — so without this
/// an account changing its own settings in a loop could queue without limit.
pub const QUEUED_PER_EVENT: u64 = 50;

/// What makes two security rows "the same warning to the same place": the
/// recipient, the event, the channel, the destination (for a warning to a
/// previous destination, that destination), the repository, and whether it
/// is a test.
type Kind = (i64, String, String, String, Option<i64>, bool);
/// The coarser budget: recipient, event, test flag, and whether the rows are
/// warnings to a previous destination. Those have a budget of their own: if
/// they shared it with ordinary rows, whoever holds the account could fill it
/// with ordinary settings changes and then change the destination, and the
/// warning owed to the old destination would be the row that is dropped.
type EventBudget = (i64, String, bool, bool);

fn kind_of(row: &delivery::ActiveModel) -> Option<Kind> {
    use crate::notification::PRIORITY_SHARED;
    if row.priority.clone().unwrap() > PRIORITY_SHARED {
        return None;
    }
    Some((
        row.user_id.clone().unwrap(),
        row.event_key.clone().unwrap(),
        row.channel.clone().unwrap(),
        row.destination_hash.clone().unwrap(),
        row.repo_id.clone().unwrap(),
        row.is_test.clone().unwrap(),
    ))
}

async fn held_of_event(db: &impl ConnectionTrait, budget: &EventBudget) -> Result<u64> {
    let query = delivery::Entity::find()
        .filter(delivery::Column::UserId.eq(budget.0))
        .filter(delivery::Column::EventKey.eq(budget.1.as_str()))
        .filter(delivery::Column::Status.is_in(["queued", "inflight"]))
        .filter(delivery::Column::IsTest.eq(budget.2));
    let query = if budget.3 {
        query.filter(delivery::Column::PreviousDestinationEncrypted.is_not_null())
    } else {
        query.filter(delivery::Column::PreviousDestinationEncrypted.is_null())
    };
    Ok(query.count(db).await?)
}

async fn held_of_kind(db: &impl ConnectionTrait, kind: &Kind) -> Result<u64> {
    let query = delivery::Entity::find()
        .filter(delivery::Column::UserId.eq(kind.0))
        .filter(delivery::Column::EventKey.eq(kind.1.as_str()))
        .filter(delivery::Column::Channel.eq(kind.2.as_str()))
        .filter(delivery::Column::Status.is_in(["queued", "inflight"]))
        .filter(delivery::Column::DestinationHash.eq(kind.3.as_str()))
        .filter(delivery::Column::IsTest.eq(kind.5));
    let query = match kind.4 {
        Some(repo_id) => query.filter(delivery::Column::RepoId.eq(repo_id)),
        None => query.filter(delivery::Column::RepoId.is_null()),
    };
    Ok(query.count(db).await?)
}

/// Keep the oldest: a security row arriving while [`QUEUED_PER_KIND`] rows of
/// its kind already wait is recorded as `coalesced` instead of queued. Rows
/// already queued are never displaced — evicting them would let whoever can
/// cause the event erase a warning somebody else caused. Without the bound, a
/// flood grows the due set the page query has to rank on every pass.
/// Concurrent enqueues can overshoot by a few; the bound only has to hold.
async fn admit(
    db: &impl ConnectionTrait,
    rows: Vec<delivery::ActiveModel>,
) -> Result<Vec<delivery::ActiveModel>> {
    let mut held: std::collections::HashMap<Kind, u64> = std::collections::HashMap::new();
    let mut per_event: std::collections::HashMap<EventBudget, u64> =
        std::collections::HashMap::new();
    let mut admitted = Vec::with_capacity(rows.len());
    for mut row in rows {
        let Some(kind) = kind_of(&row) else {
            admitted.push(row);
            continue;
        };
        let prior = row
            .previous_destination_encrypted
            .clone()
            .unwrap()
            .is_some();
        let budget = (kind.0, kind.1.clone(), kind.5, prior);
        let count = match held.get(&kind) {
            Some(count) => *count,
            None => held_of_kind(db, &kind).await?,
        };
        let event_count = match per_event.get(&budget) {
            Some(count) => *count,
            None => held_of_event(db, &budget).await?,
        };
        if count >= QUEUED_PER_KIND || event_count >= QUEUED_PER_EVENT {
            row.status = Set("cancelled".into());
            row.last_error = Set(Some("coalesced".into()));
            row.metadata = Set("{}".into());
            row.destination_hash = Set(String::new());
            row.previous_destination_encrypted = Set(None);
            row.previous_token_encrypted = Set(None);
            row.prior_locale = Set(None);
            row.completed_at = Set(Some(chrono::Utc::now()));
            held.insert(kind, count);
            per_event.insert(budget, event_count);
        } else {
            held.insert(kind, count + 1);
            per_event.insert(budget, event_count + 1);
        }
        admitted.push(row);
    }
    Ok(admitted)
}

pub async fn insert(db: &impl ConnectionTrait, row: delivery::ActiveModel) -> Result<()> {
    let row = admit(db, vec![row]).await?.remove(0);
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
    let rows = admit(db, rows).await?;
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

/// Rows of an account whose retirement has begun are left alone: delivering
/// would write to a closing account, cancelling would let a retirement that is
/// then aborted erase a pending security warning. A completed retirement
/// removes them with the account; an aborted one makes them due again.
fn not_retiring() -> sea_orm::sea_query::SimpleExpr {
    use sea_orm::sea_query::Query;
    delivery::Column::UserId.not_in_subquery(
        Query::select()
            .column(user::Column::Id)
            .from(user::Entity)
            .and_where(user::Column::DeletedAt.is_not_null())
            .to_owned(),
    )
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
        .filter(not_retiring())
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

/// Rows one recipient may take in a page of class 0 or 1.
pub const PER_RECIPIENT: u64 = 5;
/// Activity rows per page. Rows of a page are sent one after another, so this
/// bounds how many activity sends a security row that arrives mid-page waits
/// behind.
pub const ACTIVITY_PAGE: u64 = 10;
/// Places a class keeps in a page while it has due rows, so class 0 can never
/// take the whole page from classes 1 and 2.
pub const CLASS_FLOOR: usize = 5;

/// The ids to deliver next, at most `limit`, in delivery order.
///
/// Classes from [`Event::priority`]: the account's own credentials (0) before
/// shared-resource security (1) before activity (2). Within classes 0 and 1 one
/// recipient takes at most [`PER_RECIPIENT`] rows per page, so an account
/// flooding its own events, or a flood aimed at one victim, cannot hold the
/// class for everybody else. Classes 1 and 2 keep [`CLASS_FLOOR`] places each
/// while they have due rows. What remains unbounded is a flood spread over many
/// recipients; that needs a per-actor budget.
pub async fn due(db: &impl ConnectionTrait, limit: u64) -> Result<Vec<i64>> {
    use crate::notification::{PRIORITY_ACCOUNT, PRIORITY_ACTIVITY, PRIORITY_SHARED};
    let now = chrono::Utc::now();
    let account = due_per_recipient(db, PRIORITY_ACCOUNT, now, limit).await?;
    let shared = due_per_recipient(db, PRIORITY_SHARED, now, limit).await?;
    let activity = delivery::Entity::find()
        .select_only()
        .column(delivery::Column::Id)
        .filter(due_now(PRIORITY_ACTIVITY, now))
        .order_by_asc(delivery::Column::NextAttemptAt)
        .order_by_asc(delivery::Column::Id)
        .limit(ACTIVITY_PAGE.min(limit))
        .into_tuple::<i64>()
        .all(db)
        .await?;
    Ok(allocate(
        usize::try_from(limit).unwrap_or(usize::MAX),
        account,
        shared,
        activity,
    ))
}

fn due_now(class: i16, now: chrono::DateTime<chrono::Utc>) -> Condition {
    Condition::all()
        .add(delivery::Column::Priority.eq(class))
        .add(claimable(now))
        .add(not_retiring())
        .add(delivery::Column::NextAttemptAt.lte(now))
        .add(delivery::Column::ExpiresAt.gt(now))
        .add(delivery::Column::Attempts.lt(6))
}

/// Due rows of one class, oldest first, at most [`PER_RECIPIENT`] per user.
/// The window ranks every due row of the class, not a prefix of them: cutting
/// the input first would let one recipient's backlog fill the prefix and keep
/// everybody else out of the page.
async fn due_per_recipient(
    db: &impl ConnectionTrait,
    class: i16,
    now: chrono::DateTime<chrono::Utc>,
    limit: u64,
) -> Result<Vec<i64>> {
    use sea_orm::sea_query::{Alias, Order, Query};
    let mut ranked = Query::select();
    ranked
        .column(delivery::Column::Id)
        .column(delivery::Column::NextAttemptAt)
        .expr_as(
            Expr::cust("ROW_NUMBER() OVER (PARTITION BY user_id ORDER BY next_attempt_at, id)"),
            Alias::new("recipient_rank"),
        )
        .from(delivery::Entity)
        .cond_where(due_now(class, now));
    let mut page = Query::select();
    page.column(Alias::new("id"))
        .from_subquery(ranked, Alias::new("ranked"))
        .and_where(Expr::col(Alias::new("recipient_rank")).lte(PER_RECIPIENT))
        .order_by(Alias::new("next_attempt_at"), Order::Asc)
        .order_by(Alias::new("id"), Order::Asc)
        .limit(limit);
    let rows = db.query_all(db.get_database_backend().build(&page)).await?;
    rows.iter()
        .map(|row| row.try_get::<i64>("", "id").map_err(Into::into))
        .collect()
}

/// Lay out one page from the three classes' candidates.
///
/// Meant for `limit >= 2 * CLASS_FLOOR` (the dispatcher asks for 50): below
/// that the floors of classes 1 and 2 take the whole page, and activity can
/// come before security.
fn allocate(limit: usize, account: Vec<i64>, shared: Vec<i64>, activity: Vec<i64>) -> Vec<i64> {
    let shared_floor = shared.len().min(CLASS_FLOOR);
    let activity_floor = activity.len().min(CLASS_FLOOR);
    let take_account = account
        .len()
        .min(limit.saturating_sub(shared_floor + activity_floor));
    let take_shared = shared
        .len()
        .min(limit.saturating_sub(take_account + activity_floor));
    let take_activity = activity
        .len()
        .min(limit.saturating_sub(take_account + take_shared));
    account
        .into_iter()
        .take(take_account)
        .chain(shared.into_iter().take(take_shared))
        .chain(activity.into_iter().take(take_activity))
        .collect()
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

/// Hand a claimed row back untouched: its account began retiring after the
/// claim. The attempt the claim counted is returned too, so retirements that
/// start and abort around a pending warning cannot spend its six attempts.
/// Compare-and-set on the lease, like [`finish`].
pub async fn release(
    db: &impl ConnectionTrait,
    held: &delivery::Model,
    retry_at: chrono::DateTime<chrono::Utc>,
) -> Result<()> {
    let Some(lease) = held.lease_token.as_deref() else {
        anyhow::bail!("notification delivery has no lease");
    };
    delivery::Entity::update_many()
        .col_expr(delivery::Column::Status, Expr::value("queued"))
        .col_expr(
            delivery::Column::Attempts,
            Expr::col(delivery::Column::Attempts).sub(1),
        )
        .col_expr(delivery::Column::NextAttemptAt, Expr::value(retry_at))
        .col_expr(
            delivery::Column::LeaseToken,
            Expr::value(Option::<String>::None),
        )
        .col_expr(
            delivery::Column::LeaseUntil,
            Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
        )
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

/// Bind values per statement, comfortably under SQLite's 32766 and MySQL's
/// 65535 placeholders.
const SUPERSEDE_BATCH: usize = 300;

/// Cancel queued rows of earlier versions of the given thread notifications.
///
/// `current` holds `(user_id, notification id, current event id)` for every
/// notification just written. A fold rewrites the notification, so each
/// version gets a new event id; without this the dispatcher would carry every
/// superseded version until it reached and stopped it. Rows already in flight
/// hold a lease and are left to `prepare`, which stops them the same way. The
/// terminal state matches that path: `cancelled`,
/// `thread_removed_or_superseded`, payload cleared.
pub async fn cancel_superseded_thread_rows(
    db: &impl ConnectionTrait,
    current: &[(i64, i64, String)],
) -> Result<u64> {
    let now = chrono::Utc::now();
    let mut cancelled = 0;
    // Three binds per notification, plus the status filter.
    for batch in current.chunks(SUPERSEDE_BATCH / 3) {
        let mut versions = Condition::any();
        for (user_id, note_id, event_id) in batch {
            versions = versions.add(
                Condition::all()
                    .add(delivery::Column::UserId.eq(*user_id))
                    .add(delivery::Column::EventId.starts_with(format!("thread:{note_id}:")))
                    .add(delivery::Column::EventId.ne(event_id.as_str())),
            );
        }
        cancelled += clear_payload(delivery::Entity::update_many())
            .col_expr(delivery::Column::Status, Expr::value("cancelled"))
            .col_expr(
                delivery::Column::LastError,
                Expr::value("thread_removed_or_superseded"),
            )
            .col_expr(delivery::Column::CompletedAt, Expr::value(now))
            .col_expr(
                delivery::Column::LeaseToken,
                Expr::value(Option::<String>::None),
            )
            .col_expr(
                delivery::Column::LeaseUntil,
                Expr::value(Option::<chrono::DateTime<chrono::Utc>>::None),
            )
            .filter(delivery::Column::Status.eq("queued"))
            .filter(versions)
            .exec(db)
            .await?
            .rows_affected;
    }
    Ok(cancelled)
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

#[cfg(test)]
mod page_tests {
    use super::{allocate, CLASS_FLOOR};

    fn ids(from: i64, count: i64) -> Vec<i64> {
        (from..from + count).collect()
    }

    /// Class 0 goes first, but cannot take the places classes 1 and 2 keep.
    #[test]
    fn account_rows_lead_without_starving_the_other_classes() {
        let page = allocate(50, ids(0, 50), ids(100, 50), ids(200, 10));
        assert_eq!(page.len(), 50);
        assert_eq!(&page[..40], &ids(0, 40)[..]);
        assert_eq!(
            page.iter().filter(|id| (100..150).contains(*id)).count(),
            CLASS_FLOOR
        );
        assert_eq!(
            page.iter().filter(|id| (200..210).contains(*id)).count(),
            CLASS_FLOOR
        );
    }

    /// An empty class keeps nothing: the places go to whoever has rows.
    #[test]
    fn floors_apply_only_to_classes_with_due_rows() {
        assert_eq!(allocate(50, ids(0, 60), vec![], vec![]).len(), 50);
        let page = allocate(50, ids(0, 3), ids(100, 60), ids(200, 10));
        assert_eq!(&page[..3], &[0, 1, 2]);
        assert_eq!(
            page.iter().filter(|id| (100..160).contains(*id)).count(),
            42
        );
        assert_eq!(page.iter().filter(|id| (200..210).contains(*id)).count(), 5);
    }

    #[test]
    fn a_small_limit_never_overflows() {
        assert_eq!(allocate(3, ids(0, 10), ids(100, 10), ids(200, 10)).len(), 3);
        assert!(allocate(0, ids(0, 10), ids(100, 10), ids(200, 10)).is_empty());
    }
}
