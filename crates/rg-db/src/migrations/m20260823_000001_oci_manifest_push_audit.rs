//! Move "who published this image" out of a column nothing reads and into the
//! journal, then drop the column.
//!
//! `oci_manifest.push_by` was written by every manifest publication and read by
//! nothing: no endpoint served it, the admin audit view never saw it, and no CLI
//! command asked for it (card_b70de2169bd6). It also could not answer the
//! question it was named for. A manifest is content-addressed, so the second
//! `docker push` of the same bytes under a new tag keeps the existing row and
//! the existing `push_by` — the person who ran that push left no trace at all.
//!
//! `audit_log` is where the rest of Plombir Git records mutations, and
//! `rg-http`'s `record_manifest_push` now writes an `oci.manifest.push` event
//! for *every* publication, including the re-tags the column could not
//! represent.
//!
//! That leaves the attribution already on disk, and a bare `DROP COLUMN` would
//! destroy it. So `up` copies each existing `push_by` into an audit row first,
//! carrying the manifest's own `created_at` rather than the migration's clock —
//! a publication that happened in June must read as June. The copy is one
//! `INSERT … SELECT`: no value round-trips through Rust, so no timestamp is
//! reformatted and no string is concatenated in a dialect-specific way.
//!
//! `down` restores the column empty. The values are in `audit_log` by then, and
//! putting them back in two places is how the two disagree later.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, Statement};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260823_000001_oci_manifest_push_audit"
    }
}

/// The historical attribution, as one statement.
///
/// `LEFT JOIN users` because `push_by` has no foreign key behind it: an account
/// deleted since the push leaves the id without a name, which is exactly what
/// `AuditActor` records for an actor whose name could not be read. `LEFT JOIN
/// oci_repository` for the same reason — a missing namespace costs the row its
/// resource name, never the row.
const BACKFILL: &str = "\
    INSERT INTO audit_log \
        (user_id, username, action, resource_type, resource_id, resource_name, details, created_at) \
    SELECT m.push_by, u.username, 'oci.manifest.push', 'oci_manifest', m.id, r.namespace, \
           '{\"source\":\"backfill:oci_manifest.push_by\"}', m.created_at \
    FROM oci_manifest m \
    LEFT JOIN users u ON u.id = m.push_by \
    LEFT JOIN oci_repository r ON r.id = m.oci_repository_id \
    WHERE m.push_by IS NOT NULL";

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        // Gated on the column, which makes the whole migration idempotent: once
        // it is gone the backfill cannot run a second time and duplicate the
        // journal it just wrote.
        if !manager.has_column("oci_manifest", "push_by").await? {
            return Ok(());
        }

        let db = manager.get_connection();
        let backend = db.get_database_backend();
        let copied = db
            .execute(Statement::from_string(backend, BACKFILL.to_string()))
            .await?;
        if copied.rows_affected() > 0 {
            tracing::info!(
                events = copied.rows_affected(),
                "recorded the historical OCI manifest publishers in the audit log before dropping \
                 `oci_manifest.push_by`"
            );
        }

        manager
            .alter_table(
                Table::alter()
                    .table(OciManifest::Table)
                    .drop_column(OciManifest::PushBy)
                    .to_owned(),
            )
            .await?;

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_column("oci_manifest", "push_by").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(OciManifest::Table)
                        .add_column(ColumnDef::new(OciManifest::PushBy).big_integer().null())
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }
}

#[derive(DeriveIden)]
enum OciManifest {
    Table,
    PushBy,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, DatabaseConnection, DbBackend};

    const SCHEMA: &str = "\
        CREATE TABLE users (id INTEGER PRIMARY KEY, username TEXT NOT NULL);\
        CREATE TABLE oci_repository (id INTEGER PRIMARY KEY, namespace TEXT NOT NULL);\
        CREATE TABLE oci_manifest (\
            id INTEGER PRIMARY KEY, oci_repository_id BIGINT NOT NULL, digest TEXT NOT NULL,\
            media_type TEXT NOT NULL, size BIGINT NOT NULL, manifest_json TEXT NOT NULL,\
            schema_version INTEGER NOT NULL, push_by BIGINT,\
            created_at TEXT NOT NULL, updated_at TEXT NOT NULL\
        );\
        CREATE TABLE audit_log (\
            id INTEGER PRIMARY KEY AUTOINCREMENT, user_id BIGINT, username TEXT,\
            action TEXT NOT NULL, resource_type TEXT, resource_id BIGINT, resource_name TEXT,\
            ip_address TEXT, user_agent TEXT, details TEXT, created_at TEXT NOT NULL\
        );\
        INSERT INTO users (id, username) VALUES (7, 'alice');\
        INSERT INTO oci_repository (id, namespace) VALUES (3, 'alice/app');\
        INSERT INTO oci_manifest \
            (id, oci_repository_id, digest, media_type, size, manifest_json, schema_version,\
             push_by, created_at, updated_at) \
        VALUES \
            (1, 3, 'sha256:aaa', 'application/vnd.oci.image.manifest.v1+json', 12, '{}', 2,\
             7, '2026-06-01T10:00:00Z', '2026-06-01T10:00:00Z'),\
            (2, 3, 'sha256:bbb', 'application/vnd.oci.image.manifest.v1+json', 12, '{}', 2,\
             404, '2026-06-02T10:00:00Z', '2026-06-02T10:00:00Z'),\
            (3, 3, 'sha256:ccc', 'application/vnd.oci.image.manifest.v1+json', 12, '{}', 2,\
             NULL, '2026-06-03T10:00:00Z', '2026-06-03T10:00:00Z');";

    async fn fixture() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(SCHEMA).await.unwrap();
        db
    }

    async fn audit_rows(
        db: &DatabaseConnection,
    ) -> Vec<(
        Option<i64>,
        Option<String>,
        Option<i64>,
        Option<String>,
        String,
    )> {
        db.query_all(Statement::from_string(
            DbBackend::Sqlite,
            "SELECT user_id, username, resource_id, resource_name, created_at FROM audit_log \
             WHERE action = 'oci.manifest.push' ORDER BY resource_id"
                .to_string(),
        ))
        .await
        .unwrap()
        .into_iter()
        .map(|row| {
            (
                row.try_get::<Option<i64>>("", "user_id").unwrap(),
                row.try_get::<Option<String>>("", "username").unwrap(),
                row.try_get::<Option<i64>>("", "resource_id").unwrap(),
                row.try_get::<Option<String>>("", "resource_name").unwrap(),
                row.try_get::<String>("", "created_at").unwrap(),
            )
        })
        .collect()
    }

    /// The attribution survives the column. Applied twice, because a migration
    /// that already ran must not fail — or duplicate — on the next boot.
    #[tokio::test]
    async fn the_publishers_reach_the_journal_before_the_column_goes() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        Migration.up(&manager).await.unwrap();

        assert!(
            !manager.has_column("oci_manifest", "push_by").await.unwrap(),
            "the column with no reader is still there"
        );

        let rows = audit_rows(&db).await;
        assert_eq!(
            rows.len(),
            2,
            "one event per attributed manifest, and no second copy from the second run: {rows:?}"
        );

        assert_eq!(rows[0].0, Some(7), "the actor id must survive");
        assert_eq!(
            rows[0].1.as_deref(),
            Some("alice"),
            "the actor's name is read from the account row, not invented"
        );
        assert_eq!(
            rows[0].2,
            Some(1),
            "the event must name the manifest it is about"
        );
        assert_eq!(rows[0].3.as_deref(), Some("alice/app"));
        assert_eq!(
            rows[0].4, "2026-06-01T10:00:00Z",
            "a June publication recorded with the migration's clock is a falsified record"
        );

        // An account deleted since the push: the id is all that is left, and a
        // blank name would be indistinguishable from a name that failed to load.
        assert_eq!(rows[1].0, Some(404));
        assert_eq!(rows[1].1, None);
    }

    /// The manifests themselves are untouched — this migration destroys a
    /// column, never an image.
    #[tokio::test]
    async fn every_manifest_row_survives() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();

        let remaining = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM oci_manifest".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(remaining.try_get::<i64>("", "n").unwrap(), 3);
    }

    #[tokio::test]
    async fn down_restores_the_column_empty() {
        let db = fixture().await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();

        Migration.down(&manager).await.unwrap();
        Migration.down(&manager).await.unwrap();

        assert!(manager.has_column("oci_manifest", "push_by").await.unwrap());
        let filled = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS n FROM oci_manifest WHERE push_by IS NOT NULL".to_string(),
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            filled.try_get::<i64>("", "n").unwrap(),
            0,
            "the journal is the record now; a second copy is how the two disagree later"
        );
    }
}
