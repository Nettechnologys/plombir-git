//! Mirror sync lease entity — maps to `mirror_sync_lease`.
//!
//! One row exists only while some pass is writing a repository's mirror clone,
//! so the table is empty on a quiet instance. See
//! `mirror_ops::bid_for_sync_lease` for the protocol and
//! `m20260807_000001_create_mirror_sync_lease` for why it exists.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "mirror_sync_lease")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The repository whose mirror clone this lease covers.
    #[sea_orm(unique)]
    pub repo_id: i64,
    /// Opaque identifier of the current holder.
    pub token: String,
    /// When the current holder took the lease — the input to expiry.
    pub since: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id",
        on_delete = "Cascade"
    )]
    Repository,
}

impl ActiveModelBehavior for ActiveModel {}
