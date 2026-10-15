//! Push mirror entity — maps to the `push_mirrors` table.
//!
//! A remote this repository sends its branches and tags to. The opposite
//! direction of [`super::mirror`], and a repository may have several.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "push_mirrors")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    /// The remote, without a credential in it.
    pub url: String,
    pub username: Option<String>,
    /// AES-256-GCM ciphertext of the remote's password or token; decrypted for
    /// the duration of one push only.
    pub password_encrypted: Option<String>,
    /// Push after every ref move, not only on the interval.
    pub sync_on_commit: bool,
    pub sync_interval_seconds: i64,
    /// Make the remote an exact copy: force-update rewritten branches and
    /// delete the ones this repository no longer has. Off, the remote only
    /// ever moves forward.
    pub force: bool,
    /// The owner's switch. The outcome of the last pass is not here.
    pub enabled: bool,
    pub next_sync_at: Option<DateTimeUtc>,
    /// The first ref move the remote has not received yet.
    pub pending_since: Option<DateTimeUtc>,
    /// Bumped on every ref move; a pass clears `pending_since` only if this is
    /// still the value it started from.
    pub pending_revision: i64,
    pub last_sync_at: Option<DateTimeUtc>,
    pub last_success_at: Option<DateTimeUtc>,
    pub last_sync_error: Option<String>,
    pub lease_token: Option<String>,
    pub lease_acquired_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

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
