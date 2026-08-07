//! Spent WebAuthn ceremony entity — maps to `webauthn_ceremony_spend`.
//!
//! One row per passkey ceremony whose challenge has been answered, kept only
//! until the cookie carrying that ceremony can no longer be unsealed. The table
//! is empty on an instance where nobody has used a passkey in the last few
//! minutes. See `webauthn_ceremony_ops::spend` for the protocol and
//! `m20260807_000003_create_webauthn_ceremony_spend` for why it exists.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "webauthn_ceremony_spend")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The server-issued identifier of the ceremony this row spends.
    #[sea_orm(unique)]
    pub ceremony_id: String,
    /// When the ceremony was answered.
    pub spent_at: DateTimeUtc,
    /// When this record may be dropped — after the ceremony's cookie has
    /// stopped unsealing, never before.
    pub expires_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
