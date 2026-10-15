use sea_orm_migration::prelude::*;

#[derive(DeriveIden)]
enum AuditLog {
    Table,
    Id,
    UserId,
    Username,
    Action,
    ResourceType,
    ResourceId,
    ResourceName,
    IpAddress,
    UserAgent,
    Details,
    CreatedAt,
}

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260607_000011_create_audit_logs"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(AuditLog::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(AuditLog::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(AuditLog::UserId).big_integer().null())
                    .col(ColumnDef::new(AuditLog::Username).string_len(255).null())
                    .col(ColumnDef::new(AuditLog::Action).string_len(100).not_null())
                    .col(ColumnDef::new(AuditLog::ResourceType).string_len(50).null())
                    .col(ColumnDef::new(AuditLog::ResourceId).big_integer().null())
                    .col(
                        ColumnDef::new(AuditLog::ResourceName)
                            .string_len(255)
                            .null(),
                    )
                    .col(ColumnDef::new(AuditLog::IpAddress).string_len(45).null())
                    .col(ColumnDef::new(AuditLog::UserAgent).text().null())
                    .col(ColumnDef::new(AuditLog::Details).text().null())
                    .col(
                        ColumnDef::new(AuditLog::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Index for querying by user
        manager
            .create_index(
                Index::create()
                    .table(AuditLog::Table)
                    .name("idx_audit_log_user_id")
                    .col(AuditLog::UserId)
                    .to_owned(),
            )
            .await?;

        // Index for querying by action
        manager
            .create_index(
                Index::create()
                    .table(AuditLog::Table)
                    .name("idx_audit_log_action")
                    .col(AuditLog::Action)
                    .to_owned(),
            )
            .await?;

        // Index for querying by resource
        manager
            .create_index(
                Index::create()
                    .table(AuditLog::Table)
                    .name("idx_audit_log_resource")
                    .col(AuditLog::ResourceType)
                    .col(AuditLog::ResourceId)
                    .to_owned(),
            )
            .await?;

        // Index for querying by time
        manager
            .create_index(
                Index::create()
                    .table(AuditLog::Table)
                    .name("idx_audit_log_created_at")
                    .col(AuditLog::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(AuditLog::Table).to_owned())
            .await
    }
}
