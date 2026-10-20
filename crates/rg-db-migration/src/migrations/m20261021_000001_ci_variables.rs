//! `vars.*`: the configuration variables of a repository and of an
//! organization, which an Actions workflow reads as `${{ vars.NAME }}`.
//!
//! Unlike `ci_secrets` a value is stored as written and shown back: a variable
//! is configuration, not a credential, and a job log prints it like any other
//! text. One table per owner rather than one table with two nullable owner
//! columns: a unique index treats `NULL` as distinct on every backend, and
//! "one name per repository" / "one name per organization" is the guarantee
//! the upsert relies on.
//!
//! A variable lives exactly as long as its owner: both foreign keys are
//! `ON DELETE CASCADE`, so purging a repository or an organization takes its
//! variables with it. The author is `ON DELETE SET NULL`, as for `ci_secrets`
//! (`m20260805_000004_repo_config_outlives_its_author`): the value a workflow
//! reads outlives the collaborator who set it.
//!
//! `value` is `TEXT`, which MySQL caps at 64 KiB — the API's ceiling for a
//! value; `name` is `VARCHAR(100)`, the API's ceiling for a name and short
//! enough for MySQL to index whole.

use sea_orm_migration::prelude::*;

pub struct Migration;

pub const NAME: &str = "m20261021_000001_ci_variables";
pub const REPO_UNIQUE_INDEX: &str = "uq_ci_variables_repo_name";
pub const ORG_UNIQUE_INDEX: &str = "uq_org_ci_variables_org_name";
pub const REPO_CREATED_BY_INDEX: &str = "idx_ci_variables_created_by";
pub const ORG_CREATED_BY_INDEX: &str = "idx_org_ci_variables_created_by";

impl MigrationName for Migration {
    fn name(&self) -> &str {
        NAME
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(CiVariables::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(CiVariables::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(CiVariables::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(CiVariables::Name).string_len(100).not_null())
                    .col(ColumnDef::new(CiVariables::Value).text().not_null())
                    .col(
                        ColumnDef::new(CiVariables::CreatedById)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(CiVariables::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(CiVariables::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ci_variables_repo")
                            .from(CiVariables::Table, CiVariables::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_ci_variables_created_by")
                            .from(CiVariables::Table, CiVariables::CreatedById)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(REPO_UNIQUE_INDEX)
                    .table(CiVariables::Table)
                    .col(CiVariables::RepoId)
                    .col(CiVariables::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(REPO_CREATED_BY_INDEX)
                    .table(CiVariables::Table)
                    .col(CiVariables::CreatedById)
                    .to_owned(),
            )
            .await?;

        manager
            .create_table(
                Table::create()
                    .table(OrgCiVariables::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OrgCiVariables::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OrgCiVariables::OrgId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OrgCiVariables::Name)
                            .string_len(100)
                            .not_null(),
                    )
                    .col(ColumnDef::new(OrgCiVariables::Value).text().not_null())
                    .col(
                        ColumnDef::new(OrgCiVariables::CreatedById)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(OrgCiVariables::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OrgCiVariables::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_org_ci_variables_org")
                            .from(OrgCiVariables::Table, OrgCiVariables::OrgId)
                            .to(Organizations::Table, Organizations::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .name("fk_org_ci_variables_created_by")
                            .from(OrgCiVariables::Table, OrgCiVariables::CreatedById)
                            .to(Users::Table, Users::Id)
                            .on_delete(ForeignKeyAction::SetNull),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(ORG_UNIQUE_INDEX)
                    .table(OrgCiVariables::Table)
                    .col(OrgCiVariables::OrgId)
                    .col(OrgCiVariables::Name)
                    .unique()
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(ORG_CREATED_BY_INDEX)
                    .table(OrgCiVariables::Table)
                    .col(OrgCiVariables::CreatedById)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(OrgCiVariables::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await?;
        manager
            .drop_table(
                Table::drop()
                    .table(CiVariables::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

#[derive(Iden)]
enum CiVariables {
    Table,
    Id,
    RepoId,
    Name,
    Value,
    CreatedById,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum OrgCiVariables {
    Table,
    Id,
    OrgId,
    Name,
    Value,
    CreatedById,
    CreatedAt,
    UpdatedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}

#[derive(Iden)]
enum Organizations {
    Table,
    Id,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
