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

/// Delete up to `limit` login attempts recorded before `cutoff`, oldest first,
/// and return how many went.
///
/// The retention sweep's batch. Each row carries the client's IP address and
/// user agent; the admin view pages through them, and nothing decides anything
/// from rows this old — so keeping them forever is cost and exposure with no
/// use. Ids first, then a delete by key — see
/// [`crate::ops::webhook_ops::delete_deliveries_before`]. The caller bounds
/// `limit`.
pub async fn delete_before(
    db: &DatabaseConnection,
    cutoff: chrono::DateTime<chrono::Utc>,
    limit: u64,
) -> Result<u64, DbErr> {
    let ids: Vec<i64> = Entity::find()
        .select_only()
        .column(login_log::Column::Id)
        .filter(login_log::Column::CreatedAt.lt(cutoff))
        .order_by_asc(login_log::Column::CreatedAt)
        .order_by_asc(login_log::Column::Id)
        .limit(limit)
        .into_tuple()
        .all(db)
        .await?;
    if ids.is_empty() {
        return Ok(0);
    }
    let result = Entity::delete_many()
        .filter(login_log::Column::Id.is_in(ids))
        .exec(db)
        .await?;
    Ok(result.rows_affected)
}
