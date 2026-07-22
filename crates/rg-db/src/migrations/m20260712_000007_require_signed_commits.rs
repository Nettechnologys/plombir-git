use sea_orm_migration::prelude::*;

pub struct Migration;
impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260712_000007_require_signed_commits"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager
            .has_column("protected_branches", "require_signed_commits")
            .await?
        {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(ProtectedBranches::Table)
                    .add_column(
                        ColumnDef::new(ProtectedBranches::RequireSignedCommits)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .to_owned(),
            )
            .await
    }
    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager
            .has_column("protected_branches", "require_signed_commits")
            .await?
        {
            return Ok(());
        }
        manager
            .alter_table(
                Table::alter()
                    .table(ProtectedBranches::Table)
                    .drop_column(ProtectedBranches::RequireSignedCommits)
                    .to_owned(),
            )
            .await
    }
}
#[derive(DeriveIden)]
enum ProtectedBranches {
    Table,
    RequireSignedCommits,
}
