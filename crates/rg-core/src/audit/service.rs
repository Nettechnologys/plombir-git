//! Audit log service — helper to record audit events.
//!
//! Call `record()` from HTTP handlers after completing a mutating operation.

use sea_orm::DatabaseConnection;

/// Record an audit log entry.
///
/// # Example
/// ```ignore
/// use rg_core::audit::service::record;
/// record(&db, Some(user_id), "repo.create", "repo", Some(repo_id),
///        Some(&repo_full_name), Some(req.headers()), Some(json!({"visibility": "private"}))).await?;
/// ```
pub async fn record(
    db: &DatabaseConnection,
    user_id: Option<i64>,
    username: Option<&str>,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<i64>,
    resource_name: Option<&str>,
    headers: Option<&axum::http::HeaderMap>,
    details: Option<serde_json::Value>,
) -> anyhow::Result<i64> {
    use crate::audit::service::extract_ip_and_ua;
    let (ip_address, user_agent) = headers
        .map(|h| extract_ip_and_ua(h))
        .unwrap_or((None, None));

    let details_str = details.map(|v| v.to_string());

    let entry = crate::entities::audit_log::ActiveModel {
        id: sea_orm::NotSet,
        user_id: sea_orm::Set(user_id),
        username: sea_orm::Set(username.map(|s| s.to_string())),
        action: sea_orm::Set(action.to_string()),
        resource_type: sea_orm::Set(resource_type.map(|s| s.to_string())),
        resource_id: sea_orm::Set(resource_id),
        resource_name: sea_orm::Set(resource_name.map(|s| s.to_string())),
        ip_address: sea_orm::Set(ip_address),
        user_agent: sea_orm::Set(user_agent),
        details: sea_orm::Set(details_str),
        created_at: sea_orm::Set(chrono::Utc::now()),
    };

    let result = rg_db::ops::audit_log_ops::insert(db, entry)
        .await
        .map_err(|e| anyhow::anyhow!("audit insert failed: {}", e))?;

    Ok(result.id)
}

/// Extract client IP and User-Agent from request headers.
pub fn extract_ip_and_ua(headers: &axum::http::HeaderMap) -> (Option<String>, Option<String>) {
    use axum::http::header;

    let ip_address = headers
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .map(|s| s.trim().to_string())
        .or_else(|| {
            headers
                .get("X-Real-IP")
                .and_then(|v| v.to_str().ok())
                .map(|s| s.to_string())
        });

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.to_string());

    (ip_address, user_agent)
}

/// Convenience macro to record audit log.
///
/// # Examples
/// ```ignore
/// audit!(db, user, "repo.create", "repo", repo.id, &repo.full_name, headers, {
///     "visibility": "private"
/// });
/// ```
#[macro_export]
macro_rules! audit {
    ($db:expr, $user_id:expr, $username:expr, $action:expr, $resource_type:expr, $resource_id:expr, $resource_name:expr, $headers:expr, $details:tt) => {{
        let details_val = serde_json::json!($details);
        $crate::audit::service::record(
            $db,
            $user_id,
            $username,
            $action,
            $resource_type,
            $resource_id,
            $resource_name,
            $headers,
            Some(details_val),
        ).await
    }};
    ($db:expr, $user_id:expr, $username:expr, $action:expr, $resource_type:expr, $resource_id:expr, $resource_name:expr, $headers:expr) => {{
        $crate::audit::service::record(
            $db,
            $user_id,
            $username,
            $action,
            $resource_type,
            $resource_id,
            $resource_name,
            $headers,
            None,
        ).await
    }};
}
