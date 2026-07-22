//! Migration: create `login_logs` table for audit trail.
use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .create_table(
                Table::create()
                    .table(LoginLogs::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(LoginLogs::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(LoginLogs::UserId).big_integer().null())
                    .col(ColumnDef::new(LoginLogs::Username).string().not_null())
                    .col(
                        ColumnDef::new(LoginLogs::AuthProvider)
                            .string_len(20)
                            .not_null()
                            .default("local"),
                    )
                    .col(ColumnDef::new(LoginLogs::IpAddress).string_len(45).null())
                    .col(ColumnDef::new(LoginLogs::UserAgent).string_len(512).null())
                    .col(ColumnDef::new(LoginLogs::Success).boolean().not_null())
                    .col(
                        ColumnDef::new(LoginLogs::FailureReason)
                            .string_len(255)
                            .null(),
                    )
                    .col(
                        ColumnDef::new(LoginLogs::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .foreign_key(
                        ForeignKey::create()
                            .from(LoginLogs::Table, LoginLogs::UserId)
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
                    .table(LoginLogs::Table)
                    .name("idx_login_logs_user_id")
                    .col(LoginLogs::UserId)
                    .to_owned(),
            )
            .await?;

        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .table(LoginLogs::Table)
                    .name("idx_login_logs_created_at")
                    .col(LoginLogs::CreatedAt)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(LoginLogs::Table).to_owned())
            .await
    }
}

#[derive(Iden)]
enum LoginLogs {
    Table,
    Id,
    UserId,
    Username,
    AuthProvider,
    IpAddress,
    UserAgent,
    Success,
    FailureReason,
    CreatedAt,
}

#[derive(Iden)]
enum Users {
    Table,
    Id,
}
