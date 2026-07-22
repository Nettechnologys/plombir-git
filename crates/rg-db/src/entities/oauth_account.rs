//! OAuthAccount entity — maps to `oauth_accounts` table.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "oauth_accounts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// "github" | "gitlab" | "google" | "oidc"
    pub provider: String,
    /// Provider's user ID
    pub provider_user_id: String,
    /// Login name on provider
    pub provider_username: String,
    pub email: String,
    /// Encrypted access token (AES-GCM)
    pub access_token: Option<String>,
    /// Encrypted refresh token
    pub refresh_token: Option<String>,
    pub token_expires_at: Option<DateTimeUtc>,
    pub user_id: i64,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
