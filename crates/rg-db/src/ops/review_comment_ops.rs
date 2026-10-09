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
pub async fn create<C: ConnectionTrait>(db: &C, model: ActiveModel) -> Result<ReviewComment> {
    model.insert(db).await.context("db: create review comment")
}

/// Update a review comment.
pub async fn update<C: ConnectionTrait>(db: &C, model: ActiveModel) -> Result<ReviewComment> {
    model.update(db).await.context("db: update review comment")
}

/// Replace a review comment's body, stamping `updated_at`. `None` when the
/// row is gone.
pub async fn update_body(
    db: &DatabaseConnection,
    id: i64,
    body: &str,
) -> Result<Option<ReviewComment>> {
    let updated = CommentEntity::update_many()
        .col_expr(review_comment::Column::Body, sea_query::Expr::value(body))
        .col_expr(
            review_comment::Column::UpdatedAt,
            sea_query::Expr::value(chrono::Utc::now()),
        )
        .filter(review_comment::Column::Id.eq(id))
        .exec(db)
        .await
        .context("db: edit review comment")?;
    if updated.rows_affected == 0 {
        return Ok(None);
    }
    find_by_id(db, id).await
}

/// Whether any comment replies to `id`.
pub async fn has_replies(db: &DatabaseConnection, id: i64) -> Result<bool> {
    Ok(CommentEntity::find()
        .filter(review_comment::Column::ReplyToId.eq(id))
        .count(db)
        .await
        .context("db: count review comment replies")?
        > 0)
}

/// Delete a review comment. `false` when it was already gone.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let deleted = CommentEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete review comment")?;
    Ok(deleted.rows_affected == 1)
}
