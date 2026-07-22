use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ci_environment_approvals")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub job_id: i64,
    pub environment_id: i64,
    pub approved_by: i64,
    pub created_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
