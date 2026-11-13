//! InstanceSigningKey entity — maps to the `instance_signing_keys` table.
//!
//! Every Ed25519 provenance identity this instance has held: exactly one
//! `active` key signs, `retired` keys still verify what they signed, `revoked`
//! keys are named so a signature under them is refused loudly. Only the active
//! row keeps its sealed seed; the others keep the public half alone, so a later
//! database leak does not hand over a private key that once signed.
use sea_orm::entity::prelude::*;

/// The only value `active_slot` ever holds. The column is `UNIQUE` and `NULL`
/// on every non-active row, which makes "at most one active key" a database
/// invariant rather than a promise kept by code.
pub const ACTIVE_SLOT: i16 = 1;

pub const STATUS_ACTIVE: &str = "active";
pub const STATUS_RETIRED: &str = "retired";
pub const STATUS_REVOKED: &str = "revoked";

/// Generated from the operating system's RNG.
pub const ORIGIN_GENERATED: &str = "generated";
/// Derived from the `jwt_secret` of the day it was adopted
/// (`SHA-256("forgekeep-ci-oidc-ed25519-v1\0" || jwt_secret)`): anyone who knew
/// that secret can recompute the private half.
pub const ORIGIN_LEGACY_DERIVED: &str = "legacy_derived";

/// Deliberately **not** `Serialize`/`Deserialize`, unlike every neighbouring
/// entity: `seed_encrypted` is private key material, and a serde impl is all it
/// takes for a well-meaning `Json(model)` somewhere to publish it. The rows are
/// read by `rg_core::auth::instance_key` (not a doc link: that crate sits above
/// `rg-db`) and never cross an API boundary as they are.
#[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
#[sea_orm(table_name = "instance_signing_keys")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// JWKS `kid`: hex of the first 8 bytes of SHA-256(public key). `None` only
    /// on a row carried over from the singleton table until the first process
    /// holding the encryption key fills it in.
    pub kid: Option<String>,
    /// Hex-encoded 32-byte Ed25519 public key; `None` under the same condition
    /// as `kid`.
    pub public_key: Option<String>,
    /// Hex-encoded 32-byte seed, AES-GCM sealed under the at-rest encryption
    /// key. Present on the active row only.
    pub seed_encrypted: Option<String>,
    pub origin: String,
    pub status: String,
    /// [`ACTIVE_SLOT`] on the active row, `None` everywhere else.
    pub active_slot: Option<i16>,
    pub created_at: DateTimeUtc,
    pub retired_at: Option<DateTimeUtc>,
    pub revoked_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
