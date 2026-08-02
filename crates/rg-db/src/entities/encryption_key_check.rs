//! EncryptionKeyCheck entity — a singleton ciphertext that proves which
//! at-rest key opens this database even before users store their first secret.
use sea_orm::entity::prelude::*;

/// The primary key of the single marker row.
pub const SINGLETON_ID: i64 = 1;

/// Deliberately not serializable: this value never belongs in an API response.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "encryption_key_check")]
pub struct Model {
    #[sea_orm(primary_key, auto_increment = false)]
    pub id: i64,
    /// A fixed domain-separated plaintext, encrypted under the current
    /// instance encryption key.
    pub value_encrypted: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
