//! Database operations for milestones.

use anyhow::{Context, Result};
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

/// Update a milestone.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Milestone> {
    model.update(db).await.context("db: update milestone")
}

/// Delete a milestone by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    MilestoneEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete milestone")?;
    Ok(())
}

/// Count open (non-closed) issues in a milestone.
pub async fn count_open_by_milestone(db: &DatabaseConnection, milestone_id: i64) -> Result<i64> {
    use crate::entities::issue;
    let count = issue::Entity::find()
        .filter(issue::Column::MilestoneId.eq(milestone_id))
        .filter(issue::Column::State.ne("closed"))
        .count(db)
        .await
        .context("db: count open issues by milestone")?;
    Ok(count as i64)
}
