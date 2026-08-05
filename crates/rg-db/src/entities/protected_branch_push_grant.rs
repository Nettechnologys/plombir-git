use sea_orm::entity::prelude::*;

/// One live user allowed to bypass a protected branch's normal push rule.
///
/// The JSON column on `protected_branches` is retained only as a wire-compatible
/// mirror.  This relation is the database-enforced source of referential
/// integrity: both the rule and the principal cascade out of the grant.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Eq)]
#[sea_orm(table_name = "protected_branch_push_grants")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub protected_branch_id: i64,
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i64,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::protected_branch::Entity",
        from = "Column::ProtectedBranchId",
        to = "super::protected_branch::Column::Id"
    )]
    ProtectedBranch,
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl ActiveModelBehavior for ActiveModel {}
