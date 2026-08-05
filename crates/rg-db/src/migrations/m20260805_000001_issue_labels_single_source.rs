//! Make `issue_labels` the only stored owner of an issue's labels.
//!
//! `issues.labels` used to duplicate the label names as JSON. Normal issue
//! writes kept the JSON and junction rows together, but imports only wrote the
//! JSON while label rename/delete only changed the normalized tables. Before
//! dropping the duplicate, copy every valid legacy name into the junction. A
//! malformed JSON value or a name without a repository label aborts the
//! migration before the column is removed: silently discarding that value
//! would turn schema cleanup into data loss.

use std::collections::{HashMap, HashSet};

use sea_orm_migration::prelude::*;
use sea_orm_migration::sea_orm::{ConnectionTrait, DbBackend, Statement};

pub struct Migration;

impl MigrationName for Migration {
    fn name(&self) -> &str {
        "m20260805_000001_issue_labels_single_source"
    }
}

#[async_trait::async_trait]
impl MigrationTrait for Migration {
    async fn up(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("issues").await? || !manager.has_column("issues", "labels").await? {
            return Ok(());
        }

        backfill_issue_labels(manager).await?;

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("issues"))
                    .drop_column(Alias::new("labels"))
                    .to_owned(),
            )
            .await
    }

    async fn down(&self, manager: &SchemaManager) -> Result<(), DbErr> {
        if !manager.has_table("issues").await? || manager.has_column("issues", "labels").await? {
            return Ok(());
        }

        manager
            .alter_table(
                Table::alter()
                    .table(Alias::new("issues"))
                    .add_column(ColumnDef::new(Alias::new("labels")).string().null())
                    .to_owned(),
            )
            .await?;

        restore_legacy_json(manager).await
    }
}

async fn backfill_issue_labels(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = db.get_database_backend();

    let label_rows = db
        .query_all(Statement::from_string(
            backend,
            "SELECT id, repo_id, name FROM labels".to_string(),
        ))
        .await?;
    let mut label_ids = HashMap::with_capacity(label_rows.len());
    for row in label_rows {
        let id: i64 = row.try_get("", "id")?;
        let repo_id: i64 = row.try_get("", "repo_id")?;
        let name: String = row.try_get("", "name")?;
        label_ids.insert((repo_id, name), id);
    }

    let existing_rows = db
        .query_all(Statement::from_string(
            backend,
            "SELECT issue_id, label_id FROM issue_labels".to_string(),
        ))
        .await?;
    let mut existing = HashSet::with_capacity(existing_rows.len());
    for row in existing_rows {
        existing.insert((
            row.try_get::<i64>("", "issue_id")?,
            row.try_get::<i64>("", "label_id")?,
        ));
    }

    // Validate the entire legacy copy before writing any rows. A bad value
    // leaves the old column intact and does not produce a half-backfilled
    // junction on databases whose migration runner is not transactional.
    let issue_rows = db
        .query_all(Statement::from_string(
            backend,
            "SELECT id, repo_id, labels FROM issues WHERE labels IS NOT NULL".to_string(),
        ))
        .await?;
    let mut pending = Vec::new();
    for row in issue_rows {
        let issue_id: i64 = row.try_get("", "id")?;
        let repo_id: i64 = row.try_get("", "repo_id")?;
        let json: String = row.try_get("", "labels")?;
        let names: Vec<String> = serde_json::from_str(&json).map_err(|error| {
            DbErr::Custom(format!(
                "cannot remove issues.labels: issue {issue_id} contains invalid label JSON: {error}"
            ))
        })?;

        let mut seen_names = HashSet::new();
        for name in names
            .into_iter()
            .filter(|name| seen_names.insert(name.clone()))
        {
            let label_id = label_ids
                .get(&(repo_id, name.clone()))
                .copied()
                .ok_or_else(|| {
                    DbErr::Custom(format!(
                        "cannot remove issues.labels: issue {issue_id} references missing label {name:?} in repository {repo_id}"
                    ))
                })?;
            if existing.insert((issue_id, label_id)) {
                pending.push((issue_id, label_id));
            }
        }
    }

    for (issue_id, label_id) in pending {
        db.execute(Statement::from_sql_and_values(
            backend,
            match backend {
                DbBackend::Postgres => {
                    "INSERT INTO issue_labels (issue_id, label_id, created_at) VALUES ($1, $2, CURRENT_TIMESTAMP)"
                }
                _ => {
                    "INSERT INTO issue_labels (issue_id, label_id, created_at) VALUES (?, ?, CURRENT_TIMESTAMP)"
                }
            },
            [issue_id.into(), label_id.into()],
        ))
        .await?;
    }

    Ok(())
}

async fn restore_legacy_json(manager: &SchemaManager<'_>) -> Result<(), DbErr> {
    let db = manager.get_connection();
    let backend = db.get_database_backend();
    let rows = db
        .query_all(Statement::from_string(
            backend,
            "SELECT il.issue_id, l.name FROM issue_labels il \
             JOIN labels l ON l.id = il.label_id ORDER BY il.id"
                .to_string(),
        ))
        .await?;

    let mut names_by_issue: HashMap<i64, Vec<String>> = HashMap::new();
    for row in rows {
        names_by_issue
            .entry(row.try_get("", "issue_id")?)
            .or_default()
            .push(row.try_get("", "name")?);
    }

    for (issue_id, names) in names_by_issue {
        let json = serde_json::to_string(&names)
            .map_err(|error| DbErr::Custom(format!("serialize restored issue labels: {error}")))?;
        db.execute(Statement::from_sql_and_values(
            backend,
            match backend {
                DbBackend::Postgres => "UPDATE issues SET labels = $1 WHERE id = $2",
                _ => "UPDATE issues SET labels = ? WHERE id = ?",
            },
            [json.into(), issue_id.into()],
        ))
        .await?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm_migration::sea_orm::Database;

    const SCHEMA: &str = "\
        CREATE TABLE issues (id INTEGER PRIMARY KEY, repo_id BIGINT NOT NULL, labels TEXT);\
        CREATE TABLE labels (id INTEGER PRIMARY KEY, repo_id BIGINT NOT NULL, name TEXT NOT NULL);\
        CREATE TABLE issue_labels (\
            id INTEGER PRIMARY KEY AUTOINCREMENT,\
            issue_id BIGINT NOT NULL, label_id BIGINT NOT NULL, created_at TEXT NOT NULL,\
            UNIQUE(issue_id, label_id)\
        );";

    #[tokio::test]
    async fn legacy_json_is_backfilled_before_the_duplicate_column_is_dropped() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(&format!(
            "{SCHEMA}\
             INSERT INTO labels (id, repo_id, name) VALUES (10, 7, 'bug'), (11, 7, 'help wanted');\
             INSERT INTO issues (id, repo_id, labels) VALUES\
               (1, 7, '[\"bug\",\"help wanted\",\"bug\"]'), (2, 7, NULL);\
             INSERT INTO issue_labels (issue_id, label_id, created_at)\
               VALUES (1, 10, CURRENT_TIMESTAMP);"
        ))
        .await
        .unwrap();

        Migration.up(&SchemaManager::new(&db)).await.unwrap();

        assert!(!SchemaManager::new(&db)
            .has_column("issues", "labels")
            .await
            .unwrap());
        let rows = db
            .query_all(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT issue_id, label_id FROM issue_labels ORDER BY label_id".to_string(),
            ))
            .await
            .unwrap();
        let pairs: Vec<(i64, i64)> = rows
            .iter()
            .map(|row| {
                (
                    row.try_get("", "issue_id").unwrap(),
                    row.try_get("", "label_id").unwrap(),
                )
            })
            .collect();
        assert_eq!(pairs, vec![(1, 10), (1, 11)]);
    }

    #[tokio::test]
    async fn an_unresolvable_legacy_name_aborts_without_dropping_the_source() {
        let db = Database::connect("sqlite::memory:").await.unwrap();
        db.execute_unprepared(&format!(
            "{SCHEMA}\
             INSERT INTO issues (id, repo_id, labels) VALUES (1, 7, '[\"missing\"]');"
        ))
        .await
        .unwrap();

        let error = Migration
            .up(&SchemaManager::new(&db))
            .await
            .expect_err("unknown legacy label must stop the migration");
        assert!(error.to_string().contains("missing label \"missing\""));
        assert!(SchemaManager::new(&db)
            .has_column("issues", "labels")
            .await
            .unwrap());
        let count = db
            .query_one(Statement::from_string(
                DbBackend::Sqlite,
                "SELECT COUNT(*) AS count FROM issue_labels".to_string(),
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get::<i64>("", "count")
            .unwrap();
        assert_eq!(count, 0);
    }
}
