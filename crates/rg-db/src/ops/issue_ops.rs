//! Database operations for issues.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::issue::{self, ActiveModel, Entity as IssueEntity, Model as Issue};

/// Find an issue by (repo_id, number).
pub async fn find_by_repo_and_number(
    db: &DatabaseConnection,
    repo_id: i64,
    number: i64,
) -> Result<Option<Issue>> {
    IssueEntity::find()
        .filter(issue::Column::RepoId.eq(repo_id))
        .filter(issue::Column::Number.eq(number))
        .one(db)
        .await
        .context("db: find issue by repo and number")
}

/// Find an issue by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Issue>> {
    IssueEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find issue by id")
}

/// Find issues by IDs (batch query using IN clause).
/// Returns only existing issues — missing IDs are silently skipped.
pub async fn find_by_ids(db: &DatabaseConnection, ids: &[i64]) -> Result<Vec<Issue>> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    IssueEntity::find()
        .filter(issue::Column::Id.is_in(ids.iter().copied()))
        .all(db)
        .await
        .context("db: find issues by ids")
}

/// List issues for a repo, optionally filtered by state.
pub async fn list_by_repo(
    db: &DatabaseConnection,
    repo_id: i64,
    state: Option<&str>,
) -> Result<Vec<Issue>> {
    let mut query = IssueEntity::find().filter(issue::Column::RepoId.eq(repo_id));
    if let Some(s) = state {
        query = query.filter(issue::Column::State.eq(s));
    }
    query
        .order_by_desc(issue::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list issues by repo")
}

/// Paginated list of issues for a repo.
/// Returns (data, total) — SQL LIMIT/OFFSET pushed to the database.
pub async fn list_by_repo_paginated(
    db: &DatabaseConnection,
    repo_id: i64,
    state: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<Issue>, i64)> {
    let mut base = IssueEntity::find().filter(issue::Column::RepoId.eq(repo_id));
    if let Some(s) = state {
        base = base.filter(issue::Column::State.eq(s));
    }
    let query = base.order_by_desc(issue::Column::CreatedAt);

    let total = query
        .clone()
        .count(db)
        .await
        .context("db: count issues by repo")? as i64;
    let issues = query
        .offset(offset)
        .limit(limit)
        .all(db)
        .await
        .context("db: list issues by repo (paginated)")?;

    Ok((issues, total))
}

/// Get the next issue number for a repo (max + 1, or 1 if no issues).
pub async fn next_number(db: &DatabaseConnection, repo_id: i64) -> Result<i64> {
    let max = IssueEntity::find()
        .filter(issue::Column::RepoId.eq(repo_id))
        .order_by_desc(issue::Column::Number)
        .one(db)
        .await
        .context("db: get max issue number")?;
    Ok(max.map(|m| m.number + 1).unwrap_or(1))
}

/// Create a new issue.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Issue> {
    model.insert(db).await.context("db: create issue")
}

/// Update an issue.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Issue> {
    model.update(db).await.context("db: update issue")
}

/// Delete an issue by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    IssueEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete issue")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::entities::issue::ActiveModel;
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
        // Insert minimal user + repos to satisfy FK constraints
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

    async fn create_test_issue(
        db: &DatabaseConnection,
        repo_id: i64,
        number: i64,
        title: &str,
    ) -> Issue {
        let model = ActiveModel {
            id: sea_orm::NotSet,
            repo_id: Set(repo_id),
            number: Set(number),
            title: Set(title.to_string()),
            body: Set(None),
            state: Set("open".to_string()),
            author_id: Set(1),
            assignee_id: Set(None),
            milestone_id: Set(None),
            labels: Set(None),
            created_at: Set(Utc::now()),
            updated_at: Set(Utc::now()),
            closed_at: Set(None),
            deleted_at: Set(None),
        };
        create(db, model).await.expect("create test issue")
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
        let issue = create_test_issue(&db, 1, 1, "test issue").await;

        let result = find_by_ids(&db, &[issue.id]).await.unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, issue.id);
        assert_eq!(result[0].title, "test issue");
    }

    #[tokio::test]
    async fn test_find_by_ids_multiple() {
        let db = setup_test_db().await;
        let i1 = create_test_issue(&db, 1, 1, "issue 1").await;
        let _i2 = create_test_issue(&db, 1, 2, "issue 2").await;
        let i3 = create_test_issue(&db, 2, 1, "other repo").await;

        // Query i1 and i3, skip i2
        let result = find_by_ids(&db, &[i1.id, i3.id]).await.unwrap();
        assert_eq!(result.len(), 2);
        let titles: Vec<&str> = result.iter().map(|i| i.title.as_str()).collect();
        assert!(titles.contains(&"issue 1"));
        assert!(titles.contains(&"other repo"));
    }

    #[tokio::test]
    async fn test_find_by_ids_nonexistent() {
        let db = setup_test_db().await;
        let issue = create_test_issue(&db, 1, 1, "exists").await;

        // Query existing + non-existing IDs
        let result = find_by_ids(&db, &[issue.id, 99999]).await.unwrap();
        assert_eq!(result.len(), 1); // only existing returned
        assert_eq!(result[0].id, issue.id);
    }

    #[tokio::test]
    async fn test_find_by_repo_and_number() {
        let db = setup_test_db().await;
        create_test_issue(&db, 1, 5, "repo 1 #5").await;
        create_test_issue(&db, 2, 5, "repo 2 #5").await;

        let r1 = find_by_repo_and_number(&db, 1, 5).await.unwrap();
        assert!(r1.is_some());
        assert_eq!(r1.unwrap().title, "repo 1 #5");

        let r2 = find_by_repo_and_number(&db, 2, 5).await.unwrap();
        assert!(r2.is_some());
        assert_eq!(r2.unwrap().title, "repo 2 #5");

        let none = find_by_repo_and_number(&db, 1, 99).await.unwrap();
        assert!(none.is_none());
    }
}
