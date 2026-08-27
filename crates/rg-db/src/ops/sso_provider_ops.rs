//! SSO provider operations.
use sea_orm::sea_query::Expr;
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

/// Insert a provider from admin settings.
///
/// This is the only statement in the crate that creates a provider row.
/// `update_settings` never falls back to an insert, so a PATCH whose row was
/// deleted underneath it cannot resurrect the provider under a fresh id.
pub async fn create(
    db: &DatabaseConnection,
    input: SsoProviderInput<'_>,
) -> Result<sso_provider::Model, DbErr> {
    let now = chrono::Utc::now();
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

/// Overwrite an existing provider in one conditional statement.
///
/// The handler resolves the row, validates the request and re-encrypts the
/// secrets in statements of their own, so a concurrent delete can win before
/// this write. `None` is that ordinary absence: it keeps the outcome out of
/// SeaORM's backend-shaped `RecordNotUpdated` error, and — unlike the
/// read-then-insert this replaced — it cannot answer a delete by putting the
/// provider back under a new id.
pub async fn update_settings(
    db: &DatabaseConnection,
    id: i64,
    input: SsoProviderInput<'_>,
) -> Result<Option<sso_provider::Model>, DbErr> {
    let now = chrono::Utc::now();
    let result = Entity::update_many()
        .col_expr(sso_provider::Column::Name, Expr::value(input.name))
        .col_expr(sso_provider::Column::Slug, Expr::value(input.slug))
        .col_expr(
            sso_provider::Column::ProviderType,
            Expr::value(input.provider_type),
        )
        .col_expr(sso_provider::Column::ClientId, Expr::value(input.client_id))
        .col_expr(
            sso_provider::Column::ClientSecretEnc,
            Expr::value(input.client_secret_enc),
        )
        .col_expr(
            sso_provider::Column::DiscoveryUrl,
            Expr::value(input.discovery_url),
        )
        .col_expr(sso_provider::Column::Scopes, Expr::value(input.scopes))
        .col_expr(sso_provider::Column::LdapHost, Expr::value(input.ldap_host))
        .col_expr(sso_provider::Column::LdapPort, Expr::value(input.ldap_port))
        .col_expr(
            sso_provider::Column::LdapBindDn,
            Expr::value(input.ldap_bind_dn),
        )
        .col_expr(
            sso_provider::Column::LdapBindPasswordEnc,
            Expr::value(input.ldap_bind_password_enc),
        )
        .col_expr(
            sso_provider::Column::LdapBaseDn,
            Expr::value(input.ldap_base_dn),
        )
        .col_expr(
            sso_provider::Column::LdapUserFilter,
            Expr::value(input.ldap_user_filter),
        )
        .col_expr(sso_provider::Column::Enabled, Expr::value(input.enabled))
        .col_expr(
            sso_provider::Column::AutoProvision,
            Expr::value(input.auto_provision),
        )
        .col_expr(
            sso_provider::Column::AllowedEmailDomains,
            Expr::value(input.allowed_email_domains),
        )
        .col_expr(sso_provider::Column::IconUrl, Expr::value(input.icon_url))
        .col_expr(sso_provider::Column::UpdatedAt, Expr::value(now))
        .filter(sso_provider::Column::Id.eq(id))
        .exec(db)
        .await?;
    match result.rows_affected {
        // MySQL may report zero rows for a write that changed nothing. The
        // identity re-read tells that apart from a delete without depending on
        // per-backend affected-row settings.
        0 | 1 => find_by_id(db, id).await,
        rows => Err(DbErr::Custom(format!(
            "sso provider update affected {rows} rows for id {id}"
        ))),
    }
}

/// Delete a provider by id, reporting whether this call removed it.
///
/// The handler reads the provider and counts its linked identities before
/// getting here, in statements of their own. `false` means a concurrent delete
/// won the race — an outcome that must not come back as `{"deleted": true}`.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool, DbErr> {
    let result = Entity::delete_by_id(id).exec(db).await?;
    Ok(result.rows_affected > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn scratch_db(name: &str) -> (DatabaseConnection, tempfile::TempDir) {
        let directory = tempfile::tempdir().expect("create temporary database directory");
        let db = crate::connect_with_pool(
            &format!(
                "sqlite://{}?mode=rwc",
                directory.path().join(name).display()
            ),
            crate::TEST_CONNECT_TIMEOUT_SECS,
            60,
            4,
        )
        .await
        .expect("connect to temporary database");
        crate::run_migrations(&db)
            .await
            .expect("migrate temporary database");
        (db, directory)
    }

    fn input<'a>(name: &'a str, slug: &'a str) -> SsoProviderInput<'a> {
        SsoProviderInput {
            name,
            slug,
            provider_type: "oidc",
            client_id: Some("client-id"),
            discovery_url: Some("https://idp.invalid/.well-known/openid-configuration"),
            scopes: Some("openid profile email"),
            enabled: true,
            ..Default::default()
        }
    }

    /// How many provider rows the instance ships with before this test writes
    /// anything: the migrations seed disabled placeholders, and "nothing was
    /// re-created" has to be measured against that, not against zero.
    async fn provider_count(db: &DatabaseConnection) -> usize {
        list_all(db).await.expect("list providers").len()
    }

    /// The delete lands between the handler's lookup and this write — the
    /// window the old read-then-insert `upsert` closed by inserting a *new*
    /// provider, silently undoing an administrator's deletion under a fresh id.
    ///
    /// The state is reproduced exactly, not approximated: the row is committed
    /// gone before the UPDATE is issued, which is what the losing PATCH sees.
    #[tokio::test]
    async fn an_update_whose_row_was_deleted_reports_absence_and_re_creates_nothing() {
        let (db, _directory) = scratch_db("sso-provider-update-race.db").await;
        let seeded = provider_count(&db).await;

        let provider = create(&db, input("Directory", "dir"))
            .await
            .expect("seed the provider a PATCH is about to target");
        assert_eq!(provider_count(&db).await, seeded + 1);

        assert!(
            delete_by_id(&db, provider.id)
                .await
                .expect("delete the provider"),
            "the concurrent DELETE must be the call that removed the row"
        );

        let outcome = update_settings(&db, provider.id, input("Renamed", "renamed"))
            .await
            .expect("an absent row is an ordinary outcome, not a database error");

        assert!(
            outcome.is_none(),
            "a PATCH that matched no row must report absence so the route can answer 404"
        );
        assert_eq!(
            provider_count(&db).await,
            seeded,
            "the update must not put the deleted provider back under a new id"
        );
        assert!(
            find_by_slug(&db, "renamed")
                .await
                .expect("look for a resurrected row")
                .is_none(),
            "no provider may exist under the slug the losing PATCH asked for"
        );
    }

    /// The other half of the same statement: when the row is still there the
    /// conditional UPDATE must overwrite it in place, keeping its id.
    #[tokio::test]
    async fn an_update_that_finds_its_row_overwrites_it_and_keeps_the_id() {
        let (db, _directory) = scratch_db("sso-provider-update.db").await;
        let seeded = provider_count(&db).await;

        let provider = create(&db, input("Directory", "dir"))
            .await
            .expect("seed the provider");

        let updated = update_settings(
            &db,
            provider.id,
            SsoProviderInput {
                enabled: false,
                auto_provision: true,
                allowed_email_domains: Some("example.test"),
                ..input("Renamed Directory", "renamed")
            },
        )
        .await
        .expect("update the provider")
        .expect("the row is still there, so the update must find it");

        assert_eq!(updated.id, provider.id, "an update must not change the id");
        assert_eq!(updated.name, "Renamed Directory");
        assert_eq!(updated.slug, "renamed");
        assert!(!updated.enabled);
        assert!(updated.auto_provision);
        assert_eq!(
            updated.allowed_email_domains.as_deref(),
            Some("example.test")
        );
        assert_eq!(
            provider_count(&db).await,
            seeded + 1,
            "an update overwrites in place and adds no row"
        );
    }
}
