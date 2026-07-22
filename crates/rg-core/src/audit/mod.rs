//! Audit logging module — provides [`record`] for appending audit events.
//!
//! # Usage
//! ```ignore
//! use rg_core::audit::record;
//!
//! record(&db, Some(user.id), Some(&user.username),
//!     "repo.create", Some("repo"), Some(repo.id), Some(&repo.full_name),
//!     Some(&ip), Some(&ua), Some("{}".to_string())).await?;
//! ```

pub mod archiver;
#[path = "audit.rs"]
mod audit_impl;

pub use audit_impl::record;

/// Shorthand macro so callers don't need to pass `&db` explicitly.
///
/// `$db` — `&DatabaseConnection`
/// `$user_id` — `Option<i64>`
/// `$username` — `Option<&str>`
/// `$action` — `&str`
/// remaining fields are optional (resource_type, resource_id, resource_name, ip, ua, details)
#[macro_export]
macro_rules! audit {
    ($db:expr, $user_id:expr, $username:expr, $action:expr,
     $rt:expr, $rid:expr, $rn:expr, $ip:expr, $ua:expr, $details:expr $(,)?) => {{
        let _ = $crate::audit::record(
            $db, $user_id, $username, $action, $rt, $rid, $rn, $ip, $ua, $details,
        )
        .await;
    }};
}
