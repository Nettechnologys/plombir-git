use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260802_000002_add_user_session_version"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("users", "session_version").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .add_column(
                            ColumnDef::new(Users::SessionVersion)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_column("users", "session_version").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(Users::Table)
                        .drop_column(Users::SessionVersion)
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
    SessionVersion,
}
