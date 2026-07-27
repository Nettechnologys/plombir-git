//! Database operations for import tasks.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::import_task::{self, ActiveModel, Entity as ImportTaskEntity, Model};

/// Statuses in which an import task is still considered "in progress".
/// A task in one of these states is expected to keep advancing; if it stops
/// updating it has almost certainly been orphaned (e.g. its background
/// `tokio::spawn` died with a server restart / crash).
pub const RUNNING_STATUSES: [&str; 3] = ["pending", "cloning", "importing"];

/// Create a new import task.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.insert(db).await.context("db: create import task")
}

/// Find an import task by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>> {
    ImportTaskEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find import task by id")
}

/// Find the latest import task for a user.
pub async fn find_by_user(db: &DatabaseConnection, user_id: i64, limit: u64) -> Result<Vec<Model>> {
    ImportTaskEntity::find()
        .filter(import_task::Column::UserId.eq(user_id))
        .order_by_desc(import_task::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .context("db: find import tasks by user")
}

/// Find an import task by target owner/name (latest first).
pub async fn find_by_target(
    db: &DatabaseConnection,
    owner: &str,
    name: &str,
) -> Result<Option<Model>> {
    ImportTaskEntity::find()
        .filter(import_task::Column::TargetOwner.eq(owner))
        .filter(import_task::Column::TargetName.eq(name))
        .order_by_desc(import_task::Column::CreatedAt)
        .one(db)
        .await
        .context("db: find import task by target")
}

/// Find import tasks by repo_id.
pub async fn find_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Option<Model>> {
    ImportTaskEntity::find()
        .filter(import_task::Column::RepoId.eq(repo_id))
        .order_by_desc(import_task::Column::CreatedAt)
        .one(db)
        .await
        .context("db: find import task by repo")
}

/// Update an import task.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.update(db).await.context("db: update import task")
}

/// Update status and progress atomically.
pub async fn update_progress(
    db: &DatabaseConnection,
    id: i64,
    status: &str,
    progress: i32,
    stage: Option<&str>,
) -> Result<Model> {
    let task = find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found: {id}"))?;

    let mut model: ActiveModel = task.into();
    model.status = Set(status.to_string());
    model.progress = Set(progress);
    model.stage = Set(stage.map(|s| s.to_string()));
    model.updated_at = Set(Utc::now());

    update(db, model).await
}

/// Mark import as failed with error message.
pub async fn mark_failed(db: &DatabaseConnection, id: i64, error: &str) -> Result<Model> {
    let task = find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found: {id}"))?;

    let mut model: ActiveModel = task.into();
    model.status = Set("failed".to_string());
    model.error = Set(Some(error.to_string()));
    model.updated_at = Set(Utc::now());

    update(db, model).await
}

/// Mark import as completed with final stats.
pub async fn mark_completed(db: &DatabaseConnection, id: i64, stats_json: &str) -> Result<Model> {
    let task = find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found: {id}"))?;

    let mut model: ActiveModel = task.into();
    model.status = Set("completed".to_string());
    model.progress = Set(100);
    model.stage = Set(Some("Import completed".to_string()));
    model.stats = Set(Some(stats_json.to_string()));
    model.updated_at = Set(Utc::now());

    update(db, model).await
}

/// Set repo_id after repository is created.
pub async fn set_repo_id(db: &DatabaseConnection, id: i64, repo_id: i64) -> Result<Model> {
    let task = find_by_id(db, id)
        .await?
        .ok_or_else(|| anyhow::anyhow!("import task not found: {id}"))?;

    let mut model: ActiveModel = task.into();
    model.repo_id = Set(Some(repo_id));
    model.updated_at = Set(Utc::now());

    update(db, model).await
}

/// Delete an import task by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    ImportTaskEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete import task")?;
    Ok(())
}

/// List all active (non-completed, non-failed) import tasks.
pub async fn list_active(db: &DatabaseConnection, limit: u64) -> Result<Vec<Model>> {
    ImportTaskEntity::find()
        .filter(import_task::Column::Status.is_in(RUNNING_STATUSES))
        .order_by_asc(import_task::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .context("db: list active import tasks")
}

/// Find import tasks that appear stuck: still in a running status but not
/// updated within `older_than_secs`. The background import runs as a detached
/// `tokio::spawn`, so a server restart/crash orphans any in-flight import —
/// `mark_completed`/`mark_failed` never fire and the task hangs in
/// `pending`/`cloning`/`importing` forever, making status polling spin
/// endlessly. This is the import-side twin of `pipeline_ops::find_stuck_jobs`
/// for CI jobs; recovery is handled by the runner watchdog.
pub async fn find_stuck(db: &DatabaseConnection, older_than_secs: i64) -> Result<Vec<Model>> {
    let cutoff = Utc::now() - chrono::Duration::seconds(older_than_secs);
    ImportTaskEntity::find()
        .filter(import_task::Column::Status.is_in(RUNNING_STATUSES))
        .filter(import_task::Column::UpdatedAt.lte(cutoff))
        .order_by_asc(import_task::Column::CreatedAt)
        .all(db)
        .await
        .context("db: find stuck import tasks")
}

/// Atomically fail a stuck import task, but only if it is *still* in a running
/// status and *still* older than the cutoff. The guarded `UPDATE ... WHERE`
/// avoids a race with the owning task legitimately finishing (or a concurrent
/// `update_progress`) between `find_stuck` and this call — a task that just
/// completed is left untouched. Returns `true` when a row was actually failed.
pub async fn fail_stuck(
    db: &DatabaseConnection,
    id: i64,
    older_than_secs: i64,
    error: &str,
) -> Result<bool> {
    let cutoff = Utc::now() - chrono::Duration::seconds(older_than_secs);
    let res = ImportTaskEntity::update_many()
        .col_expr(import_task::Column::Status, Expr::value("failed"))
        .col_expr(import_task::Column::Error, Expr::value(error))
        .col_expr(import_task::Column::UpdatedAt, Expr::value(Utc::now()))
        .filter(import_task::Column::Id.eq(id))
        .filter(import_task::Column::Status.is_in(RUNNING_STATUSES))
        .filter(import_task::Column::UpdatedAt.lte(cutoff))
        .exec(db)
        .await
        .context("db: fail stuck import task")?;
    Ok(res.rows_affected > 0)
}

/// List all import tasks (admin use).
pub async fn list_all(db: &DatabaseConnection, limit: u64) -> Result<Vec<Model>> {
    ImportTaskEntity::find()
        .order_by_desc(import_task::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .context("db: list all import tasks")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use sea_orm::{ConnectOptions, Database, Set, Statement};

    async fn setup_test_db() -> DatabaseConnection {
        let mut opt = ConnectOptions::new("sqlite::memory:");
        opt.max_connections(1);
        let db = Database::connect(opt).await.expect("connect in-memory db");
        crate::run_migrations(&db).await.expect("run migrations");
        db.execute(Statement::from_string(
            sea_orm::DatabaseBackend::Sqlite,
            "INSERT INTO users(id, username, email, password_hash, is_admin, is_active, created_at, updated_at) VALUES(1, 'test', 'test@test.com', 'x', 0, 1, '2024-01-01', '2024-01-01')",
        )).await.expect("insert test user");
        db
    }

    /// Insert an import task with an explicit status and `updated_at`.
    async fn insert_task(db: &DatabaseConnection, status: &str, updated_at: chrono::DateTime<Utc>) -> Model {
        let model = ActiveModel {
            user_id: Set(1),
            repo_id: Set(None),
            platform: Set("github".to_string()),
            source_url: Set("https://example.com/x/y".to_string()),
            target_owner: Set("owner".to_string()),
            target_name: Set("name".to_string()),
            status: Set(status.to_string()),
            progress: Set(0),
            stage: Set(None),
            error: Set(None),
            user_mapping: Set(None),
            import_repo: Set(true),
            import_issues: Set(false),
            import_pull_requests: Set(false),
            import_wiki: Set(false),
            import_releases: Set(false),
            import_labels: Set(false),
            import_milestones: Set(false),
            stats: Set(None),
            created_at: Set(updated_at),
            updated_at: Set(updated_at),
            ..Default::default()
        };
        create(db, model).await.expect("insert task")
    }

    #[tokio::test]
    async fn find_stuck_matches_only_old_running_tasks() {
        let db = setup_test_db().await;
        let old = Utc::now() - Duration::seconds(3600);
        let fresh = Utc::now();

        let stuck_cloning = insert_task(&db, "cloning", old).await;
        let stuck_importing = insert_task(&db, "importing", old).await;
        let _fresh_running = insert_task(&db, "importing", fresh).await; // too recent
        let _old_completed = insert_task(&db, "completed", old).await; // terminal
        let _old_failed = insert_task(&db, "failed", old).await; // terminal

        let stuck = find_stuck(&db, 600).await.unwrap();
        let ids: Vec<i64> = stuck.iter().map(|t| t.id).collect();
        assert_eq!(stuck.len(), 2, "only old running tasks are stuck: {ids:?}");
        assert!(ids.contains(&stuck_cloning.id));
        assert!(ids.contains(&stuck_importing.id));
    }

    #[tokio::test]
    async fn fail_stuck_is_guarded_and_idempotent() {
        let db = setup_test_db().await;
        let old = Utc::now() - Duration::seconds(3600);
        let task = insert_task(&db, "cloning", old).await;

        // First call fails it and reports the write.
        assert!(fail_stuck(&db, task.id, 600, "interrupted").await.unwrap());
        let after = find_by_id(&db, task.id).await.unwrap().unwrap();
        assert_eq!(after.status, "failed");
        assert_eq!(after.error.as_deref(), Some("interrupted"));

        // Second call is a no-op: the task is no longer in a running status.
        assert!(!fail_stuck(&db, task.id, 600, "again").await.unwrap());
        let after2 = find_by_id(&db, task.id).await.unwrap().unwrap();
        assert_eq!(after2.error.as_deref(), Some("interrupted"), "not clobbered");
    }

    #[tokio::test]
    async fn fail_stuck_skips_freshly_updated_task() {
        // Guards the race where a task legitimately advanced between find_stuck
        // and fail_stuck: a running task updated just now must not be failed.
        let db = setup_test_db().await;
        let task = insert_task(&db, "importing", Utc::now()).await;
        assert!(!fail_stuck(&db, task.id, 600, "interrupted").await.unwrap());
        let after = find_by_id(&db, task.id).await.unwrap().unwrap();
        assert_eq!(after.status, "importing");
    }
}
