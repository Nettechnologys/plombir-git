//! Board column entity — maps to the `board_columns` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "board_columns")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub board_id: i64,
    pub name: String,
    pub color: Option<String>,
    pub position: i32,
    pub created_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::board::Entity",
        from = "Column::BoardId",
        to = "super::board::Column::Id"
    )]
    Board,
    #[sea_orm(has_many = "super::board_card::Entity")]
    Cards,
}

impl Related<super::board::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Board.def()
    }
}
impl Related<super::board_card::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Cards.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
