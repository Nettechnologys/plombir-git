//! Database operations for milestones.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::milestone::{
    self, ActiveModel, Entity as MilestoneEntity, Model as Milestone,
};

/// Find a milestone by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Milestone>> {
    MilestoneEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find milestone by id")
}

/// List milestones for a repo.
pub async fn list_by_repo(
    db: &DatabaseConnection,
    repo_id: i64,
    state: Option<&str>,
) -> Result<Vec<Milestone>> {
    let mut query = MilestoneEntity::find().filter(milestone::Column::RepoId.eq(repo_id));
    if let Some(s) = state {
        query = query.filter(milestone::Column::State.eq(s));
    }
    query
        .order_by_asc(milestone::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list milestones by repo")
}

/// Create a new milestone.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Milestone> {
    model.insert(db).await.context("db: create milestone")
}

/// Update a milestone in one conditional statement.
///
/// The HTTP layer reads the row first to anchor its global id to a repository.
/// A delete between that read and this write is therefore a normal `None`, not
/// a backend-shaped `RecordNotUpdated` error.
pub async fn update(
    db: &DatabaseConnection,
    id: i64,
    title: Option<String>,
    description: Option<Option<String>>,
    state: Option<String>,
    due_date: Option<Option<chrono::DateTime<chrono::Utc>>>,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Milestone>> {
    let mut update = MilestoneEntity::update_many()
        .col_expr(milestone::Column::UpdatedAt, Expr::value(updated_at));
    if let Some(title) = title {
        update = update.col_expr(milestone::Column::Title, Expr::value(title));
    }
    if let Some(description) = description {
        update = update.col_expr(milestone::Column::Description, Expr::value(description));
    }
    if let Some(state) = state {
        update = update.col_expr(milestone::Column::State, Expr::value(state));
    }
    if let Some(due_date) = due_date {
        update = update.col_expr(milestone::Column::DueDate, Expr::value(due_date));
    }

    update
        .filter(milestone::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: update milestone")?;
    find_by_id(db, id).await
}

/// Delete a milestone by id. `Ok(false)` means no such row.
///
/// The caller's scope lookup and this `DELETE` are two statements: reporting
/// `rows_affected` is what stops a route from confirming a deletion that a
/// concurrent request had already performed.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = MilestoneEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete milestone")?;
    Ok(result.rows_affected > 0)
}

/// Count open (non-closed) issues filed under a milestone of `repo_id`.
///
/// A milestone belongs to exactly one repository, so the `repo_id` filter is
/// redundant on clean data — and that is the point. Filtering on the milestone
/// id alone let an issue in *another* repository keep this count above zero
/// forever, which is what kept the owning repository's milestone from ever
/// being reported closed. The handlers now refuse to create such a row; this
/// keeps any row written before they did from poisoning the count.
pub async fn count_open_by_milestone(
    db: &DatabaseConnection,
    repo_id: i64,
    milestone_id: i64,
) -> Result<i64> {
    use crate::entities::issue;
    let count = issue::Entity::find()
        .filter(issue::Column::RepoId.eq(repo_id))
        .filter(issue::Column::MilestoneId.eq(milestone_id))
        .filter(issue::Column::State.ne("closed"))
        .count(db)
        .await
        .context("db: count open issues by milestone")?;
    Ok(count as i64)
}
