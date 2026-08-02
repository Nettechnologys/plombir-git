//! SSO provider operations.
use sea_orm::*;

use crate::entities::sso_provider;
pub use crate::entities::sso_provider::Entity;

/// List all configured SSO providers (admin use).
pub async fn list_all(db: &DatabaseConnection) -> Result<Vec<sso_provider::Model>, DbErr> {
    Entity::find()
        .order_by_asc(sso_provider::Column::Id)
        .all(db)
        .await
}

/// List only enabled providers (for login page).
pub async fn list_enabled(db: &DatabaseConnection) -> Result<Vec<sso_provider::Model>, DbErr> {
    Entity::find()
        .filter(sso_provider::Column::Enabled.eq(true))
        .order_by_asc(sso_provider::Column::Id)
        .all(db)
        .await
}

/// Find provider by slug (e.g. "github", "google").
pub async fn find_by_slug(
    db: &DatabaseConnection,
    slug: &str,
) -> Result<Option<sso_provider::Model>, DbErr> {
    Entity::find()
        .filter(sso_provider::Column::Slug.eq(slug))
        .one(db)
        .await
}

/// Find provider by id.
pub async fn find_by_id(
    db: &DatabaseConnection,
    id: i64,
) -> Result<Option<sso_provider::Model>, DbErr> {
    Entity::find_by_id(id).one(db).await
}

/// Everything an admin write decides about a provider row.
///
/// This used to be seventeen positional parameters, half of them
/// `Option<&str>`: a call site was a column of `None`s that only the compiler's
/// arity check stood behind, and every new policy field made the next reader's
/// job worse. Named fields let a caller state the four things it cares about
/// and `..Default::default()` the rest — and make `auto_provision: false`
/// impossible to confuse with the `enabled` flag two lines up.
#[derive(Debug, Default, Clone)]
pub struct SsoProviderInput<'a> {
    pub name: &'a str,
    pub slug: &'a str,
    pub provider_type: &'a str,
    pub client_id: Option<&'a str>,
    pub client_secret_enc: Option<&'a str>,
    pub discovery_url: Option<&'a str>,
    pub scopes: Option<&'a str>,
    pub ldap_host: Option<&'a str>,
    pub ldap_port: Option<i32>,
    pub ldap_bind_dn: Option<&'a str>,
    pub ldap_bind_password_enc: Option<&'a str>,
    pub ldap_base_dn: Option<&'a str>,
    pub ldap_user_filter: Option<&'a str>,
    pub enabled: bool,
    /// May a first login through this provider create an account? See
    /// [`sso_provider::Model::auto_provision`].
    pub auto_provision: bool,
    /// Canonical comma-separated email-domain allowlist, or `None` for "no
    /// restriction". Canonicalise with
    /// `rg_core::user::provisioning::normalize_email_domains` before it gets
    /// here — the matcher assumes the stored form.
    pub allowed_email_domains: Option<&'a str>,
    pub icon_url: Option<&'a str>,
}

/// Upsert a provider from admin settings.
pub async fn upsert(
    db: &DatabaseConnection,
    id: Option<i64>,
    input: SsoProviderInput<'_>,
) -> Result<sso_provider::Model, DbErr> {
    let now = chrono::Utc::now();
    if let Some(existing_id) = id {
        let some = Entity::find_by_id(existing_id).one(db).await?;
        if let Some(m) = some {
            let mut am: sso_provider::ActiveModel = m.into();
            am.name = Set(input.name.to_string());
            am.slug = Set(input.slug.to_string());
            am.provider_type = Set(input.provider_type.to_string());
            am.client_id = Set(input.client_id.map(str::to_string));
            am.client_secret_enc = Set(input.client_secret_enc.map(str::to_string));
            am.discovery_url = Set(input.discovery_url.map(str::to_string));
            am.scopes = Set(input.scopes.map(str::to_string));
            am.ldap_host = Set(input.ldap_host.map(str::to_string));
            am.ldap_port = Set(input.ldap_port);
            am.ldap_bind_dn = Set(input.ldap_bind_dn.map(str::to_string));
            am.ldap_bind_password_enc = Set(input.ldap_bind_password_enc.map(str::to_string));
            am.ldap_base_dn = Set(input.ldap_base_dn.map(str::to_string));
            am.ldap_user_filter = Set(input.ldap_user_filter.map(str::to_string));
            am.enabled = Set(input.enabled);
            am.auto_provision = Set(input.auto_provision);
            am.allowed_email_domains = Set(input.allowed_email_domains.map(str::to_string));
            am.icon_url = Set(input.icon_url.map(str::to_string));
            am.updated_at = Set(now);
            return am.update(db).await;
        }
    }

    let am = sso_provider::ActiveModel {
        id: NotSet,
        name: Set(input.name.to_string()),
        slug: Set(input.slug.to_string()),
        provider_type: Set(input.provider_type.to_string()),
        client_id: Set(input.client_id.map(str::to_string)),
        client_secret_enc: Set(input.client_secret_enc.map(str::to_string)),
        discovery_url: Set(input.discovery_url.map(str::to_string)),
        scopes: Set(input.scopes.map(str::to_string)),
        ldap_host: Set(input.ldap_host.map(str::to_string)),
        ldap_port: Set(input.ldap_port),
        ldap_bind_dn: Set(input.ldap_bind_dn.map(str::to_string)),
        ldap_bind_password_enc: Set(input.ldap_bind_password_enc.map(str::to_string)),
        ldap_base_dn: Set(input.ldap_base_dn.map(str::to_string)),
        ldap_user_filter: Set(input.ldap_user_filter.map(str::to_string)),
        enabled: Set(input.enabled),
        auto_provision: Set(input.auto_provision),
        allowed_email_domains: Set(input.allowed_email_domains.map(str::to_string)),
        icon_url: Set(input.icon_url.map(str::to_string)),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await
}

/// Delete a provider by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<(), DbErr> {
    Entity::delete_by_id(id).exec(db).await?;
    Ok(())
}
