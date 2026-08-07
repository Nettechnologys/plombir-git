//! Audit logging — the workspace's only writer of `audit_log` rows.
//!
//! # Usage
//! ```ignore
//! use rg_core::audit::{record, AuditActor};
//!
//! // Before the mutation, when a failed lookup should refuse the request:
//! let actor = AuditActor::resolve(&db, admin_id).await?;
//! // …perform the mutation…
//! record(&db, &actor, "admin.unlock_user", Some("user"), Some(target.id),
//!        Some(&target.username), Some(&headers), None).await;
//! ```
//!
//! The actor is a [`AuditActor`] and not a string on purpose — see the module
//! docs of the implementation for what that prevents.

pub mod archiver;
#[path = "audit.rs"]
mod audit_impl;

pub use audit_impl::{extract_ip_and_ua, record, AuditActor};
