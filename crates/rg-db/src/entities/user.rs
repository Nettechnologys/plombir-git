//! User entity — maps to the `users` table.
//! Extended with LDAP/SSO/2FA fields.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "users")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    #[sea_orm(unique)]
    pub username: String,
    #[sea_orm(unique)]
    pub email: String,
    /// Argon2 hashed password (empty for LDAP/OAuth2 users)
    pub password_hash: String,
    pub display_name: Option<String>,
    pub avatar_url: Option<String>,
    pub bio: Option<String>,
    pub is_admin: bool,
    pub is_active: bool,

    // ── LDAP/SSO/2FA fields ──────────────────────────────────
    /// "local" | "ldap" | "oauth2"
    pub auth_provider: String,
    /// LDAP distinguished name (for LDAP users)
    pub ldap_dn: Option<String>,
    /// LDAP uid (for lookup)
    pub ldap_uid: Option<String>,
    /// SSO provider that owns this LDAP identity.
    pub ldap_provider_id: Option<i64>,
    /// Encrypted TOTP secret (AES-GCM), base64 encoded
    pub totp_secret: Option<String>,
    /// Whether MFA is enforced for this user
    pub mfa_enabled: bool,
    /// "totp" | "sms" | "email" | NULL
    pub mfa_type: Option<String>,
    /// JSON array of hashed backup codes, stored as TEXT
    pub backup_codes: Option<String>,
    /// Last successful login timestamp
    pub last_login_at: Option<DateTimeUtc>,
    /// Failed login attempts (for brute-force protection)
    pub login_attempts: i32,
    /// Account locked until this timestamp
    pub locked_until: Option<DateTimeUtc>,

    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub deleted_at: Option<DateTimeUtc>,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(has_many = "super::repository::Entity")]
    Repository,
    #[sea_orm(has_many = "super::ssh_key::Entity")]
    SshKey,
    #[sea_orm(has_many = "super::access_token::Entity")]
    AccessToken,
    #[sea_orm(has_many = "super::oauth_account::Entity")]
    OAuthAccount,
    #[sea_orm(has_many = "super::mfa_backup_code::Entity")]
    MfaBackupCode,
    #[sea_orm(has_many = "super::login_log::Entity")]
    LoginLog,
}

impl Related<super::repository::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Repository.def()
    }
}

impl Related<super::ssh_key::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::SshKey.def()
    }
}

impl Related<super::access_token::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::AccessToken.def()
    }
}

impl Related<super::oauth_account::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::OAuthAccount.def()
    }
}

impl Related<super::mfa_backup_code::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::MfaBackupCode.def()
    }
}

impl Related<super::login_log::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::LoginLog.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
