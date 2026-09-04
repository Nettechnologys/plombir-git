//! OAuthAccount entity — maps to `oauth_accounts` table.
//!
//! The row is an *identity*, not a credential store. It once also held the
//! provider's `access_token` / `refresh_token` encrypted, and those columns were
//! dropped by `m20260822_000002_drop_oauth_account_tokens` after the endpoint
//! that read them was removed: an instance holding somebody else's live GitHub /
//! GitLab / OIDC credentials with no feature depending on them is a liability
//! and nothing more (card_51dd82b6dc82). Signing in never needed them — the
//! callback resolves an account through `(provider, provider_user_id)`.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "oauth_accounts")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The `sso_providers.slug` this identity signed in through — the row's
    /// **link** to its provider, not a label naming a kind. There is no foreign
    /// key behind it, so `sso_provider_ops::update_settings` carries these rows
    /// whenever the slug it copies moves (card_0cf83ac01b31).
    pub provider: String,
    /// Provider's user ID
    pub provider_user_id: String,
    /// Login name on provider
    pub provider_username: String,
    pub email: String,
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
