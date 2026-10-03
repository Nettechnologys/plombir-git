//! User entity — maps to the `users` table.
//! Extended with LDAP/SSO/2FA fields.
//!
//! Three fields this struct used to carry are gone, and each was the same
//! defect: a column the server filled and never asked a question of
//! (card_b70de2169bd6). `ldap_dn` was a copy of a directory entry's place in an
//! organisation chart, while the pair that actually resolves a bind is
//! `(ldap_provider_id, ldap_uid)`. `mfa_type` labelled an enrolment the
//! challenge path never consulted — it branches on `mfa_enabled` and the stored
//! TOTP secret. `backup_codes` was the recovery store `mfa_backup_codes`
//! replaced, left holding hashes of codes no login would accept. See the
//! `m20260823_00000{2,3,4}` migrations.

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
    /// LDAP uid (for lookup)
    pub ldap_uid: Option<String>,
    /// SSO provider that owns this LDAP identity.
    pub ldap_provider_id: Option<i64>,
    /// Encrypted TOTP secret (AES-GCM), base64 encoded
    pub totp_secret: Option<String>,
    /// Encrypted TOTP secret of an enrolment that has not proved itself yet.
    ///
    /// `POST /users/mfa/setup` used to write straight into `totp_secret`, so
    /// merely re-opening the wizard replaced the factor that was holding the
    /// account: `mfa_enabled` stayed `true` against a secret no authenticator
    /// had, and the owner's only way in was a backup code (card_08400088bb40).
    /// New material waits here until [`crate::ops::user_ops::
    /// enable_mfa_with_backup_codes`] promotes it, which happens only after a
    /// code computed from it has been presented.
    pub pending_totp_secret: Option<String>,
    /// When the secret above was handed out. `NULL` = nothing in flight.
    ///
    /// A setup response is the one place the plaintext secret is ever shown, so
    /// this is what bounds how long that response stays usable to arm a factor.
    pub pending_totp_secret_at: Option<DateTimeUtc>,
    /// Whether MFA is enforced for this user
    pub mfa_enabled: bool,
    /// Newest TOTP time step this account has already spent passing the second
    /// factor. `NULL` = no TOTP login has ever completed.
    ///
    /// A TOTP code is a pure function of the secret and the clock, so nothing in
    /// the secret distinguishes a first use from a replay: with `skew = 1` over a
    /// 30-second step, one intercepted code stays valid for ~90 seconds. This is
    /// the spent state that makes a successful check *consume* something, the way
    /// `mfa_backup_code::used` does for a recovery code.
    pub totp_last_step: Option<i64>,
    /// Last successful login timestamp
    pub last_login_at: Option<DateTimeUtc>,
    /// Failed login attempts (for brute-force protection)
    pub login_attempts: i32,
    /// Account locked until this timestamp
    pub locked_until: Option<DateTimeUtc>,
    /// Monotonic generation for revoking all bearer sessions at once.
    pub session_version: i64,

    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
    pub deleted_at: Option<DateTimeUtc>,
    /// The person answerable for this account when it is a bot — an AI agent's
    /// own identity. `None` for every human account.
    ///
    /// A bot authenticates only with Personal Access Tokens its owner minted
    /// for it (`auth_provider = "bot"`, no password), and it stops working the
    /// moment its owner does: see `rg_http::pat_auth::resolve_pat`.
    pub bot_owner_id: Option<i64>,
}

impl Model {
    /// Whether this account may still authenticate and act.
    ///
    /// The single gate behind every credential check — password, SSH key,
    /// Personal Access Token, docker login, password reset. Deactivating an
    /// account is the standard answer to an offboarding or a compromise, so it
    /// has to close *every* door at once; six independent `is_active` checks
    /// scattered over six call sites are six chances to forget one, and the
    /// one that was forgotten is the one an attacker uses.
    ///
    /// `deleted_at` is folded in for the same reason: nothing soft-deletes a
    /// user today, but the column exists, and the day something does, a
    /// tombstoned account must not keep pushing over SSH.
    pub fn is_usable(&self) -> bool {
        self.is_active && self.deleted_at.is_none()
    }

    /// Whether this account is a bot owned by a person.
    pub fn is_bot(&self) -> bool {
        self.bot_owner_id.is_some()
    }
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
    #[sea_orm(has_many = "super::passkey_credential::Entity")]
    PasskeyCredential,
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

impl Related<super::passkey_credential::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PasskeyCredential.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
