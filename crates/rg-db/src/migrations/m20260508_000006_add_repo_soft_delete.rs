use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260508_000006_add_repo_soft_delete"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for (column, is_timestamp) in [("deleted_at", true), ("origin_repo_id", false)] {
            if manager.has_column("repositories", column).await? {
                continue;
            }
            let mut definition = ColumnDef::new(Alias::new(column));
            if is_timestamp {
                definition.timestamp_with_time_zone();
            } else {
                definition.big_integer();
            }
            definition.null();
            manager
                .alter_table(
                    Table::alter()
                        .table(Alias::new("repositories"))
                        .add_column(&mut definition)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        for column in ["origin_repo_id", "deleted_at"] {
            if manager.has_column("repositories", column).await? {
                manager
                    .alter_table(
                        Table::alter()
                            .table(Alias::new("repositories"))
                            .drop_column(Alias::new(column))
                            .to_owned(),
                    )
                    .await?;
            }
        }

        Ok(())
    }
}
