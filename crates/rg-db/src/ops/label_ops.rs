//! Database operations for labels.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::label::{self, ActiveModel, Entity as LabelEntity, Model as Label};

/// Find a label by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Label>> {
    LabelEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find label by id")
}

/// Find labels by IDs (batch query using IN clause).
/// Returns only existing labels — missing IDs are silently skipped.
pub async fn find_by_ids(db: &DatabaseConnection, ids: &[i64]) -> Result<Vec<Label>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    LabelEntity::find()
        .filter(label::Column::Id.is_in(ids.iter().copied()))
        .all(db)
        .await
        .context("db: find labels by ids")
}

/// List all labels for a repo.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Label>> {
    LabelEntity::find()
        .filter(label::Column::RepoId.eq(repo_id))
        .order_by_asc(label::Column::Name)
        .all(db)
        .await
        .context("db: list labels by repo")
}

/// Create a new label.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Label> {
    model.insert(db).await.context("db: create label")
}

/// Update a label.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Label> {
    model.update(db).await.context("db: update label")
}

/// Delete a label by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    LabelEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete label")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::label::ActiveModel;
    use chrono::Utc;
    use sea_orm::Set;

    async fn setup_test_db() -> DatabaseConnection {
        use sea_orm::{ConnectOptions, Database, Statement};
        let mut opt = ConnectOptions::new("sqlite::memory:");
        opt.max_connections(1);
        let db = Database::connect(opt)
            .await
            .expect("connect to in-memory db");
        crate::run_migrations(&db).await.expect("run migrations");
        // Insert minimal user + repo to satisfy FK constraints on labels
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) VALUES(1, 'test', 'test@test.com', 'x', 0, 1, '2024-01-01', '2024-01-01')",
        )).await.expect("insert test user");
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) VALUES(1, 1, 'testrepo', 0, 'main', 0, 0, '2024-01-01', '2024-01-01')",
        )).await.expect("insert test repo 1");
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO repositories(id, owner_id, name, is_private, default_branch, stars_count, forks_count, created_at, updated_at) VALUES(2, 1, 'testrepo2', 0, 'main', 0, 0, '2024-01-01', '2024-01-01')",
        )).await.expect("insert test repo 2");
        db
    }

    async fn create_test_label(
        db: &DatabaseConnection,
        repo_id: i64,
        name: &str,
        color: &str,
    ) -> Label {
        let model = ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            name: Set(name.to_string()),
            color: Set(color.to_string()),
            description: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
        };
        create(db, model).await.expect("create test label")
    }

    #[tokio::test]
    async fn test_find_by_ids_empty() {
        let db = setup_test_db().await;
        let result = find_by_ids(&db, &[]).await.unwrap();
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_find_by_ids_single() {
        let db = setup_test_db().await;
        let label = create_test_label(&db, 1, "bug", "#ff0000").await;

        let result = find_by_ids(&db, &[label.id]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].name, "bug");
        assert_eq!(result[0].color, "#ff0000");
    }

    #[tokio::test]
    async fn test_find_by_ids_multiple() {
        let db = setup_test_db().await;
        let l1 = create_test_label(&db, 1, "bug", "#ff0000").await;
        let l2 = create_test_label(&db, 1, "feature", "#00ff00").await;
        let _l3 = create_test_label(&db, 2, "other", "#0000ff").await;

        let result = find_by_ids(&db, &[l1.id, l2.id]).await.unwrap();
        assert_eq!(result.len(), 2);
    }

    #[tokio::test]
    async fn test_find_by_ids_nonexistent() {
        let db = setup_test_db().await;
        let label = create_test_label(&db, 1, "exists", "#000000").await;

        let result = find_by_ids(&db, &[label.id, 99999]).await.unwrap();
        assert_eq!(result.len(), 1);
    }

    #[tokio::test]
    async fn test_list_by_repo() {
        let db = setup_test_db().await;
        create_test_label(&db, 1, "bug", "#ff0000").await;
        create_test_label(&db, 1, "feature", "#00ff00").await;
        create_test_label(&db, 2, "other", "#0000ff").await;

        let labels = list_by_repo(&db, 1).await.unwrap();
        assert_eq!(labels.len(), 2);
    }
}
