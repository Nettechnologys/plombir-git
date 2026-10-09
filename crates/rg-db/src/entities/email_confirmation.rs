//! SeaORM entity for `email_confirmations`: one row per link mailed out to
//! prove an address — a registration waiting for it, or an account moving to
//! it. See `m20261008_000001_account_self_service`.

use sea_orm::entity::prelude::*;

/// [`Model::purpose`] of a registration waiting for its address.
pub const PURPOSE_REGISTRATION: &str = "registration";
/// [`Model::purpose`] of an account moving to a new address.
pub const PURPOSE_EMAIL_CHANGE: &str = "email_change";
/// [`Model::purpose`] of an account proving the address it already has —
/// one registered before addresses were confirmed, or on an open instance.
pub const PURPOSE_EMAIL_VERIFY: &str = "email_verify";
/// [`Model::purpose`] of a notice that carried no link — "someone tried to
/// register with your address". Its row holds no usable token; it exists so
/// the per-address mail cooldown counts notices too.
pub const PURPOSE_NOTICE: &str = "notice";

#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "email_confirmations")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = true)]
    pub id: i64,
    pub purpose: String,
    /// SHA-256 of the token in the link; the token itself is never stored.
    pub token_hash: String,
    pub email: String,
    pub user_id: Option<i64>,
    pub username: Option<String>,
    /// The Argon2 hash a pending registration's account is created with. Not
    /// serialisable on purpose: nothing about this row is an API answer.
    pub password_hash: Option<String>,
    pub expires_at: chrono::DateTime<chrono::Utc>,
    pub created_at: chrono::DateTime<chrono::Utc>,
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
