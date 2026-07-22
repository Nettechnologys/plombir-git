//! Database operations for issue comments.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::issue_comment::{
    self, ActiveModel, Entity as CommentEntity, Model as Comment,
};

/// Find a comment by id.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Comment>> {
    CommentEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find issue comment by id")
}

/// List comments for an issue, ordered by creation time.
pub async fn list_by_issue(db: &DatabaseConnection, issue_id: i64) -> Result<Vec<Comment>> {
    CommentEntity::find()
        .filter(issue_comment::Column::IssueId.eq(issue_id))
        .order_by_asc(issue_comment::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list issue comments")
}

/// Create a new comment.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Comment> {
    model.insert(db).await.context("db: create issue comment")
}

/// Update a comment.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Comment> {
    model.update(db).await.context("db: update issue comment")
}

/// Delete a comment by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    CommentEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete issue comment")?;
    Ok(())
}
