//! Board entity — maps to the `boards` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "boards")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: Option<i64>,
    pub org_id: Option<i64>,
    pub name: String,
    pub description: Option<String>,
    /// The account that created the board, or `None` once that account has been
    /// deleted. The board belongs to its repository or organization, so the
    /// column is `ON DELETE SET NULL` — otherwise one departing collaborator
    /// took an entire organization's board, its columns and every card on it
    /// (`m20260805_000004_repo_config_outlives_its_author`).
    pub created_by: Option<i64>,
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
    #[sea_orm(
        belongs_to = "super::organization::Entity",
        from = "Column::OrgId",
        to = "super::organization::Column::Id"
    )]
    Organization,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::CreatedBy",
        to = "super::user::Column::Id"
    )]
    User,
    #[sea_orm(has_many = "super::board_column::Entity")]
    Columns,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}
impl Related<super::organization::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Organization.def()
    }
}
impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}
impl Related<super::board_column::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Columns.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
