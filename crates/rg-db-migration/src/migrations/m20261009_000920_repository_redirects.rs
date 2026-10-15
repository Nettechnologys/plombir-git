//! Where a repository's old name leads (card_e83bf21a5e5b).
//!
//! A rename or a transfer moves every storage family and keeps the row's id,
//! but the old `owner/name` answered `404` from then on — every clone URL in a
//! CI config, every bookmark, every link in an issue elsewhere broke at once.
//! A row here names the namespace and the name a repository left, and the
//! repository it went to; the repository's current name is read from its row,
//! so a chain of renames still lands on the latest one.
//!
//! A redirect is consulted only where no live repository answers, and it is
//! dropped when a repository takes the name — the new one owns the address.
//! The namespace is the owner's id, not its name: a renamed *owner* would
//! otherwise strand every redirect under it. It cascades with the repository.

use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

const UNIQUE_ADDRESS: &str = "uq_repository_redirects_address";
const BY_REPO: &str = "idx_repository_redirects_repo_id";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(RepositoryRedirects::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(RepositoryRedirects::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    // `user` or `org` — which table `namespace_id` is a row of.
                    .col(
                        ColumnDef::new(RepositoryRedirects::NamespaceKind)
                            .string_len(8)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RepositoryRedirects::NamespaceId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RepositoryRedirects::Name)
                            .string_len(255)
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RepositoryRedirects::RepoId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(RepositoryRedirects::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(RepositoryRedirects::Table, RepositoryRedirects::RepoId)
                            .to(Repositories::Table, Repositories::Id)
                            .on_delete(ForeignKeyAction::Cascade),
                    )
                    .to_owned(),
            )
            .await?;
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .unique()
                    .name(UNIQUE_ADDRESS)
                    .table(RepositoryRedirects::Table)
                    .col(RepositoryRedirects::NamespaceKind)
                    .col(RepositoryRedirects::NamespaceId)
                    .col(RepositoryRedirects::Name)
                    .to_owned(),
            )
            .await?;
        // Deleting a repository finds its redirects by this column.
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name(BY_REPO)
                    .table(RepositoryRedirects::Table)
                    .col(RepositoryRedirects::RepoId)
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(RepositoryRedirects::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum RepositoryRedirects {
    Table,
    Id,
    NamespaceKind,
    NamespaceId,
    Name,
    RepoId,
    CreatedAt,
}

#[derive(Iden)]
enum Repositories {
    Table,
    Id,
}
