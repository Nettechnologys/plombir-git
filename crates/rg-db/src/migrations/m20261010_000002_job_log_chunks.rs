//! Store streaming CI logs as individually addressable chunks.

use sea_orm_migration::prelude::*;

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20261010_000002_job_log_chunks"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("pipeline_jobs", "log_length").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(PipelineJobs::Table)
                        .add_column(
                            ColumnDef::new(PipelineJobs::LogLength)
                                .big_integer()
                                .not_null()
                                .default(0),
                        )
                        .to_owned(),
                )
                .await?;
        }

        // Old rows keep their text as a base segment. New appends never rewrite it.
        let length = if manager.get_database_backend() == sea_orm::DatabaseBackend::MySql {
            "CHAR_LENGTH(log)"
        } else {
            "LENGTH(log)"
        };
        if !manager.has_table("job_log_chunks").await? {
            manager
                .get_connection()
                .execute_unprepared(&format!(
                    "UPDATE pipeline_jobs SET log_length = {length} WHERE log IS NOT NULL"
                ))
                .await?;

            manager
                .create_table(
                    Table::create()
                        .table(JobLogChunks::Table)
                        .col(ColumnDef::new(JobLogChunks::JobId).big_integer().not_null())
                        .col(
                            ColumnDef::new(JobLogChunks::StartOffset)
                                .big_integer()
                                .not_null(),
                        )
                        .col(
                            ColumnDef::new(JobLogChunks::EndOffset)
                                .big_integer()
                                .not_null(),
                        )
                        .col(ColumnDef::new(JobLogChunks::Content).text().not_null())
                        .primary_key(
                            Index::create()
                                .col(JobLogChunks::JobId)
                                .col(JobLogChunks::StartOffset),
                        )
                        .foreign_key(
                            ForeignKey::create()
                                .from(JobLogChunks::Table, JobLogChunks::JobId)
                                .to(PipelineJobs::Table, PipelineJobs::Id)
                                .on_delete(ForeignKeyAction::Cascade),
                        )
                        .to_owned(),
                )
                .await?;
        }
        if !manager
            .has_index("job_log_chunks", "idx_job_log_chunks_end")
            .await?
        {
            manager
                .create_index(
                    Index::create()
                        .name("idx_job_log_chunks_end")
                        .table(JobLogChunks::Table)
                        .col(JobLogChunks::JobId)
                        .col(JobLogChunks::EndOffset)
                        .to_owned(),
                )
                .await?;
        }
        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        use sea_orm_migration::sea_orm::Statement;
        let db = manager.get_connection();
        let occupied = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                "SELECT 1 FROM job_log_chunks LIMIT 1".to_owned(),
            ))
            .await?
            .is_some();
        if occupied {
            return Err(DbErr::Migration(
                "cannot roll back job log chunks while streaming logs exist".to_owned(),
            ));
        }
        manager
            .drop_table(Table::drop().table(JobLogChunks::Table).to_owned())
            .await?;
        manager
            .alter_table(
                Table::alter()
                    .table(PipelineJobs::Table)
                    .drop_column(PipelineJobs::LogLength)
                    .to_owned(),
            )
            .await
    }
}

#[derive(DeriveIden)]
enum PipelineJobs {
    Table,
    Id,
    LogLength,
}

#[derive(DeriveIden)]
enum JobLogChunks {
    Table,
    JobId,
    StartOffset,
    EndOffset,
    Content,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, Statement};

    #[tokio::test]
    async fn upgrade_preserves_legacy_log_and_refuses_a_lossy_rollback() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared("CREATE TABLE pipeline_jobs (id BIGINT PRIMARY KEY, log TEXT)")
            .await
            .unwrap();
        db.execute_unprepared("INSERT INTO pipeline_jobs (id, log) VALUES (1, 'é終')")
            .await
            .unwrap();
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        let row = db
            .query_one(Statement::from_string(
                db.get_database_backend(),
                "SELECT log, log_length FROM pipeline_jobs WHERE id = 1".to_owned(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<String>("", "log").unwrap(), "é終");
        assert_eq!(row.try_get::<i64>("", "log_length").unwrap(), 2);

        db.execute_unprepared(
            "INSERT INTO job_log_chunks (job_id, start_offset, end_offset, content) \
             VALUES (1, 2, 3, 'x')",
        )
        .await
        .unwrap();
        let error = Migration.down(&manager).await.unwrap_err();
        assert!(error.to_string().contains("streaming logs exist"));
        assert!(manager.has_table("job_log_chunks").await.unwrap());
    }
}
