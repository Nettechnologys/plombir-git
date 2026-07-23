//! PasskeyCredential entity — maps to the `passkey_credentials` table.
//! Stores a user's registered WebAuthn / passkey authenticators.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "passkey_credentials")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i64,
    /// URL-safe base64 (no padding) of the raw credential id.
    #[sea_orm(unique)]
    pub credential_id: String,
    /// Serialized `webauthn_rs::prelude::Passkey` (JSON).
    pub passkey: String,
    /// User-supplied label for the authenticator.
    pub name: String,
    pub created_at: DateTimeUtc,
    pub last_used_at: Option<DateTimeUtc>,
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
