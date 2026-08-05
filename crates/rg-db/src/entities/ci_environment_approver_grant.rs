use sea_orm::entity::prelude::*;

/// One live user allowed to approve jobs for a protected CI environment.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "ci_environment_approver_grants")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub environment_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::ci_environment::Entity",
        from = "Column::EnvironmentId",
        to = "super::ci_environment::Column::Id"
    )]
    Environment,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl ActiveModelBehavior for ActiveModel {}
