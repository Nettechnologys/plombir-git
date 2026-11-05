//! Durable, leased per-event/recipient/channel delivery. Old-destination
//! ciphertext exists only for a mandatory settings-change warning.
use sea_orm::entity::prelude::*;

#[derive(Clone, PartialEq, Eq, DeriveEntityModel)]
#[sea_orm(table_name = "notification_deliveries")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub event_id: String,
    pub user_id: i64,
    pub event_key: String,
    pub channel: String,
    pub metadata: String,
    pub repo_id: Option<i64>,
    pub admin_only: bool,
    pub generation: i64,
    pub destination_hash: String,
    pub previous_destination_encrypted: Option<String>,
    pub previous_token_encrypted: Option<String>,
    pub prior_locale: Option<String>,
    pub is_test: bool,
    pub status: String,
    pub attempts: i32,
    pub next_attempt_at: DateTimeUtc,
    pub lease_token: Option<String>,
    pub lease_until: Option<DateTimeUtc>,
    pub last_error: Option<String>,
    pub created_at: DateTimeUtc,
    pub expires_at: DateTimeUtc,
    pub completed_at: Option<DateTimeUtc>,
}

impl std::fmt::Debug for Model {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NotificationDelivery")
            .field("id", &self.id)
            .field("event_key", &self.event_key)
            .field("channel", &self.channel)
            .field("status", &self.status)
            .finish_non_exhaustive()
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
