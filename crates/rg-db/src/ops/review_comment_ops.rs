//! Database operations for review comments.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::review_comment::{
    self, ActiveModel, Entity as CommentEntity, Model as ReviewComment,
};

/// Find a comment by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<ReviewComment>> {
    CommentEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find review comment by id")
}

/// List all comments for a review, ordered by creation time.
pub async fn list_by_review(db: &DatabaseConnection, review_id: i64) -> Result<Vec<ReviewComment>> {
    CommentEntity::find()
        .filter(review_comment::Column::ReviewId.eq(review_id))
        .order_by_asc(review_comment::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list review comments by review")
}

/// List all comments for a PR (across all reviews).
pub async fn list_by_pr(db: &DatabaseConnection, pr_id: i64) -> Result<Vec<ReviewComment>> {
    CommentEntity::find()
        .filter(review_comment::Column::PrId.eq(pr_id))
        .order_by_asc(review_comment::Column::CreatedAt)
        .all(db)
        .await
        .context("db: list review comments by PR")
}

/// Create a new review comment.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<ReviewComment> {
    model.insert(db).await.context("db: create review comment")
}

/// Update a review comment.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<ReviewComment> {
    model.update(db).await.context("db: update review comment")
}

/// Delete a review comment by ID.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    CommentEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete review comment")?;
    Ok(())
}
