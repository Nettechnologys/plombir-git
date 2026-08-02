//! Database operations for issue labels junction table.

use anyhow::{Context, Result};
use sea_orm::{ConnectionTrait, *};

use crate::entities::issue_label::{
    self, ActiveModel, Entity as IssueLabelEntity, Model as IssueLabel,
};

/// Set labels for an issue (replace all existing labels).
pub async fn set_labels(db: &DatabaseConnection, issue_id: i64, label_ids: Vec<i64>) -> Result<()> {
    let txn = db.begin().await.context("db: begin transaction")?;

    // CRITICAL: SeaORM batch delete (pitfall #3)
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
///
/// Deliberately `Result<()>` and not `Result<bool>`: this is a bulk cascade
/// helper run before the label row itself goes away, and a label carried by no
/// issue legitimately matches zero rows. Only the single-row deletes whose
/// `rows_affected` decides an HTTP status need to report it.
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
        where_clause,
        label_ids.len()
    );
    let backend = db.get_database_backend();
    let total = db
        .query_one(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(backend, &count_sql),
            [],
        ))
        .await
        .context("db: count issues with all labels")?;
    let total = decode_total(total)?;

    // Get paginated issue IDs
    let sql = format!(
        "SELECT issue_id FROM issue_labels WHERE {} GROUP BY issue_id HAVING COUNT(DISTINCT label_id) = {} ORDER BY issue_id DESC LIMIT {} OFFSET {}",
        where_clause,
        label_ids.len(),
        limit,
        offset
    );
    let rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(backend, &sql),
            [],
        ))
        .await
        .context("db: find issues with all labels")?;

    // Every row the query returned is a match, so a row that will not decode is
    // a failed query — not one issue fewer. Dropping it here was especially
    // convincing because `total` above had already been counted successfully:
    // the caller got a page shorter than the total it was handed, which reads
    // as "the rest is on the next page" rather than as an error.
    let issue_ids = rows
        .iter()
        .map(|row| row.try_get_by_index(0))
        .collect::<Result<Vec<i64>, DbErr>>()
        .context("db: find issues with all labels: decode issue_id")?;

    Ok((issue_ids, total))
}

/// Decode the single row a `COUNT(*)` aggregate is obliged to return.
///
/// An absent row or an undecodable value means the count query did not answer,
/// not that no issues matched. Zero is valid only after it decodes successfully.
fn decode_total(row: Option<QueryResult>) -> Result<i64> {
    let row = row.context("db: count issues with all labels: aggregate returned no row")?;
    row.try_get_by_index(0)
        .context("db: count issues with all labels: decode total")
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn memory_db() -> DatabaseConnection {
        Database::connect("sqlite::memory:")
            .await
            .expect("open an in-memory database")
    }

    async fn one_row(db: &DatabaseConnection, sql: &str) -> Option<QueryResult> {
        db.query_one(Statement::from_string(
            db.get_database_backend(),
            sql.to_string(),
        ))
        .await
        .expect("run query")
    }

    #[tokio::test]
    async fn a_decoded_zero_total_is_still_a_total() {
        let db = memory_db().await;
        assert_eq!(
            decode_total(one_row(&db, "SELECT 0").await).expect("zero decodes"),
            0,
            "an honest total of zero must stay a total"
        );
    }

    #[tokio::test]
    async fn an_undecodable_total_is_an_error_not_zero() {
        let db = memory_db().await;
        let sql = "SELECT 'not a number'";

        let error = decode_total(one_row(&db, sql).await)
            .expect_err("an undecodable COUNT row is a failed query, not zero");
        assert!(
            format!("{error:#}").contains("decode total"),
            "the failure must identify the failed decode, got: {error:#}"
        );
    }

    #[tokio::test]
    async fn an_empty_label_filter_stays_an_empty_result() {
        let db = memory_db().await;
        assert_eq!(
            find_issues_with_all_labels(&db, &[], 0, 20)
                .await
                .expect("an empty label filter does not query the database"),
            (Vec::new(), 0)
        );
    }

    /// The junction table as the production query sees it. SQLite's column
    /// affinity is a preference, not a constraint, which is what lets the test
    /// below store an `issue_id` the typed read cannot decode.
    async fn labelled_issues(rows: &[(&str, i64)]) -> DatabaseConnection {
        let db = memory_db().await;
        db.execute_unprepared(
            "CREATE TABLE issue_labels (issue_id INTEGER NOT NULL, label_id INTEGER NOT NULL);",
        )
        .await
        .expect("create the junction table");
        for (issue_id, label_id) in rows {
            db.execute_unprepared(&format!(
                "INSERT INTO issue_labels (issue_id, label_id) VALUES ('{issue_id}', {label_id});"
            ))
            .await
            .expect("seed a junction row");
        }
        db
    }

    #[tokio::test]
    async fn decodable_issue_ids_are_returned_with_their_total() {
        let db = labelled_issues(&[("7", 1), ("9", 1)]).await;
        assert_eq!(
            find_issues_with_all_labels(&db, &[1], 0, 20)
                .await
                .expect("decodable rows are returned"),
            (vec![9, 7], 2)
        );
    }

    #[tokio::test]
    async fn an_undecodable_issue_id_is_an_error_not_a_shorter_page() {
        // `total` counts this row fine, so before the fix the caller received a
        // successful `(vec![7], 2)` — a page one issue short of its own total.
        let db = labelled_issues(&[("7", 1), ("not a number", 1)]).await;

        let error = find_issues_with_all_labels(&db, &[1], 0, 20)
            .await
            .expect_err("an undecodable issue_id is a failed query, not a dropped row");
        assert!(
            format!("{error:#}").contains("decode issue_id"),
            "the failure must identify the failed decode, got: {error:#}"
        );
    }
}
