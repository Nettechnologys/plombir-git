//! Create durable Issue, pull-request and comment attachments.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::DatabaseBackend;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260714_000001_create_attachments"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if manager.has_table("attachments").await? {
            return Ok(());
        }
        manager.create_table(attachments_table_statement()).await?;

        manager
            .create_index(blob_key_index_statement(manager.get_database_backend()))
            .await?;

        for (name, columns) in [
            ("idx_attachments_repo", vec![Attachments::RepoId]),
            ("idx_attachments_issue", vec![Attachments::IssueId]),
            ("idx_attachments_pr", vec![Attachments::PullRequestId]),
            (
                "idx_attachments_issue_comment",
                vec![Attachments::IssueCommentId],
            ),
            (
                "idx_attachments_review_comment",
                vec![Attachments::ReviewCommentId],
            ),
        ] {
            let mut index = Index::create();
            index.name(name).table(Attachments::Table);
            for column in columns {
                index.col(column);
            }
            manager.create_index(index.to_owned()).await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        manager
            .drop_table(
                Table::drop()
                    .table(Attachments::Table)
                    .if_exists()
                    .to_owned(),
            )
            .await
    }
}

fn attachments_table_statement() -> TableCreateStatement {
    Table::create()
        .table(Attachments::Table)
        .if_not_exists()
        .col(
            ColumnDef::new(Attachments::Id)
                .big_integer()
                .not_null()
                .auto_increment()
                .primary_key(),
        )
        .col(
            ColumnDef::new(Attachments::Uuid)
                .string_len(36)
                .not_null()
                .unique_key(),
        )
        .col(ColumnDef::new(Attachments::RepoId).big_integer().not_null())
        .col(
            ColumnDef::new(Attachments::UploaderId)
                .big_integer()
                .not_null(),
        )
        .col(ColumnDef::new(Attachments::IssueId).big_integer().null())
        .col(
            ColumnDef::new(Attachments::PullRequestId)
                .big_integer()
                .null(),
        )
        .col(
            ColumnDef::new(Attachments::IssueCommentId)
                .big_integer()
                .null(),
        )
        .col(
            ColumnDef::new(Attachments::ReviewCommentId)
                .big_integer()
                .null(),
        )
        .col(
            ColumnDef::new(Attachments::Filename)
                .string_len(255)
                .not_null(),
        )
        .col(
            ColumnDef::new(Attachments::BlobKey)
                .string_len(1024)
                .not_null(),
        )
        .col(
            ColumnDef::new(Attachments::ContentType)
                .string_len(255)
                .not_null(),
        )
        .col(ColumnDef::new(Attachments::Size).big_integer().not_null())
        .col(
            ColumnDef::new(Attachments::DownloadCount)
                .big_integer()
                .not_null()
                .default(0),
        )
        .col(
            ColumnDef::new(Attachments::CreatedAt)
                .timestamp_with_time_zone()
                .not_null(),
        )
        .foreign_key(
            ForeignKey::create()
                .from(Attachments::Table, Attachments::RepoId)
                .to(Repositories::Table, Repositories::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .from(Attachments::Table, Attachments::UploaderId)
                .to(Users::Table, Users::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .from(Attachments::Table, Attachments::IssueId)
                .to(Issues::Table, Issues::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .from(Attachments::Table, Attachments::PullRequestId)
                .to(PullRequests::Table, PullRequests::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .from(Attachments::Table, Attachments::IssueCommentId)
                .to(IssueComments::Table, IssueComments::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .foreign_key(
            ForeignKey::create()
                .from(Attachments::Table, Attachments::ReviewCommentId)
                .to(ReviewComments::Table, ReviewComments::Id)
                .on_delete(ForeignKeyAction::Cascade),
        )
        .to_owned()
}

fn blob_key_index_statement(backend: DatabaseBackend) -> IndexCreateStatement {
    let mut index = Index::create();
    index
        .name("idx_attachments_blob_key")
        .table(Attachments::Table)
        .unique();
    if backend == DatabaseBackend::MySql {
        // InnoDB limits one index key to 3072 bytes. With utf8mb4, indexing the
        // complete VARCHAR(1024) can require 4096 bytes. Attachment keys place
        // their unique UUID near the beginning, so this prefix remains unique.
        index.col((Attachments::BlobKey, 768));
    } else {
        index.col(Attachments::BlobKey);
    }
    index.to_owned()
}

#[derive(DeriveIden)]
enum Attachments {
    Table,
    Id,
    Uuid,
    RepoId,
    UploaderId,
    IssueId,
    PullRequestId,
    IssueCommentId,
    ReviewCommentId,
    Filename,
    BlobKey,
    ContentType,
    Size,
    DownloadCount,
    CreatedAt,
}

#[derive(DeriveIden)]
enum Repositories {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum Users {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum Issues {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum PullRequests {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum IssueComments {
    Table,
    Id,
}
#[derive(DeriveIden)]
enum ReviewComments {
    Table,
    Id,
}

#[cfg(test)]
mod tests {
    use super::{attachments_table_statement, blob_key_index_statement};
    use sea_orm_migration::sea_orm::DatabaseBackend;
    use sea_orm_migration::sea_query::{
        MysqlQueryBuilder, PostgresQueryBuilder, SqliteQueryBuilder,
    };

    #[test]
    fn attachment_table_ddl_builds_for_all_supported_databases() {
        let sqlite = attachments_table_statement().to_string(SqliteQueryBuilder);
        let postgres = attachments_table_statement().to_string(PostgresQueryBuilder);
        let mysql = attachments_table_statement().to_string(MysqlQueryBuilder);

        for ddl in [sqlite, postgres, mysql] {
            assert!(ddl.contains("attachments"));
            assert!(ddl.contains("pull_request_id"));
            assert!(ddl.contains("review_comment_id"));
            assert!(ddl.contains("FOREIGN KEY"));
        }
    }

    #[test]
    fn attachment_blob_key_index_respects_backend_key_limits() {
        let sqlite =
            blob_key_index_statement(DatabaseBackend::Sqlite).to_string(SqliteQueryBuilder);
        let postgres =
            blob_key_index_statement(DatabaseBackend::Postgres).to_string(PostgresQueryBuilder);
        let mysql = blob_key_index_statement(DatabaseBackend::MySql).to_string(MysqlQueryBuilder);

        assert!(sqlite.contains("UNIQUE"));
        assert!(postgres.contains("UNIQUE"));
        assert!(!sqlite.contains("(768)"));
        assert!(!postgres.contains("(768)"));
        assert!(mysql.contains("UNIQUE"));
        assert!(mysql.contains("(768)"));
    }
}
