use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000014_add_ldap_provider_identity"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "ldap_provider_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(ColumnDef::new(Users::LdapProviderId).big_integer().null())
                        .to_owned(),
                )
                .await?;
        }
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("uq_users_ldap_provider_uid")
                    .table(Users::Table)
                    .col(Users::LdapProviderId)
                    .col(Users::LdapUid)
                    .unique()
                    .to_owned(),
            )
            .await?;
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_index(
                Index::drop()
                    .if_exists()
                    .name("uq_users_ldap_provider_uid")
                    .table(Users::Table)
                    .to_owned(),
            )
            .await?;
        if manager.has_column("users", "ldap_provider_id").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .drop_column(Users::LdapProviderId)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }
}

#[derive(DeriveIden)]
enum Users {
    Table,
    LdapProviderId,
    LdapUid,
}
