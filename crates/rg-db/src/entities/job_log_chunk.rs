//! Append-only segments of a CI job log.

use sea_orm::entity::prelude::*;

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "job_log_chunks")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub job_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub start_offset: i64,
    pub end_offset: i64,
    pub content: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
