use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "ci_environment_approvals")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub job_id: i64,
    pub environment_id: i64,
    /// The account that approved the deployment, or `None` once that account
    /// has been deleted. The row is the audit record that an approval happened,
    /// so the column is `ON DELETE SET NULL` — cascading it claimed nobody ever
    /// approved (`m20260805_000004_repo_config_outlives_its_author`).
    pub approved_by: Option<i64>,
    pub created_at: DateTimeUtc,
}
#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}
impl ActiveModelBehavior for ActiveModel {}
