//! card_56f118bbe845: a tag is a name pointing at an image, not a property of
//! it — so tags move to a table of their own.
//!
//! `oci_manifest` carried the tag as a column and indexed the row twice:
//! `UNIQUE(oci_repository_id, digest)` and `UNIQUE(oci_repository_id, tag)`.
//! One digest therefore physically could not carry two names, and the most
//! ordinary push in any CI answered `500`:
//!
//! ```text
//! docker tag app:$SHA app:latest
//! docker push app:$SHA && docker push app:latest
//! ```
//!
//! The second push wrote the same bytes, hit the digest key, and the handler
//! reported `UNKNOWN / failed to record manifest`. The same shape breaks
//! `docker push -a`, every `staging → prod` promotion by tag, and every mirror
//! that copies an image with more than one name. In the OCI Distribution spec
//! tags map many-to-one onto manifests; the schema allowed exactly one.
//!
//! After this migration `oci_manifest` is purely content-addressed — one row
//! per `(repository, digest)`, no `tag` column — and `oci_tag` holds the
//! mapping with `UNIQUE(oci_repository_id, tag)`. Moving a tag is now an update
//! of a pointer rather than a rewrite of the image row, so a re-tag can no
//! longer mutate the media type, size or push author recorded for bytes that
//! did not change.
//!
//! `down` is deliberately lossy and says so: a manifest that gained a second
//! name cannot be squeezed back into a single column, so the restored row keeps
//! the lexicographically first of its tags and the rest are dropped.

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::ConnectionTrait;

const TAG_INDEX: &str = "idx_oci_manifest_tag";
const TAG_TABLE_INDEX: &str = "idx_oci_tag_repo_tag";

#[derive(DeriveMigrationName)]
pub struct Migration;

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("oci_manifest").await? {
            return Ok(());
        }

        manager
            .create_table(
                Table::create()
                    .table(OciTag::Table)
                    .if_not_exists()
                    .col(
                        ColumnDef::new(OciTag::Id)
                            .big_integer()
                            .not_null()
                            .auto_increment()
                            .primary_key(),
                    )
                    .col(
                        ColumnDef::new(OciTag::OciRepositoryId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(ColumnDef::new(OciTag::Tag).string().not_null())
                    .col(
                        ColumnDef::new(OciTag::OciManifestId)
                            .big_integer()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OciTag::CreatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .col(
                        ColumnDef::new(OciTag::UpdatedAt)
                            .timestamp_with_time_zone()
                            .not_null(),
                    )
                    .to_owned(),
            )
            .await?;

        // The one key the registry needs: a tag names at most one image inside
        // one repository. Nothing constrains how many tags name one image,
        // which is the whole point of the table.
        if !manager.has_index("oci_tag", TAG_TABLE_INDEX).await? {
            manager
                .create_index(
                    Index::create()
                        .unique()
                        .name(TAG_TABLE_INDEX)
                        .table(OciTag::Table)
                        .col(OciTag::OciRepositoryId)
                        .col(OciTag::Tag)
                        .to_owned(),
                )
                .await?;
        }

        // Carry the existing names across before the column that held them is
        // dropped. The old unique key guarantees each `(repository, tag)`
        // appears once, so this cannot collide with the new one.
        if manager.has_column("oci_manifest", "tag").await? {
            manager
                .get_connection()
                .execute_unprepared(
                    "INSERT INTO oci_tag \
                         (oci_repository_id, tag, oci_manifest_id, created_at, updated_at) \
                     SELECT oci_repository_id, tag, id, created_at, updated_at \
                     FROM oci_manifest WHERE tag IS NOT NULL",
                )
                .await?;

            // SQLite refuses to drop an indexed column, so the index goes first
            // on every backend rather than only where it is required.
            if manager.has_index("oci_manifest", TAG_INDEX).await? {
                manager
                    .drop_index(
                        Index::drop()
                            .name(TAG_INDEX)
                            .table(OciManifest::Table)
                            .to_owned(),
                    )
                    .await?;
            }

            manager
                .alter_table(
                    Table::alter()
                        .table(OciManifest::Table)
                        .drop_column(OciManifest::Tag)
                        .to_owned(),
                )
                .await?;
        }

        Ok(())
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("oci_manifest").await? {
            return Ok(());
        }

        if !manager.has_column("oci_manifest", "tag").await? {
            manager
                .alter_table(
                    Table::alter()
                        .table(OciManifest::Table)
                        .add_column(ColumnDef::new(OciManifest::Tag).string().null())
                        .to_owned(),
                )
                .await?;

            // One column holds one name. Restoring the lexicographically first
            // tag of each manifest keeps the old unique key satisfiable — tags
            // are unique per repository, so distinct manifests receive distinct
            // names — and loses every additional name the split made possible.
            if manager.has_table("oci_tag").await? {
                manager
                    .get_connection()
                    .execute_unprepared(
                        "UPDATE oci_manifest SET tag = \
                             (SELECT MIN(t.tag) FROM oci_tag t \
                              WHERE t.oci_manifest_id = oci_manifest.id \
                                AND t.oci_repository_id = oci_manifest.oci_repository_id)",
                    )
                    .await?;
            }

            if !manager.has_index("oci_manifest", TAG_INDEX).await? {
                manager
                    .create_index(
                        Index::create()
                            .unique()
                            .name(TAG_INDEX)
                            .table(OciManifest::Table)
                            .col(OciManifest::OciRepositoryId)
                            .col(OciManifest::Tag)
                            .to_owned(),
                    )
                    .await?;
            }
        }

        manager
            .drop_table(Table::drop().table(OciTag::Table).if_exists().to_owned())
            .await?;
        Ok(())
    }
}

#[derive(DeriveIden)]
enum OciManifest {
    Table,
    OciRepositoryId,
    Tag,
}

#[derive(DeriveIden)]
enum OciTag {
    Table,
    Id,
    OciRepositoryId,
    Tag,
    OciManifestId,
    CreatedAt,
    UpdatedAt,
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::{Database, DatabaseConnection, SqlErr, Statement};

    /// The `(tag, manifest)` pairs now held by `oci_tag`, in tag order.
    async fn tag_rows(db: &DatabaseConnection) -> Vec<(String, i64)> {
        db.query_all(Statement::from_string(
            db.get_database_backend(),
            "SELECT tag, oci_manifest_id FROM oci_tag ORDER BY tag".to_string(),
        ))
        .await
        .expect("read the migrated tag rows")
        .iter()
        .map(|row| {
            (
                row.try_get::<String>("", "tag").expect("tag"),
                row.try_get::<i64>("", "oci_manifest_id")
                    .expect("oci_manifest_id"),
            )
        })
        .collect()
    }

    /// A database at the pre-migration shape, with one tagged and one untagged
    /// manifest in the same repository.
    async fn legacy_database() -> DatabaseConnection {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(
            "CREATE TABLE oci_manifest (\
                id BIGINT PRIMARY KEY, \
                oci_repository_id BIGINT NOT NULL, \
                digest VARCHAR(255) NOT NULL, \
                tag VARCHAR(255) NULL, \
                media_type VARCHAR(255) NOT NULL, \
                size BIGINT NOT NULL, \
                manifest_json TEXT NOT NULL, \
                schema_version INTEGER NOT NULL, \
                push_by BIGINT NULL, \
                created_at TIMESTAMP NOT NULL, \
                updated_at TIMESTAMP NOT NULL\
            );\
            CREATE UNIQUE INDEX idx_oci_manifest_digest \
                ON oci_manifest (oci_repository_id, digest);\
            CREATE UNIQUE INDEX idx_oci_manifest_tag \
                ON oci_manifest (oci_repository_id, tag);\
            INSERT INTO oci_manifest VALUES \
                (1, 7, 'sha256:aaa', 'v1', 'application/json', 3, '{}', 2, NULL, \
                 CURRENT_TIMESTAMP, CURRENT_TIMESTAMP), \
                (2, 7, 'sha256:bbb', NULL, 'application/json', 3, '{}', 2, NULL, \
                 CURRENT_TIMESTAMP, CURRENT_TIMESTAMP);",
        )
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn existing_tags_survive_the_move_and_the_column_is_gone() {
        let db = legacy_database().await;
        let manager = SchemaManager::new(&db);

        Migration.up(&manager).await.unwrap();
        // Re-running must neither duplicate the carried rows nor fail.
        Migration.up(&manager).await.unwrap();

        assert_eq!(
            tag_rows(&db).await,
            vec![("v1".to_string(), 1)],
            "the tagged manifest must keep its name and the untagged one must not gain one"
        );
        assert!(
            !manager.has_column("oci_manifest", "tag").await.unwrap(),
            "the tag column outlived the table that replaced it"
        );
    }

    #[tokio::test]
    async fn one_digest_now_carries_several_tags_but_a_tag_still_names_one_image() {
        let db = legacy_database().await;
        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        db.execute_unprepared(
            "INSERT INTO oci_tag \
                 (oci_repository_id, tag, oci_manifest_id, created_at, updated_at) \
             VALUES (7, 'latest', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .await
        .expect("a second tag on one image is the whole point of the split");

        let duplicate = db
            .execute_unprepared(
                "INSERT INTO oci_tag \
                     (oci_repository_id, tag, oci_manifest_id, created_at, updated_at) \
                 VALUES (7, 'latest', 2, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
            )
            .await
            .unwrap_err();
        assert!(
            matches!(
                duplicate.sql_err(),
                Some(SqlErr::UniqueConstraintViolation(_))
            ),
            "one tag must still name exactly one image: {duplicate:#}"
        );
    }

    #[tokio::test]
    async fn down_restores_one_name_per_image_and_says_which() {
        let db = legacy_database().await;
        let manager = SchemaManager::new(&db);
        Migration.up(&manager).await.unwrap();
        db.execute_unprepared(
            "INSERT INTO oci_tag \
                 (oci_repository_id, tag, oci_manifest_id, created_at, updated_at) \
             VALUES (7, 'latest', 1, CURRENT_TIMESTAMP, CURRENT_TIMESTAMP)",
        )
        .await
        .unwrap();

        Migration.down(&manager).await.unwrap();

        let restored = db
            .query_all(Statement::from_string(
                db.get_database_backend(),
                "SELECT id, tag FROM oci_manifest ORDER BY id".to_string(),
            ))
            .await
            .unwrap()
            .iter()
            .map(|row| {
                (
                    row.try_get::<i64>("", "id").unwrap(),
                    row.try_get::<Option<String>>("", "tag").unwrap(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            restored,
            vec![(1, Some("latest".to_string())), (2, None)],
            "the reversal keeps the first name of a multiply-tagged image and drops the rest"
        );
        assert!(!manager.has_table("oci_tag").await.unwrap());
    }
}
