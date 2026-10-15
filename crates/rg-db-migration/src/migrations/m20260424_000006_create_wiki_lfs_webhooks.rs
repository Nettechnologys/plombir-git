use sea_orm_migration::prelude::*;

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // ── Wiki pages ─────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(WikiPages::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WikiPages::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(WikiPages::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(WikiPages::Title).string().not_null())
                    .col(ColumnDef::new(WikiPages::Content).string().not_null())
                    .col(ColumnDef::new(WikiPages::Message).string().null())
                    .col(ColumnDef::new(WikiPages::AuthorId).big_integer().null())
                    .col(ColumnDef::new(WikiPages::Sha).string().null())
                    .col(
                        ColumnDef::new(WikiPages::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(WikiPages::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique index: (repo_id, title)
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_wiki_repo_title")
                    .table(WikiPages::Table)
                    .col(WikiPages::RepoId)
                    .col(WikiPages::Title)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ── LFS objects ───────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(LfsObjects::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(LfsObjects::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(LfsObjects::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(LfsObjects::Oid).string().not_null())
                    .col(ColumnDef::new(LfsObjects::Size).big_integer().not_null())
                    .col(
                        ColumnDef::new(LfsObjects::Uploaded)
                            .boolean()
                            .not_null()
                            .default(false),
                    )
                    .col(
                        ColumnDef::new(LfsObjects::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Unique index: (repo_id, oid)
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_lfs_repo_oid")
                    .table(LfsObjects::Table)
                    .col(LfsObjects::RepoId)
                    .col(LfsObjects::Oid)
                    .unique()
                    .to_owned(),
            )
            .await?;

        // ── Webhooks ──────────────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(Webhooks::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(Webhooks::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(ColumnDef::new(Webhooks::RepoId).big_integer().not_null())
                    .col(ColumnDef::new(Webhooks::Url).string().not_null())
                    .col(
                        ColumnDef::new(Webhooks::ContentType)
                            .string()
                            .not_null()
                            .default("json"),
                    )
                    .col(ColumnDef::new(Webhooks::Secret).string().null())
                    .col(
                        ColumnDef::new(Webhooks::Active)
                            .boolean()
                            .not_null()
                            .default(true),
                    )
                    .col(ColumnDef::new(Webhooks::Events).string().not_null())
                    .col(
                        ColumnDef::new(Webhooks::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .col(
                        ColumnDef::new(Webhooks::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Index: repo_id
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_webhook_repo")
                    .table(Webhooks::Table)
                    .col(Webhooks::RepoId)
                    .to_owned(),
            )
            .await?;

        // ── Webhook deliveries ────────────────────────────────────────────
        manager
            .create_table(
                Table::create()
                    .table(WebhookDeliveries::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(WebhookDeliveries::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::WebhookId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(WebhookDeliveries::Event).string().not_null())
                    .col(
                        ColumnDef::new(WebhookDeliveries::DeliveryId)
                            .string()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::ResponseStatus)
                            .integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::RequestPayload)
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::ResponseBody)
                            .string()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::DurationMs)
                            .big_integer()
                            .null(),
                    )
                    .col(
                        ColumnDef::new(WebhookDeliveries::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null()
                            .default(Expr::current_timestamp()),
                    )
                    .to_owned(),
            )
            .await?;

        // Index: webhook_id
        manager
            .create_index(
                Index::create()
                    .if_not_exists()
                    .name("idx_delivery_webhook")
                    .table(WebhookDeliveries::Table)
                    .col(WebhookDeliveries::WebhookId)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(Table::drop().table(WebhookDeliveries::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(Webhooks::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(LfsObjects::Table).to_owned())
            .await?;
        manager
            .drop_table(Table::drop().table(WikiPages::Table).to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum WikiPages {
    Table,
    Id,
    RepoId,
    Title,
    Content,
    Message,
    AuthorId,
    Sha,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum LfsObjects {
    Table,
    Id,
    RepoId,
    Oid,
    Size,
    Uploaded,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Webhooks {
    Table,
    Id,
    RepoId,
    Url,
    ContentType,
    Secret,
    Active,
    Events,
    CreatedAt,
    UpdatedAt,
}

#[derive(DeriveIden)]
enum WebhookDeliveries {
    Table,
    Id,
    WebhookId,
    Event,
    DeliveryId,
    ResponseStatus,
    RequestPayload,
    ResponseBody,
    DurationMs,
    CreatedAt,
}
