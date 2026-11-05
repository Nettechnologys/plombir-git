//! Personal delivery configuration. Never serialize the persisted secret.
use sea_orm::entity::prelude::*;

#[derive(Clone, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "notification_profiles")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub user_id: i64,
    pub locale: String,
    pub choices: String,
    pub telegram_token_encrypted: Option<String>,
    pub chat_id: Option<i64>,
    pub message_thread_id: Option<i32>,
    pub telegram_confirmed: bool,
    pub generation: i64,
    pub revision: i64,
    pub updated_at: DateTimeUtc,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationProfile")
            .field("user_id", &self.user_id)
            .field("revision", &self.revision)
            .finish_non_exhaustive()
    }
}

impl Model {
    pub fn defaults(user_id: i64) -> Self {
        Self {
            id: 0,
            user_id,
            locale: "en".into(),
            choices: "{}".into(),
            telegram_token_encrypted: None,
            chat_id: None,
            message_thread_id: None,
            telegram_confirmed: false,
            generation: 0,
            revision: 0,
            updated_at: chrono::Utc::now(),
        }
    }
    pub fn telegram_configured(&self) -> bool {
        self.telegram_token_encrypted.is_some() && self.chat_id.is_some()
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
impl ActiveModelBehavior for ActiveModel {}
