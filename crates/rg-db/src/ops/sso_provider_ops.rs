//! SSO provider operations.
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::oauth_account;
use crate::entities::sso_provider;
pub use crate::entities::sso_provider::Entity;
use crate::entities::user;

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
///
/// Generic over the connection so that `update_settings` can re-read the row it
/// just wrote from inside its own transaction, rather than after the commit.
pub async fn find_by_id<C: ConnectionTrait>(
    db: &C,
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

/// What a provider write did — and, when the slug moved, what moved with it.
///
/// `oauth_accounts.provider` stores the provider's **slug**, not its id, so the
/// slug is a link and not a label. A rename that leaves those rows behind does
/// not fail: the next sign-in through the provider simply stops finding the
/// link and falls through to the merge-by-email branch or to provisioning, so
/// an edit to a *name* decides which account a person lands in. The write and
/// the carry therefore have to be one transaction, and its outcome has more
/// shapes than "a row or no row".
#[derive(Debug)]
pub enum SsoProviderUpdate {
    /// The row was overwritten in place.
    Written {
        /// Boxed because it dwarfs the other variants: the row carries
        /// seventeen columns, and an unboxed one would make every outcome of
        /// this call as wide as the widest.
        provider: Box<sso_provider::Model>,
        /// The slug the row carried before this write, read inside the same
        /// transaction — not the handler's earlier copy of it.
        previous_slug: String,
        /// `oauth_accounts` rows carried from `previous_slug` onto the new
        /// slug. Zero whenever the slug itself did not change.
        moved_identities: u64,
    },
    /// A concurrent DELETE took the row first, and nothing was written.
    Gone,
    /// The requested slug already names linked identities that this provider
    /// did not write. Carrying its own links onto that slug would put two
    /// different people's sign-ins into one `(provider, provider_user_id)`
    /// space, so nothing was written.
    SlugHoldsIdentities { held: u64 },
    /// The write moves the provider across the directory boundary — `ldap` to
    /// one of the federated types or back — while identities still reach it
    /// through the side it is leaving. Those links cannot be carried the way a
    /// rename carries them: the two sides are different columns of different
    /// tables holding different keys, and a directory binding has no
    /// `provider_user_id` to become an `oauth_accounts` row. So nothing was
    /// written.
    TypeChangeStrandsIdentities {
        /// The `provider_type` the row carries now, read inside the
        /// transaction.
        from: String,
        /// The `provider_type` the write asked for.
        to: String,
        /// Identities reaching the provider through the side it would leave.
        held: u64,
    },
}

/// Does this `provider_type` bind accounts through the directory table?
///
/// The one bit that decides where a link is stored. Everything that is not
/// `ldap` — `oauth2`, `oidc`, and any type added later — links through
/// `oauth_accounts`, so an unknown spelling falls on the federated side rather
/// than inventing a third storage nobody wrote.
fn is_directory_type(provider_type: &str) -> bool {
    provider_type == "ldap"
}

/// Identities reaching a provider through the side its current type names.
///
/// Deliberately one side and not both: the question here is what a type change
/// would *strand*, and rows sitting on the other side are already unreachable —
/// counting them would refuse a write that repairs their reachability instead of
/// breaking it. The delete guard asks the other question ("does anything still
/// point at this row at all") and counts both halves.
async fn count_identities_on<C: ConnectionTrait>(
    db: &C,
    id: i64,
    slug: &str,
    provider_type: &str,
) -> Result<u64, DbErr> {
    if is_directory_type(provider_type) {
        user::Entity::find()
            .filter(user::Column::LdapProviderId.eq(id))
            .count(db)
            .await
    } else {
        oauth_account::Entity::find()
            .filter(oauth_account::Column::Provider.eq(slug))
            .count(db)
            .await
    }
}

/// Overwrite an existing provider in one transaction, carrying the identities
/// that name it by slug.
///
/// The handler resolves the row, validates the request and re-encrypts the
/// secrets in statements of their own, so a concurrent delete can win before
/// this write. [`SsoProviderUpdate::Gone`] is that ordinary absence: it keeps
/// the outcome out of SeaORM's backend-shaped `RecordNotUpdated` error, and —
/// unlike the read-then-insert this replaced — it cannot answer a delete by
/// putting the provider back under a new id.
///
/// The slug the links were written under is read *here*, under an exclusive
/// lock, and not taken from the caller: the caller's copy comes from an earlier
/// statement, and a PATCH that lands in between would leave this one moving
/// rows off a slug that no longer exists.
pub async fn update_settings(
    db: &DatabaseConnection,
    id: i64,
    input: SsoProviderInput<'_>,
) -> Result<SsoProviderUpdate, DbErr> {
    let now = chrono::Utc::now();
    let txn = db.begin().await?;

    let Some(current) = Entity::find_by_id(id).lock_exclusive().one(&txn).await? else {
        txn.rollback().await?;
        return Ok(SsoProviderUpdate::Gone);
    };
    let previous_slug = current.slug.clone();

    // Which table an identity lands in is decided by one bit of the type, not
    // by its exact spelling: `ldap` binds an account through
    // `users.ldap_provider_id`, and every federated type links it through
    // `oauth_accounts.provider`. `oauth2` <-> `oidc` therefore keeps its links
    // where they are and is free; crossing the boundary abandons them, because
    // the login path for the new type never looks in the old table again.
    //
    // There is nothing to carry — a directory binding is a row id on a user and
    // an OAuth link is a `(provider, provider_user_id)` pair, and neither can be
    // turned into the other — so the only honest answer is to refuse while the
    // links exist, in the same spirit as the delete guard (card_7fa843b39846).
    if is_directory_type(&current.provider_type) != is_directory_type(input.provider_type) {
        let held = count_identities_on(&txn, id, &previous_slug, &current.provider_type).await?;
        if held > 0 {
            txn.rollback().await?;
            return Ok(SsoProviderUpdate::TypeChangeStrandsIdentities {
                from: current.provider_type.clone(),
                to: input.provider_type.to_string(),
                held,
            });
        }
    }

    let mut moved_identities = 0;
    if previous_slug != input.slug {
        // Rows already sitting on the target slug are somebody else's identity
        // space: no live provider can hold the slug (the caller checked the
        // UNIQUE, and this write would fail it), so they were stranded there by
        // an earlier rename. Merging our links into them would make one
        // `(provider, provider_user_id)` pair mean two people, which is a
        // refusal and not a repair.
        let held = oauth_account::Entity::find()
            .filter(oauth_account::Column::Provider.eq(input.slug))
            .count(&txn)
            .await?;
        if held > 0 {
            txn.rollback().await?;
            return Ok(SsoProviderUpdate::SlugHoldsIdentities { held });
        }

        // `updated_at` is deliberately left alone: on this row it means "when
        // this identity last signed in" (see `oauth_account_ops::touch_existing`),
        // and an administrator renaming a provider is not a sign-in.
        let carried = oauth_account::Entity::update_many()
            .col_expr(oauth_account::Column::Provider, Expr::value(input.slug))
            .filter(oauth_account::Column::Provider.eq(previous_slug.as_str()))
            .exec(&txn)
            .await?;
        moved_identities = carried.rows_affected;
    }

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
        .exec(&txn)
        .await?;
    if result.rows_affected > 1 {
        txn.rollback().await?;
        return Err(DbErr::Custom(format!(
            "sso provider update affected {} rows for id {id}",
            result.rows_affected
        )));
    }

    // MySQL may report zero rows for a write that changed nothing. The identity
    // re-read tells that apart from a delete without depending on per-backend
    // affected-row settings — and SQLite, where the read above takes no lock,
    // is the backend on which a delete can still land here. Rolling back is
    // what keeps the carried identities from being committed onto the slug of a
    // provider that is no longer there.
    let Some(provider) = find_by_id(&txn, id).await? else {
        txn.rollback().await?;
        return Ok(SsoProviderUpdate::Gone);
    };
    txn.commit().await?;
    Ok(SsoProviderUpdate::Written {
        provider: Box::new(provider),
        previous_slug,
        moved_identities,
    })
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
            matches!(outcome, SsoProviderUpdate::Gone),
            "a PATCH that matched no row must report absence so the route can answer 404, got \
             {outcome:?}"
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

        let outcome = update_settings(
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
        .expect("update the provider");
        let SsoProviderUpdate::Written {
            provider: updated,
            previous_slug,
            moved_identities,
        } = outcome
        else {
            panic!("the row is still there, so the update must write it: {outcome:?}");
        };

        assert_eq!(updated.id, provider.id, "an update must not change the id");
        assert_eq!(
            previous_slug, "dir",
            "the write must report the slug the row carried before it"
        );
        assert_eq!(
            moved_identities, 0,
            "nothing linked to this provider, so nothing had to move"
        );
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

    /// Seed an account an identity can belong to. `oauth_accounts.user_id` is a
    /// foreign key, so the link cannot be written without one.
    async fn seed_user(db: &DatabaseConnection, username: &str) -> i64 {
        crate::ops::user_ops::create_user(
            db,
            username,
            &format!("{username}@example.test"),
            "",
            username,
        )
        .await
        .expect("seed the account the identity belongs to")
        .id
    }

    /// card_0cf83ac01b31: `oauth_accounts.provider` stores the slug, so a
    /// rename that leaves those rows behind silently changes which account the
    /// next sign-in through the provider lands in — the link stops being found,
    /// and the callback falls through to merge-by-email or to provisioning.
    #[tokio::test]
    async fn renaming_a_provider_carries_the_identities_that_named_its_slug() {
        let (db, _directory) = scratch_db("sso-provider-rename-carry.db").await;
        let user_id = seed_user(&db, "carried").await;

        let provider = create(&db, input("Directory", "dir"))
            .await
            .expect("seed the provider");
        let link = crate::ops::oauth_account_ops::link(
            &db,
            user_id,
            "dir",
            "external-uid-1",
            "carried",
            "carried@example.test",
        )
        .await
        .expect("link the identity")
        .expect("the first link must be written");

        let outcome = update_settings(&db, provider.id, input("Directory", "corp"))
            .await
            .expect("rename the provider");
        let SsoProviderUpdate::Written {
            provider: renamed,
            previous_slug,
            moved_identities,
        } = outcome
        else {
            panic!("the row is there and its new slug is free: {outcome:?}");
        };
        assert_eq!(renamed.slug, "corp");
        assert_eq!(previous_slug, "dir");
        assert_eq!(moved_identities, 1, "the one existing link had to move");

        let found =
            crate::ops::oauth_account_ops::find_by_provider_and_uid(&db, "corp", "external-uid-1")
                .await
                .expect("look the identity up under the new slug")
                .expect("the link must be findable under the slug the provider now carries");
        assert_eq!(
            found.id, link.id,
            "the identity must be the same row, moved — not a second link"
        );
        assert!(
            crate::ops::oauth_account_ops::find_by_provider_and_uid(&db, "dir", "external-uid-1")
                .await
                .expect("look the identity up under the old slug")
                .is_none(),
            "nothing may be left behind on the slug the provider no longer answers to"
        );
        assert_eq!(
            crate::ops::oauth_account_ops::count_by_provider(&db, "corp")
                .await
                .expect("count the links the delete guard would count"),
            1,
            "the guard that refuses to delete a linked provider counts by slug, so the rename \
             must not blind it"
        );
    }

    /// The other half of the carry: rows already sitting on the target slug
    /// belong to somebody else's identity space. `(provider, provider_user_id)`
    /// is UNIQUE, so merging into them would either fail the constraint or —
    /// worse, for a different `provider_user_id` — put two people's sign-ins
    /// under one provider name. The rename is refused whole.
    #[tokio::test]
    async fn renaming_onto_a_slug_that_still_names_identities_writes_nothing() {
        let (db, _directory) = scratch_db("sso-provider-rename-occupied.db").await;
        let mover = seed_user(&db, "mover").await;
        let stranded = seed_user(&db, "stranded").await;

        let provider = create(&db, input("Directory", "dir"))
            .await
            .expect("seed the provider");
        crate::ops::oauth_account_ops::link(
            &db,
            mover,
            "dir",
            "external-uid-1",
            "mover",
            "mover@example.test",
        )
        .await
        .expect("link the identity that would move")
        .expect("the link must be written");
        // Left on `corp` by an earlier rename, before this carry existed.
        crate::ops::oauth_account_ops::link(
            &db,
            stranded,
            "corp",
            "external-uid-2",
            "stranded",
            "stranded@example.test",
        )
        .await
        .expect("strand an identity on the target slug")
        .expect("the link must be written");

        let outcome = update_settings(&db, provider.id, input("Directory", "corp"))
            .await
            .expect("an occupied slug is an ordinary outcome, not a database error");
        assert!(
            matches!(outcome, SsoProviderUpdate::SlugHoldsIdentities { held: 1 }),
            "the refusal must name what holds the slug, got {outcome:?}"
        );

        assert_eq!(
            find_by_id(&db, provider.id)
                .await
                .expect("read the provider back")
                .expect("a refused rename leaves the provider in place")
                .slug,
            "dir",
            "a refused rename must not write the new slug"
        );
        assert!(
            crate::ops::oauth_account_ops::find_by_provider_and_uid(&db, "dir", "external-uid-1")
                .await
                .expect("look the mover up")
                .is_some(),
            "the link that would have moved must still be on the old slug"
        );
        assert_eq!(
            crate::ops::oauth_account_ops::count_by_provider(&db, "corp")
                .await
                .expect("count the target slug"),
            1,
            "the stranded identity must be the only row on the target slug"
        );
    }

    /// card_7fa843b39846: which table holds a provider's links is decided by
    /// `provider_type`, and that field is editable. Moving a directory to a
    /// federated type leaves `users.ldap_provider_id` pointing at a row whose
    /// login path no longer reads that column at all.
    #[tokio::test]
    async fn moving_a_directory_provider_off_ldap_with_bound_accounts_is_refused() {
        let (db, _directory) = scratch_db("sso-provider-type-strand.db").await;
        let bound = seed_user(&db, "bound").await;

        let provider = create(
            &db,
            SsoProviderInput {
                provider_type: "ldap",
                ..input("Directory", "dir")
            },
        )
        .await
        .expect("seed the provider");
        let member = user::Entity::find_by_id(bound)
            .one(&db)
            .await
            .expect("read the account")
            .expect("the seeded account must exist");
        let mut member: user::ActiveModel = member.into();
        member.ldap_provider_id = Set(Some(provider.id));
        member.update(&db).await.expect("bind the account");

        let outcome = update_settings(
            &db,
            provider.id,
            SsoProviderInput {
                provider_type: "oidc",
                ..input("Directory", "dir")
            },
        )
        .await
        .expect("a type change that strands links is an outcome, not a database error");
        assert!(
            matches!(
                &outcome,
                SsoProviderUpdate::TypeChangeStrandsIdentities { from, to, held: 1 }
                    if from == "ldap" && to == "oidc"
            ),
            "the refusal must name both types and what holds the provider, got {outcome:?}"
        );
        assert_eq!(
            find_by_id(&db, provider.id)
                .await
                .expect("read the provider back")
                .expect("a refused write leaves the provider in place")
                .provider_type,
            "ldap",
            "a refused type change must not write the new type"
        );
    }

    /// The other side of the same line: `oauth2` and `oidc` link through the
    /// same column, so moving between them carries nothing and must stay
    /// allowed — a guard that refuses here would block an ordinary edit.
    #[tokio::test]
    async fn moving_between_two_federated_types_is_allowed_with_links_in_place() {
        let (db, _directory) = scratch_db("sso-provider-type-federated.db").await;
        let user_id = seed_user(&db, "federated").await;

        let provider = create(
            &db,
            SsoProviderInput {
                provider_type: "oauth2",
                ..input("Corporate", "corp")
            },
        )
        .await
        .expect("seed the provider");
        crate::ops::oauth_account_ops::link(
            &db,
            user_id,
            "corp",
            "external-uid-1",
            "federated",
            "federated@example.test",
        )
        .await
        .expect("link the identity")
        .expect("the link must be written");

        let outcome = update_settings(
            &db,
            provider.id,
            SsoProviderInput {
                provider_type: "oidc",
                ..input("Corporate", "corp")
            },
        )
        .await
        .expect("rewrite the provider");
        assert!(
            matches!(outcome, SsoProviderUpdate::Written { .. }),
            "oauth2 -> oidc strands nothing and must be written, got {outcome:?}"
        );
        assert_eq!(
            crate::ops::oauth_account_ops::count_by_provider(&db, "corp")
                .await
                .expect("count the links"),
            1,
            "the link must still be found under the slug it was written on"
        );
    }
}
