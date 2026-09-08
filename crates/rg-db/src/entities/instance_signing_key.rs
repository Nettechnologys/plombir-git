//! InstanceSigningKey entity — maps to the `instance_signing_key` table.
//! The single row (id 1) holding this instance's Ed25519 provenance identity.
use sea_orm::entity::prelude::*;

/// The primary key of the one row this table ever holds.
pub const SINGLETON_ID: i64 = 1;

/// Deliberately **not** `Serialize`/`Deserialize`, unlike every neighbouring
/// entity: `seed_encrypted` is private key material, and a serde impl is all it
/// takes for a well-meaning `Json(model)` somewhere to publish it. The row is
/// read by exactly one caller (`rg_core::auth::instance_key` — not a doc link,
/// because that crate sits above `rg-db` and cannot be named from here) and
/// never crosses an API boundary.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "instance_signing_key")]
pub struct Model {
    /// Always [`SINGLETON_ID`] — assigned by the writer, never generated.
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    /// Hex-encoded 32-byte Ed25519 seed, AES-GCM encrypted under the instance's
    /// at-rest encryption key.
    pub seed_encrypted: String,
    /// When this instance first established an identity.
    pub created_at: DateTimeUtc,
    /// When the key was last deliberately replaced; `None` if never.
    pub rotated_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
