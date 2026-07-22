//! Board card entity — maps to the `board_cards` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "board_cards")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub column_id: i64,
    pub issue_id: Option<i64>,
    pub note: Option<String>,
    pub position: i32,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::board_column::Entity",
        from = "Column::ColumnId",
        to = "super::board_column::Column::Id"
    )]
    Column,
    #[sea_orm(
        belongs_to = "super::issue::Entity",
        from = "Column::IssueId",
        to = "super::issue::Column::Id"
    )]
    Issue,
}

impl Related<super::board_column::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Column.def()
    }
}
impl Related<super::issue::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Issue.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
