//! Audit log recording helper.
//!
//! This module is intentionally simple — it validates nothing and never
//! fails the calling code. Audit failures should never block the
//! primary operation.

use sea_orm::{EntityTrait, Set};

use rg_db::entities::audit_log;

/// Record an audit event.
///
/// Any error is logged but never propagated to the caller — audit
/// failures must not block the primary operation.
#[tracing::instrument(skip(db, details), fields(action = %action))]
#[allow(clippy::too_many_arguments)]
pub async fn record(
    db: &sea_orm::DatabaseConnection,
    user_id: Option<i64>,
    username: Option<&str>,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<i64>,
    resource_name: Option<&str>,
    ip_address: Option<&str>,
    user_agent: Option<&str>,
    details: Option<&str>,
) {
    let entry = audit_log::ActiveModel {
        id: Set(0), // auto-increment
        user_id: Set(user_id),
        username: Set(username.map(str::to_string)),
        action: Set(action.to_string()),
        resource_type: Set(resource_type.map(str::to_string)),
        resource_id: Set(resource_id),
        resource_name: Set(resource_name.map(str::to_string)),
        ip_address: Set(ip_address.map(str::to_string)),
        user_agent: Set(user_agent.map(str::to_string)),
        details: Set(details.map(str::to_string)),
        created_at: Set(chrono::Utc::now()),
    };

    if let Err(e) = audit_log::Entity::insert(entry).exec(db).await {
        tracing::warn!(%action, "failed to write audit log: {}", e);
    }
}
