use sea_orm::entity::prelude::*;

/// One live user allowed to create or update a protected tag.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "protected_tag_push_grants")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub protected_tag_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::protected_tag::Entity",
        from = "Column::ProtectedTagId",
        to = "super::protected_tag::Column::Id"
    )]
    ProtectedTag,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl ActiveModelBehavior for ActiveModel {}
