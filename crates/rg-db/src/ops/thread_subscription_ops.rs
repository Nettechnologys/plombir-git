//! Database operations for `thread_subscriptions`.

use anyhow::{Context, Result};
use sea_orm::sea_query::OnConflict;
use sea_orm::*;

use crate::entities::thread_subscription;

/// The account's row for a subject, subscribed or not.
pub async fn find(
    db: &DatabaseConnection,
    user_id: i64,
    subject_type: &str,
    subject_id: i64,
) -> Result<Option<thread_subscription::Model>> {
    thread_subscription::Entity::find()
        .filter(thread_subscription::Column::UserId.eq(user_id))
        .filter(thread_subscription::Column::SubjectType.eq(subject_type))
        .filter(thread_subscription::Column::SubjectId.eq(subject_id))
        .one(db)
        .await
        .context("db: find thread subscription")
}

fn row(
    user_id: i64,
    repo_id: i64,
    subject_type: &str,
    subject_id: i64,
    subscribed: bool,
    reason: &str,
) -> thread_subscription::ActiveModel {
    let now = chrono::Utc::now();
    thread_subscription::ActiveModel {
        id: NotSet,
        user_id: Set(user_id),
        repo_id: Set(repo_id),
        subject_type: Set(subject_type.to_string()),
        subject_id: Set(subject_id),
        subscribed: Set(subscribed),
        reason: Set(reason.to_string()),
        created_at: Set(now),
        updated_at: Set(now),
    }
}

/// Subscribe the account because it took part — unless it already has a row.
/// An existing row is left alone on purpose: an explicit unsubscribe must
/// survive the next comment, and an older reason is the truer one.
pub async fn subscribe_if_absent(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
    subject_type: &str,
    subject_id: i64,
    reason: &str,
) -> Result<()> {
    use thread_subscription::Column;
    thread_subscription::Entity::insert(row(
        user_id,
        repo_id,
        subject_type,
        subject_id,
        true,
        reason,
    ))
    .on_conflict(
        OnConflict::columns([Column::UserId, Column::SubjectType, Column::SubjectId])
            .do_nothing()
            .to_owned(),
    )
    .do_nothing()
    .exec(db)
    .await
    .context("db: subscribe to thread")?;
    Ok(())
}

/// The account's explicit choice, replacing whatever row it had.
pub async fn set(
    db: &DatabaseConnection,
    user_id: i64,
    repo_id: i64,
    subject_type: &str,
    subject_id: i64,
    subscribed: bool,
) -> Result<thread_subscription::Model> {
    use thread_subscription::Column;
    thread_subscription::Entity::insert(row(
        user_id,
        repo_id,
        subject_type,
        subject_id,
        subscribed,
        "manual",
    ))
    .on_conflict(
        OnConflict::columns([Column::UserId, Column::SubjectType, Column::SubjectId])
            .update_columns([Column::Subscribed, Column::Reason, Column::UpdatedAt])
            .to_owned(),
    )
    .exec_without_returning(db)
    .await
    .context("db: set thread subscription")?;
    find(db, user_id, subject_type, subject_id)
        .await?
        .context("db: thread subscription vanished after its write")
}

/// Everyone subscribed to a subject.
pub async fn list_subscribers(
    db: &DatabaseConnection,
    subject_type: &str,
    subject_id: i64,
) -> Result<Vec<i64>> {
    thread_subscription::Entity::find()
        .select_only()
        .column(thread_subscription::Column::UserId)
        .filter(thread_subscription::Column::SubjectType.eq(subject_type))
        .filter(thread_subscription::Column::SubjectId.eq(subject_id))
        .filter(thread_subscription::Column::Subscribed.eq(true))
        .order_by_asc(thread_subscription::Column::Id)
        .into_tuple()
        .all(db)
        .await
        .context("db: list thread subscribers")
}

/// Remove every row about a subject — the issue is gone.
pub async fn delete_for_subject(
    db: &DatabaseConnection,
    subject_type: &str,
    subject_id: i64,
) -> Result<u64> {
    let result = thread_subscription::Entity::delete_many()
        .filter(thread_subscription::Column::SubjectType.eq(subject_type))
        .filter(thread_subscription::Column::SubjectId.eq(subject_id))
        .exec(db)
        .await
        .context("db: delete thread subscriptions for subject")?;
    Ok(result.rows_affected)
}
