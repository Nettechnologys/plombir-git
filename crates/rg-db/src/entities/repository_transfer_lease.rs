//! Repository transfer lease entity — maps to `repository_transfer_lease`.
//!
//! One row exists only while some request is moving a repository's storage
//! between namespaces, so the table is empty on a quiet instance. See
//! `repo_ops::bid_for_transfer_lease` for the protocol and
//! `m20260806_000002_create_repository_transfer_lease` for why it exists.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "repository_transfer_lease")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The repository whose storage move this lease covers.
    #[sea_orm(unique)]
    pub repo_id: i64,
    /// Opaque identifier of the current holder.
    pub token: String,
    /// Where the storage is moving from — for the operator, not the protocol.
    pub source_namespace: String,
    /// Where the storage is moving to — for the operator, not the protocol.
    pub destination_namespace: String,
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
