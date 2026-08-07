//! Admin-only audit log query endpoints.

use std::str::FromStr;

use axum::{
    extract::{Query, State},
    http::HeaderMap,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::{api::admin::InstanceAdmin, error::AppError, AppState};

/// Parse one `start_time` / `end_time` query parameter.
///
/// An absent bound is `None` — "no bound" — and that is the only way a bound
/// may go missing. A bound that was *sent* and did not parse used to be dropped
/// through `.ok()`, and the query then ran without it: the admin who typed
/// `2026-08-01` instead of RFC 3339 while walking an incident got `200` and a
/// convincing list covering all of time, with nothing but its length to hint
/// that the window they asked for was never applied. Telling them the format is
/// wrong is the only answer that leaves them able to trust the next list.
fn parse_time_bound(
    value: Option<&str>,
    name: &str,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, AppError> {
    value
        .map(|value| {
            chrono::DateTime::<chrono::Utc>::from_str(value).map_err(|_| {
                AppError::bad_request(format!(
                    "{name} must be an ISO 8601 / RFC 3339 timestamp with an offset, \
                     e.g. 2026-08-01T00:00:00Z"
                ))
            })
        })
        .transpose()
}

/// Reject a window that cannot contain anything, rather than answering with the
/// empty list it selects — the two look identical to the caller.
fn check_time_window(
    start_time: Option<&chrono::DateTime<chrono::Utc>>,
    end_time: Option<&chrono::DateTime<chrono::Utc>>,
) -> Result<(), AppError> {
    if start_time
        .zip(end_time)
        .is_some_and(|(start, end)| start > end)
    {
        return Err(AppError::bad_request(
            "start_time must be earlier than or equal to end_time",
        ));
    }
    Ok(())
}

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    page: Option<u64>,
    /// L-4: Standardized to `per_page` (alias `page_size` for backward compat).
    #[serde(default, alias = "page_size")]
    per_page: Option<u64>,
    user_id: Option<i64>,
    action: Option<String>,
    resource_type: Option<String>,
    start_time: Option<String>, // ISO 8601
    end_time: Option<String>,   // ISO 8601
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditLogEntry {
    id: i64,
    user_id: Option<i64>,
    username: Option<String>,
    action: String,
    resource_type: Option<String>,
    resource_id: Option<i64>,
    resource_name: Option<String>,
    ip_address: Option<String>,
    details: Option<String>,
    created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct AuditLogResponse {
    total: u64,
    page: u64,
    per_page: u64,
    logs: Vec<AuditLogEntry>,
}

#[derive(Debug, Deserialize)]
pub struct LoginAttemptQuery {
    page: Option<u64>,
    #[serde(default, alias = "page_size")]
    per_page: Option<u64>,
    username: Option<String>,
    auth_provider: Option<String>,
    success: Option<bool>,
    start_time: Option<String>,
    end_time: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LoginAttemptEntry {
    id: i64,
    user_id: Option<i64>,
    username: String,
    auth_provider: String,
    ip_address: Option<String>,
    user_agent: Option<String>,
    success: bool,
    failure_reason: Option<String>,
    created_at: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct LoginAttemptResponse {
    total: u64,
    page: u64,
    per_page: u64,
    attempts: Vec<LoginAttemptEntry>,
}

/// GET /admin/audit/logs
/// List audit logs with optional filters (admin only).
#[utoipa::path(
    get,
    path = "/admin/audit/logs",
    tag = "Audit",
    params(
        ("page" = Option<u64>, Query, description = "Page number (1-based, default 1)"),
        ("per_page" = Option<u64>, Query, description = "Items per page (1-100, default 20)"),
        ("user_id" = Option<i64>, Query, description = "Filter by user ID"),
        ("action" = Option<String>, Query, description = "Filter by action"),
        ("resource_type" = Option<String>, Query, description = "Filter by resource type"),
        ("start_time" = Option<String>, Query, description = "Filter after start time (ISO 8601)"),
        ("end_time" = Option<String>, Query, description = "Filter before end time (ISO 8601)"),
    ),
    responses(
        (status = 200, description = "Paginated audit log entries", body = AuditLogResponse),
        (status = 401, description = "Admin access required"),
    ),
)]
pub async fn list_audit_logs(
    State(state): State<AppState>,
    _admin: InstanceAdmin,
    Query(q): Query<AuditLogQuery>,
) -> Result<Json<AuditLogResponse>, AppError> {
    // L-4: Standardized to 1-based page numbering (consistent with PaginationParams).
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(20).clamp(1, 100);
    // sea_orm paginator is 0-based internally, and it multiplies the index by
    // the page size without checking — so the index needs the same ceiling
    // `PaginationParams::offset` applies, or `?page=<2^62>` panics this handler
    // instead of answering it (card_8973efdcbea5).
    let page_index = crate::pagination::clamp_page_index(page - 1, per_page);

    let start_time = parse_time_bound(q.start_time.as_deref(), "start_time")?;
    let end_time = parse_time_bound(q.end_time.as_deref(), "end_time")?;
    check_time_window(start_time.as_ref(), end_time.as_ref())?;

    let (logs, total) = rg_db::ops::audit_log_ops::list_paginated(
        &state.db,
        page_index,
        per_page,
        q.user_id,
        q.action.as_deref(),
        q.resource_type.as_deref(),
        start_time,
        end_time,
    )
    .await
    .map_err(AppError::from)?;

    let logs = logs
        .into_iter()
        .map(|m| AuditLogEntry {
            id: m.id,
            user_id: m.user_id,
            username: m.username,
            action: m.action,
            resource_type: m.resource_type,
            resource_id: m.resource_id,
            resource_name: m.resource_name,
            ip_address: m.ip_address,
            details: m.details,
            created_at: m.created_at.to_rfc3339(),
        })
        .collect();

    Ok(Json(AuditLogResponse {
        total,
        page,
        per_page,
        logs,
    }))
}

/// GET /admin/login-attempts
#[utoipa::path(
    get,
    path = "/admin/login-attempts",
    tag = "Audit",
    params(
        ("page" = Option<u64>, Query, description = "Page number (1-based)"),
        ("per_page" = Option<u64>, Query, description = "Items per page (1-100)"),
        ("username" = Option<String>, Query, description = "Exact username/login filter"),
        ("auth_provider" = Option<String>, Query, description = "Authentication provider filter"),
        ("success" = Option<bool>, Query, description = "Success/failure filter"),
        ("start_time" = Option<String>, Query, description = "ISO 8601 lower bound"),
        ("end_time" = Option<String>, Query, description = "ISO 8601 upper bound"),
    ),
    responses(
        (status = 200, description = "Paginated login attempts", body = LoginAttemptResponse),
        (status = 401, description = "Admin access required"),
    ),
)]
pub async fn list_login_attempts(
    State(state): State<AppState>,
    _admin: InstanceAdmin,
    Query(q): Query<LoginAttemptQuery>,
) -> Result<Json<LoginAttemptResponse>, AppError> {
    let page = q.page.unwrap_or(1).max(1);
    let per_page = q.per_page.unwrap_or(20).clamp(1, 100);
    let start_time = parse_time_bound(q.start_time.as_deref(), "start_time")?;
    let end_time = parse_time_bound(q.end_time.as_deref(), "end_time")?;
    check_time_window(start_time.as_ref(), end_time.as_ref())?;
    let username = q
        .username
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let auth_provider = q
        .auth_provider
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty());
    if username.is_some_and(|value| value.chars().count() > 255) {
        return Err(AppError::bad_request(
            "username filter must not exceed 255 characters",
        ));
    }
    if auth_provider.is_some_and(|value| value.chars().count() > 20) {
        return Err(AppError::bad_request(
            "auth_provider filter must not exceed 20 characters",
        ));
    }
    let (attempts, total) = rg_db::ops::login_log_ops::list_paginated(
        &state.db,
        // Same ceiling as `list_audit_logs`: sea_orm's `fetch_page` multiplies
        // this by `per_page` unchecked.
        crate::pagination::clamp_page_index(page - 1, per_page),
        per_page,
        username,
        auth_provider,
        q.success,
        start_time,
        end_time,
    )
    .await
    .map_err(AppError::from)?;
    Ok(Json(LoginAttemptResponse {
        total,
        page,
        per_page,
        attempts: attempts
            .into_iter()
            .map(|attempt| LoginAttemptEntry {
                id: attempt.id,
                user_id: attempt.user_id,
                username: attempt.username,
                auth_provider: attempt.auth_provider,
                ip_address: attempt.ip_address,
                user_agent: attempt.user_agent,
                success: attempt.success,
                failure_reason: attempt.failure_reason,
                created_at: attempt.created_at.to_rfc3339(),
            })
            .collect(),
    }))
}

/// GET /admin/audit/logs/{id}
/// Fetch a single audit log entry by id (admin only).
#[utoipa::path(
    get,
    path = "/admin/audit/logs/{id}",
    tag = "Audit",
    params(
        ("id" = i64, Path, description = "Audit log entry ID"),
    ),
    responses(
        (status = 200, description = "Audit log entry details", body = AuditLogEntry),
        (status = 401, description = "Admin access required"),
        (status = 404, description = "Audit log entry not found"),
    ),
)]
pub async fn get_audit_log(
    State(state): State<AppState>,
    _admin: InstanceAdmin,
    axum::extract::Path(id): axum::extract::Path<i64>,
) -> Result<Json<AuditLogEntry>, AppError> {
    let log = rg_db::ops::audit_log_ops::find_by_id(&state.db, id)
        .await
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("audit log not found"))?;

    Ok(Json(AuditLogEntry {
        id: log.id,
        user_id: log.user_id,
        username: log.username,
        action: log.action,
        resource_type: log.resource_type,
        resource_id: log.resource_id,
        resource_name: log.resource_name,
        ip_address: log.ip_address,
        details: log.details,
        created_at: log.created_at.to_rfc3339(),
    }))
}

/// Extract client IP and User-Agent from request headers.
pub(crate) fn extract_ip_and_ua(headers: &HeaderMap) -> (Option<String>, Option<String>) {
    use axum::http::header;

    let ip_address = headers
        .get("X-Forwarded-For")
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.split(',').next())
        .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
        .map(|address| address.to_string())
        .or_else(|| {
            headers
                .get("X-Real-IP")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.trim().parse::<std::net::IpAddr>().ok())
                .map(|address| address.to_string())
        });

    let user_agent = headers
        .get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(|s| s.chars().take(512).collect());

    (ip_address, user_agent)
}

#[cfg(test)]
mod tests {
    use super::extract_ip_and_ua;
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn login_metadata_rejects_fake_ips_and_bounds_user_agents() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "x-forwarded-for",
            HeaderValue::from_static("not-an-ip, 192.0.2.1"),
        );
        headers.insert("x-real-ip", HeaderValue::from_static("2001:db8::1"));
        headers.insert(
            "user-agent",
            HeaderValue::from_str(&"a".repeat(600)).unwrap(),
        );
        let (ip, user_agent) = extract_ip_and_ua(&headers);
        assert_eq!(ip.as_deref(), Some("2001:db8::1"));
        assert_eq!(user_agent.unwrap().len(), 512);
    }
}
