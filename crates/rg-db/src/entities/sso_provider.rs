//! SsoProvider entity — maps to `sso_providers` table.
use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "sso_providers")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// Display name shown on login page
    pub name: String,
    /// URL-friendly slug: "github", "google", "oidc-corp"
    #[sea_orm(unique)]
    pub slug: String,
    /// "oauth2" | "oidc" | "ldap"
    pub provider_type: String,
    /// OAuth2 client ID (stored in the clear — only `client_secret_enc` below is encrypted)
    pub client_id: Option<String>,
    /// OAuth2 client secret (AES-GCM encrypted)
    pub client_secret_enc: Option<String>,
    /// OIDC discovery document URL (for OIDC providers)
    pub discovery_url: Option<String>,
    /// Space-separated scopes
    pub scopes: Option<String>,
    /// LDAP host (for LDAP providers)
    pub ldap_host: Option<String>,
    pub ldap_port: Option<i32>,
    /// LDAP bind DN (for binding)
    pub ldap_bind_dn: Option<String>,
    /// LDAP bind password (AES-GCM encrypted)
    pub ldap_bind_password_enc: Option<String>,
    /// LDAP base DN for user search
    pub ldap_base_dn: Option<String>,
    /// LDAP user filter template, e.g. "(uid={username})"
    pub ldap_user_filter: Option<String>,
    pub enabled: bool,
    /// May a first login through this provider *create* a Plombir Git account?
    ///
    /// `enabled` says the provider can be used to sign in; this says whether
    /// signing in is allowed to mint an account for someone who has none. They
    /// are different questions on a public IdP, where everyone holds a valid
    /// identity: `false` keeps already-linked accounts working and refuses the
    /// first login of a stranger.
    pub auto_provision: bool,
    /// Comma-separated email domains this provider may provision accounts for,
    /// canonicalised on write (trimmed, lowercased). `None` = no restriction.
    ///
    /// Matched on the exact domain of the address the provider asserts —
    /// `example.com` does not admit `mail.example.com`. Only consulted when
    /// `auto_provision` is on; it narrows who may be created, never who may
    /// sign in with an account they already have.
    pub allowed_email_domains: Option<String>,
    /// Icon URL for login button (optional)
    pub icon_url: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {}

impl ActiveModelBehavior for ActiveModel {}
