//! Database operations for issue labels junction table.

use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, *};

use crate::entities::issue_label::{
    self, ActiveModel, Entity as IssueLabelEntity, Model as IssueLabel,
};

/// Set labels for an issue (replace all existing labels).
pub async fn set_labels(db: &DatabaseConnection, issue_id: i64, label_ids: Vec<i64>) -> Result<()> {
    let txn = db.begin().await.context("db: begin transaction")?;

    // CRITICAL: SeaORM batch delete (踩坑经验 #3)
    //
    // To delete multiple rows, MUST use:
    //   Entity::delete_many().filter(...).exec(db)
    //
    // WRONG patterns:
    //   Entity::delete_by_id(id)        — only works for single PK delete
    //   Entity::update_many()            — for UPDATE, not DELETE
    //   .delete() without .filter()      — compile error or deletes nothing
    //
    // Correct batch delete pattern (used here):
    //   IssueLabelEntity::delete_many()
    //       .filter(issue_label::Column::IssueId.eq(issue_id))
    //       .exec(&txn)
    //       .await?;
    IssueLabelEntity::delete_many()
        .filter(issue_label::Column::IssueId.eq(issue_id))
        .exec(&txn)
        .await
        .context("db: delete existing issue labels")?;

    // Insert new labels
    for label_id in label_ids {
        let model = ActiveModel {
            issue_id: Set(issue_id),
            label_id: Set(label_id),
            created_at: Set(chrono::Utc::now()),
            ..Default::default()
        };
        model.insert(&txn).await.context("db: insert issue label")?;
    }

    txn.commit().await.context("db: commit transaction")?;
    Ok(())
}

/// Get all label IDs for an issue.
pub async fn get_label_ids(db: &DatabaseConnection, issue_id: i64) -> Result<Vec<i64>> {
    let labels = IssueLabelEntity::find()
        .filter(issue_label::Column::IssueId.eq(issue_id))
        .all(db)
        .await
        .context("db: get issue label ids")?;
    Ok(labels.into_iter().map(|l| l.label_id).collect())
}

/// Get all issue labels for an issue.
pub async fn get_labels(db: &DatabaseConnection, issue_id: i64) -> Result<Vec<IssueLabel>> {
    IssueLabelEntity::find()
        .filter(issue_label::Column::IssueId.eq(issue_id))
        .all(db)
        .await
        .context("db: get issue labels")
}

/// Delete all issue labels for a label ID (used when deleting a label).
pub async fn delete_by_label_id(db: &DatabaseConnection, label_id: i64) -> Result<()> {
    IssueLabelEntity::delete_many()
        .filter(issue_label::Column::LabelId.eq(label_id))
        .exec(db)
        .await
        .context("db: delete issue labels by label id")?;
    Ok(())
}

/// Find issue IDs that have ALL of the specified labels.
/// Returns (matching_issue_ids, total_count).
pub async fn find_issues_with_all_labels(
    db: &DatabaseConnection,
    label_ids: &[i64],
    offset: u64,
    limit: u64,
) -> Result<(Vec<i64>, i64)> {
    if label_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }

    // Find issue IDs that have all specified labels using GROUP BY + HAVING COUNT
    let mut conditions = Vec::new();
    for label_id in label_ids {
        conditions.push(format!("label_id = {}", label_id));
    }
    let where_clause = conditions.join(" OR ");

    // Get total count of distinct issue_ids matching all labels
    let count_sql = format!(
        "SELECT COUNT(*) FROM (SELECT issue_id FROM issue_labels WHERE {} GROUP BY issue_id HAVING COUNT(DISTINCT label_id) = {})",
        where_clause, label_ids.len()
    );
    let backend = db.get_database_backend();
    let total: i64 = db
        .query_one(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(backend, &count_sql),
            [],
        ))
        .await
        .context("db: count issues with all labels")?
        .and_then(|row| row.try_get_by_index(0).ok())
        .unwrap_or(0);

    // Get paginated issue IDs
    let sql = format!(
        "SELECT issue_id FROM issue_labels WHERE {} GROUP BY issue_id HAVING COUNT(DISTINCT label_id) = {} ORDER BY issue_id DESC LIMIT {} OFFSET {}",
        where_clause, label_ids.len(), limit, offset
    );
    let rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(backend, &sql),
            [],
        ))
        .await
        .context("db: find issues with all labels")?;

    let issue_ids: Vec<i64> = rows
        .iter()
        .filter_map(|row| row.try_get_by_index(0).ok())
        .collect();

    Ok((issue_ids, total))
}
