//! AuditLog entity — records all mutating operations for compliance/debug.
//!
//! Distinct from `login_log` (authentication events only).
//! `audit_log` captures any operation: user creates repo, pushes code,
//! creates issue, merges PR, changes settings, etc.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq, serde::Serialize)]
#[sea_orm(table_name = "audit_log")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    /// `NULL` for system actions where no user is associated.
    pub user_id: Option<i64>,
    /// Denormalised for quick display / filtering without JOIN.
    pub username: Option<String>,
    /// Machine-readable action identifier, e.g. `"repo.create"`,
    /// `"issue.update"`, `"pr.merge"`, `"user.delete"`.
    pub action: String,
    /// Type of the affected resource, e.g. `"repo"`, `"issue"`,
    /// `"pr"`, `"user"`, `"webhook"`.
    pub resource_type: Option<String>,
    /// `id` of the affected row, if applicable.
    pub resource_id: Option<i64>,
    /// Human-readable name of the affected resource (e.g. repo full_name).
    pub resource_name: Option<String>,
    /// Client IP address (IPv4 or IPv6 string).
    pub ip_address: Option<String>,
    /// `User-Agent` header from the request that triggered the action.
    pub user_agent: Option<String>,
    /// Free-form JSON or text with additional context (e.g. changed fields).
    pub details: Option<String>,
    pub created_at: chrono::DateTime<chrono::Utc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
