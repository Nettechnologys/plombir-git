//! Login log operations.
use sea_orm::*;

use crate::entities::login_log;
pub use crate::entities::login_log::Entity;

fn bounded(value: &str, max_chars: usize) -> String {
    value.chars().take(max_chars).collect()
}

/// Log a login attempt.
#[allow(clippy::too_many_arguments)]
pub async fn log_attempt(
    db: &DatabaseConnection,
    user_id: Option<i64>,
    username: &str,
    auth_provider: &str,
    ip_address: Option<&str>,
    user_agent: Option<&str>,
    success: bool,
    failure_reason: Option<&str>,
) -> Result<login_log::Model, DbErr> {
    let now = chrono::Utc::now();
    let am = login_log::ActiveModel {
        id: NotSet,
        user_id: Set(user_id),
        username: Set(bounded(username, 255)),
        auth_provider: Set(bounded(auth_provider, 20)),
        ip_address: Set(ip_address.map(|value| bounded(value, 45))),
        user_agent: Set(user_agent.map(|value| bounded(value, 512))),
        success: Set(success),
        failure_reason: Set(failure_reason.map(|value| bounded(value, 255))),
        created_at: Set(now),
    };
    am.insert(db).await
}

/// Get recent login logs for a user.
pub async fn recent_for_user(
    db: &DatabaseConnection,
    user_id: i64,
    limit: u64,
) -> Result<Vec<login_log::Model>, DbErr> {
    Entity::find()
        .filter(login_log::Column::UserId.eq(user_id))
        .order_by_desc(login_log::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
}

/// Count failed attempts for a username since a given time (brute-force detection).
pub async fn count_failed_since(
    db: &DatabaseConnection,
    username: &str,
    since: chrono::DateTime<chrono::Utc>,
) -> Result<u64, DbErr> {
    use sea_orm::QueryFilter;
    Entity::find()
        .filter(login_log::Column::Username.eq(username))
        .filter(login_log::Column::Success.eq(false))
        .filter(login_log::Column::CreatedAt.gte(since))
        .count(db)
        .await
}

#[allow(clippy::too_many_arguments)]
pub async fn list_paginated(
    db: &DatabaseConnection,
    page: u64,
    per_page: u64,
    username: Option<&str>,
    auth_provider: Option<&str>,
    success: Option<bool>,
    start_time: Option<chrono::DateTime<chrono::Utc>>,
    end_time: Option<chrono::DateTime<chrono::Utc>>,
) -> Result<(Vec<login_log::Model>, u64), DbErr> {
    let mut query = Entity::find();
    if let Some(username) = username {
        query = query.filter(login_log::Column::Username.eq(username));
    }
    if let Some(auth_provider) = auth_provider {
        query = query.filter(login_log::Column::AuthProvider.eq(auth_provider));
    }
    if let Some(success) = success {
        query = query.filter(login_log::Column::Success.eq(success));
    }
    if let Some(start_time) = start_time {
        query = query.filter(login_log::Column::CreatedAt.gte(start_time));
    }
    if let Some(end_time) = end_time {
        query = query.filter(login_log::Column::CreatedAt.lte(end_time));
    }
    let total = query.clone().count(db).await?;
    let logs = query
        .order_by_desc(login_log::Column::CreatedAt)
        .order_by_desc(login_log::Column::Id)
        .paginate(db, per_page)
        .fetch_page(page)
        .await?;
    Ok((logs, total))
}
