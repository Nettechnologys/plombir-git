//! SeaORM entity for `notification_settings`: which kinds of notification an
//! account also wants by mail. An account without a row has the defaults —
//! [`Model::defaults_for`].

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "notification_settings")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub user_id: i64,
    pub email_review_requested: bool,
    pub email_mention: bool,
    pub email_assigned: bool,
    pub email_ci_failed: bool,
    /// Activity in a thread the account takes part in or subscribed to.
    pub email_participating: bool,
    /// The mail a CI-triggering push sends the repository owner.
    pub email_ci_triggered: bool,
    pub updated_at: DateTimeUtc,
}

impl Model {
    /// What an account that never changed anything gets — the column defaults
    /// of the migration, spelled once more for the rows that do not exist.
    pub fn defaults_for(user_id: i64) -> Self {
        Self {
            user_id,
            email_review_requested: true,
            email_mention: true,
            email_assigned: true,
            email_ci_failed: true,
            email_participating: true,
            email_ci_triggered: false,
            updated_at: chrono::Utc::now(),
        }
    }
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
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
