//! Repository mirror entity — maps to the `mirrors` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "mirrors")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    pub url: String,
    pub username: Option<String>,
    pub password_encrypted: Option<String>,
    pub sync_interval_seconds: i64,
    pub next_sync_at: Option<DateTimeUtc>,
    pub last_sync_at: Option<DateTimeUtc>,
    pub last_sync_error: Option<String>,
    /// One column, two readers — see [`STATUS_INACTIVE`] before adding a third.
    pub status: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

/// The operator's switch, in the off position — and the **only** value that
/// keeps a row out of [`crate::ops::mirror_ops::list_due_sync`].
///
/// `status` carries two things at once: what the operator asked for
/// (`active` / `inactive`) and how the last pass went (`error`). That overlap
/// is only safe as long as the sweep's selection reads the *first* role alone.
/// It once read `status = "active"` instead, so the very first failed sync —
/// which writes [`STATUS_ERROR`] into this same column — took the mirror out
/// of the sweep that was supposed to retry it, permanently and silently
/// (card_770723efaa96). The outcome of a pass belongs to `last_sync_error` /
/// `last_sync_at`; only this constant decides whether a mirror is swept.
pub const STATUS_INACTIVE: &str = "inactive";

/// Switched on, last pass succeeded (or has not run yet).
pub const STATUS_ACTIVE: &str = "active";

/// Switched on, last pass failed. Still swept — the reason is in
/// `last_sync_error` and the retry is `next_sync_at` away.
pub const STATUS_ERROR: &str = "error";

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
