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

/// Find issue IDs that have ALL of the specified labels, within one repo and
/// optionally one state. Returns (page of issue ids, total_count).
///
/// `total` and the page are two answers to the same question, so they must be
/// computed from the same predicate. The repo and the state used to be applied
/// by the caller *after* `LIMIT/OFFSET` had already cut the page: `total`
/// counted every issue carrying the labels while the page had been thinned by
/// filters the count never saw, so `?labels=bug&state=closed` could answer with
/// three rows and `total: 50`. Both filters now live inside the SQL, next to
/// the `COUNT`, and the page comes back already narrowed.
///
/// `state` is caller-supplied text and is bound, never interpolated; the label
/// ids are `i64` resolved from the database and are safe to inline (the `IN`
/// list is variadic, which bind markers here would make unreadable).
pub async fn find_issues_with_all_labels(
    db: &DatabaseConnection,
    repo_id: i64,
    label_ids: &[i64],
    state: Option<&str>,
    offset: u64,
    limit: u64,
) -> Result<(Vec<i64>, i64)> {
    if label_ids.is_empty() {
        return Ok((Vec::new(), 0));
    }

    // Issue ids carrying ALL of the labels, in this repo, in this state.
    // GROUP BY + HAVING COUNT(DISTINCT ...) is the AND over labels; the join
    // to `issues` is what lets the repo and state predicates apply before
    // pagination instead of after it.
    let label_list = label_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    let state_clause = if state.is_some() {
        " AND i.state = ?"
    } else {
        ""
    };
    let mut values: Vec<Value> = vec![repo_id.into()];
    if let Some(s) = state {
        values.push(s.into());
    }
    let matching_sql = format!(
        "SELECT il.issue_id FROM issue_labels il \
         JOIN issues i ON i.id = il.issue_id \
         WHERE il.label_id IN ({label_list}) AND i.repo_id = ?{state_clause} \
         GROUP BY il.issue_id HAVING COUNT(DISTINCT il.label_id) = {}",
        label_ids.len()
    );

    // The subquery alias is not decoration: PostgreSQL rejects a derived table
    // in `FROM` without one, so the count query used to be SQLite-only.
    let count_sql = format!("SELECT COUNT(*) FROM ({matching_sql}) AS matched");
    let backend = db.get_database_backend();
    let total = db
        .query_one(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(backend, &count_sql),
            values.clone(),
        ))
        .await
        .context("db: count issues with all labels")?;
    let total = decode_total(total)?;

    // Get paginated issue IDs
    let sql = format!("{matching_sql} ORDER BY il.issue_id DESC LIMIT {limit} OFFSET {offset}");
    let rows = db
        .query_all(Statement::from_sql_and_values(
            backend,
            crate::prepare_sql(backend, &sql),
            values,
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
            find_issues_with_all_labels(&db, 1, &[], None, 0, 20)
                .await
                .expect("an empty label filter does not query the database"),
            (Vec::new(), 0)
        );
    }

    /// The junction table and the issues it joins to, as the production query
    /// sees them. SQLite's column affinity is a preference, not a constraint,
    /// which is what lets the tests below store an `issue_id` the typed read
    /// cannot decode.
    ///
    /// `issues` carries `(id, repo_id, state)`; junction rows are `(issue_id,
    /// label_id)` and are seeded verbatim so a non-numeric id can be planted.
    async fn labelled_issues(
        issues: &[(&str, i64, &str)],
        labels: &[(&str, i64)],
    ) -> DatabaseConnection {
        let db = memory_db().await;
        db.execute_unprepared(
            "CREATE TABLE issue_labels (issue_id INTEGER NOT NULL, label_id INTEGER NOT NULL);\
             CREATE TABLE issues (id INTEGER NOT NULL, repo_id INTEGER NOT NULL, state TEXT NOT NULL);",
        )
        .await
        .expect("create the junction table and its issues");
        for (id, repo_id, state) in issues {
            db.execute_unprepared(&format!(
                "INSERT INTO issues (id, repo_id, state) VALUES ('{id}', {repo_id}, '{state}');"
            ))
            .await
            .expect("seed an issue");
        }
        for (issue_id, label_id) in labels {
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
        let db =
            labelled_issues(&[("7", 1, "open"), ("9", 1, "open")], &[("7", 1), ("9", 1)]).await;
        assert_eq!(
            find_issues_with_all_labels(&db, 1, &[1], None, 0, 20)
                .await
                .expect("decodable rows are returned"),
            (vec![9, 7], 2)
        );
    }

    #[tokio::test]
    async fn an_undecodable_issue_id_is_an_error_not_a_shorter_page() {
        // `total` counts this row fine, so before the fix the caller received a
        // successful `(vec![7], 2)` — a page one issue short of its own total.
        let db = labelled_issues(
            &[("7", 1, "open"), ("not a number", 1, "open")],
            &[("7", 1), ("not a number", 1)],
        )
        .await;

        let error = find_issues_with_all_labels(&db, 1, &[1], None, 0, 20)
            .await
            .expect_err("an undecodable issue_id is a failed query, not a dropped row");
        assert!(
            format!("{error:#}").contains("decode issue_id"),
            "the failure must identify the failed decode, got: {error:#}"
        );
    }

    /// The defect the state predicate was moved into SQL for: `total` used to
    /// count every labelled issue while the page was thinned afterwards, so a
    /// one-row page came back announcing a total of three.
    #[tokio::test]
    async fn the_state_filter_narrows_the_total_and_the_page_together() {
        let db = labelled_issues(
            &[("7", 1, "open"), ("8", 1, "closed"), ("9", 1, "open")],
            &[("7", 1), ("8", 1), ("9", 1)],
        )
        .await;

        assert_eq!(
            find_issues_with_all_labels(&db, 1, &[1], Some("closed"), 0, 20)
                .await
                .expect("the state filter is a database predicate"),
            (vec![8], 1),
            "total must count what the page contains, not what the labels alone match"
        );
    }

    /// Same for the repo: it used to be applied in memory after `LIMIT`, so a
    /// page could be emptied by a filter its own total had never seen.
    #[tokio::test]
    async fn another_repos_issue_is_counted_by_neither_the_total_nor_the_page() {
        let db =
            labelled_issues(&[("7", 1, "open"), ("8", 2, "open")], &[("7", 1), ("8", 1)]).await;

        assert_eq!(
            find_issues_with_all_labels(&db, 1, &[1], None, 0, 20)
                .await
                .expect("the repo filter is a database predicate"),
            (vec![7], 1)
        );
    }

    /// A page cut by `LIMIT` still reports the full total — the pair has to be
    /// consistent, not equal.
    #[tokio::test]
    async fn a_limited_page_still_reports_the_whole_matching_total() {
        let db = labelled_issues(
            &[("7", 1, "open"), ("8", 1, "open"), ("9", 1, "open")],
            &[("7", 1), ("8", 1), ("9", 1)],
        )
        .await;

        assert_eq!(
            find_issues_with_all_labels(&db, 1, &[1], Some("open"), 0, 2)
                .await
                .expect("a page and its total"),
            (vec![9, 8], 3)
        );
    }

    /// `state` is caller-supplied text: it must arrive as a bound value, so a
    /// quote in it can only ever fail to match.
    #[tokio::test]
    async fn a_quoted_state_is_bound_not_interpolated() {
        let db = labelled_issues(&[("7", 1, "open")], &[("7", 1)]).await;

        assert_eq!(
            find_issues_with_all_labels(&db, 1, &[1], Some("open' OR '1'='1"), 0, 20)
                .await
                .expect("a quote in the state is data, not syntax"),
            (Vec::new(), 0)
        );
    }
}
