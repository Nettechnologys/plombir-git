//! SeaORM entity for `thread_subscriptions`: who follows an issue or a pull
//! request. `subscribed = false` is an explicit unsubscribe, kept so that
//! taking part in the thread again does not quietly undo it.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "thread_subscriptions")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i64,
    pub repo_id: i64,
    /// `issue` or `pull_request`.
    pub subject_type: String,
    pub subject_id: i64,
    pub subscribed: bool,
    /// `author`, `commented`, `mention`, `assigned`, `review_requested`,
    /// `manual`.
    pub reason: String,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id",
        on_delete = "Cascade"
    )]
    User,
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id",
        on_delete = "Cascade"
    )]
    Repository,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
