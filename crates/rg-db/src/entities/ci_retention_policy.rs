use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ci_retention_policies")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub repo_id: i64,
    pub artifact_retention_days: i32,
    pub cache_retention_days: i32,
    pub updated_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
