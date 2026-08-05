use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ci_secrets")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub repo_id: i64,
    pub name: String,
    #[serde(skip_serializing)]
    pub encrypted_value: String,
    /// The account that introduced the secret, or `None` once that account has
    /// been deleted. The secret belongs to its repository — routinely somebody
    /// else's — so the column is `ON DELETE SET NULL`: the value CI reads
    /// outlives the collaborator who set it
    /// (`m20260805_000004_repo_config_outlives_its_author`).
    pub created_by_id: Option<i64>,
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
        belongs_to = "super::user::Entity",
        from = "Column::CreatedById",
        to = "super::user::Column::Id"
    )]
    CreatedBy,
}
impl ActiveModelBehavior for ActiveModel {}
