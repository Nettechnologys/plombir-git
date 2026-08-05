//! Repository-scoped SSH deploy key.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "deploy_keys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    /// The account that added the key, or `None` once that account has been
    /// deleted. A deploy key is the repository's access, not the credential of
    /// whoever configured it, so the column is `ON DELETE SET NULL` — deleting
    /// an account must not silently disable somebody else's deployments
    /// (`m20260805_000004_repo_config_outlives_its_author`).
    pub created_by_id: Option<i64>,
    pub title: String,
    pub public_key: String,
    pub fingerprint: String,
    pub read_only: bool,
    pub created_at: DateTimeUtc,
    pub last_used_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedById",
        to = "super::user::Column::Id"
    )]
    Creator,
}

impl ActiveModelBehavior for ActiveModel {}
