//! Time entry entity — maps to the `time_entries` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "time_entries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub issue_id: i64,
    /// The account that logged the time, or `None` once that account has been
    /// deleted. The entry is also read as a property of the issue —
    /// `time_entry_ops::total_minutes_by_issue` sums it — so the column is
    /// `ON DELETE SET NULL`: deleting an account must not silently lower the
    /// hours recorded against an issue in somebody else's repository
    /// (`m20260805_000004_repo_config_outlives_its_author`).
    pub user_id: Option<i64>,
    pub duration_minutes: i64,
    pub description: Option<String>,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::issue::Entity",
        from = "Column::IssueId",
        to = "super::issue::Column::Id"
    )]
    Issue,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl Related<super::issue::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Issue.def()
    }
}
impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
