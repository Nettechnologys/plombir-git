//! Migration: create `sso_providers` table for admin-configurable SSO/OIDC providers.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(SsoProviders::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(SsoProviders::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(SsoProviders::Name).string_len(50).not_null())
                    .col(
                        ColumnDef::new(SsoProviders::Slug)
                            .string_len(30)
                            .not_null()
                            .unique_key(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::ProviderType)
                            .string_len(20)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::ClientId)
                            .string_len(512)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::ClientSecretEnc)
                            .string_len(1024)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::DiscoveryUrl)
                            .string_len(512)
                            .null(),
                    )
                    .col(ColumnDef::new(SsoProviders::Scopes).string_len(255).null())
                    .col(
                        ColumnDef::new(SsoProviders::LdapHost)
                            .string_len(255)
                            .null(),
                    )
                    .col(ColumnDef::new(SsoProviders::LdapPort).integer().null())
                    .col(
                        ColumnDef::new(SsoProviders::LdapBindDn)
                            .string_len(512)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::LdapBindPasswordEnc)
                            .string_len(1024)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::LdapBaseDn)
                            .string_len(512)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::LdapUserFilter)
                            .string_len(512)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::Enabled)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(ColumnDef::new(SsoProviders::IconUrl).string_len(512).null())
                    .col(
                        ColumnDef::new(SsoProviders::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(SsoProviders::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // Seed built-in providers (disabled by default)
        // Keep this as a typed timestamp value. PostgreSQL does not implicitly
        // cast a bound TEXT parameter into a timestamp column.
        let now = chrono::Utc::now();

        // GitHub
        manager
            .exec_stmt(
                Query::insert()
                    .into_table(SsoProviders::Table)
                    .columns([
                        SsoProviders::Name,
                        SsoProviders::Slug,
                        SsoProviders::ProviderType,
                        SsoProviders::Scopes,
                        SsoProviders::Enabled,
                        SsoProviders::CreatedAt,
                        SsoProviders::UpdatedAt,
                    ])
                    .values_panic([
                        "GitHub".into(),
                        "github".into(),
                        "oauth2".into(),
                        "read:user user:email".into(),
                        false.into(),
                        now.clone().into(),
                        now.clone().into(),
                    ])
                    .to_owned(),
            )
            .await?;

        // Google
        manager
            .exec_stmt(
                Query::insert()
                    .into_table(SsoProviders::Table)
                    .columns([
                        SsoProviders::Name,
                        SsoProviders::Slug,
                        SsoProviders::ProviderType,
                        SsoProviders::Scopes,
                        SsoProviders::Enabled,
                        SsoProviders::CreatedAt,
                        SsoProviders::UpdatedAt,
                    ])
                    .values_panic([
                        "Google".into(),
                        "google".into(),
                        "oidc".into(),
                        "openid email profile".into(),
                        false.into(),
                        now.clone().into(),
                        now.clone().into(),
                    ])
                    .to_owned(),
            )
            .await?;

        // GitLab
        manager
            .exec_stmt(
                Query::insert()
                    .into_table(SsoProviders::Table)
                    .columns([
                        SsoProviders::Name,
                        SsoProviders::Slug,
                        SsoProviders::ProviderType,
                        SsoProviders::Scopes,
                        SsoProviders::Enabled,
                        SsoProviders::CreatedAt,
                        SsoProviders::UpdatedAt,
                    ])
                    .values_panic([
                        "GitLab".into(),
                        "gitlab".into(),
                        "oauth2".into(),
                        "read_user".into(),
                        false.into(),
                        now.clone().into(),
                        now.into(),
                    ])
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(SsoProviders::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum SsoProviders {
    Table,
    Id,
    Name,
    Slug,
    ProviderType,
    ClientId,
    ClientSecretEnc,
    DiscoveryUrl,
    Scopes,
    LdapHost,
    LdapPort,
    LdapBindDn,
    LdapBindPasswordEnc,
    LdapBaseDn,
    LdapUserFilter,
    Enabled,
    IconUrl,
    CreatedAt,
    UpdatedAt,
}
