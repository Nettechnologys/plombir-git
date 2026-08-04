//! Wiki service — CRUD operations for repository wiki pages.
//!
//! Each repository can have an associated wiki. Wiki pages are stored in the
//! database for fast querying and optionally mirrored to a `.wiki.git` bare
//! repository on disk for version control.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection, TransactionTrait};

use rg_db::entities::wiki_page;
use rg_db::entities::wiki_revision;
use rg_db::ops::wiki_page_ops;
use rg_db::ops::wiki_revision_ops;

use crate::search::dialect::metadata_fts_upsert_sql;

/// Create a new wiki page.
pub async fn create_page(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
) -> Result<wiki_page::Model> {
    // The one answer both the pre-read and a losing insert give, so a caller
    // cannot tell which of the two noticed — and so no constraint text leaks.
    // `Conflict`, not `InvalidRequest`: an existing page refuses the title, and
    // the caller either picks another one or edits the page that holds it.
    let already_exists = || {
        crate::error::conflict(format!(
            "wiki page '{title}' already exists in this repository"
        ))
    };

    // Check for duplicate title. This read is the fast path only — the row can
    // still appear between here and the insert below, which is why the insert
    // classifies its own failure rather than trusting this answer.
    if wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("check existing wiki page")?
        .is_some()
    {
        return Err(already_exists());
    }

    let now = Utc::now();
    let model = wiki_page::ActiveModel {
        id: sea_orm::NotSet,
        repo_id: sea_orm::Set(repo_id),
        title: sea_orm::Set(title.to_string()),
        content: sea_orm::Set(content.to_string()),
        message: sea_orm::Set(message.map(|s| s.to_string())),
        author_id: sea_orm::Set(author_id),
        sha: sea_orm::Set(None),
        edit_version: sea_orm::Set(0),
        created_at: sea_orm::Set(now),
        updated_at: sea_orm::Set(now),
    };

    // Losing the `idx_wiki_repo_title` race is the same outcome the read above
    // reports, reached a moment later: someone else created the page first.
    // Only that one loss is folded — any other write failure stays an error.
    let page = match wiki_page_ops::create(db, model).await {
        Ok(page) => page,
        Err(error) if rg_db::is_unique_violation_anyhow(&error) => return Err(already_exists()),
        Err(error) => return Err(error),
    };

    // Keep the metadata FTS table in sync (non-fatal). Database triggers also
    // maintain it; the upsert makes the explicit write safe on every backend.
    let page_id = page.id;
    let page_title = page.title.clone();
    let page_content = page.content.clone();
    let backend = db.get_database_backend();
    let sql = metadata_fts_upsert_sql(backend, "wiki_pages_fts", "title, content");
    if let Err(e) = db
        .execute(sea_orm::Statement::from_sql_and_values(
            backend,
            rg_db::prepare_sql(backend, &sql),
            [page_id.into(), page_title.into(), page_content.into()],
        ))
        .await
    {
        tracing::warn!(error = %format!("{e:#}"), page_id = %page_id, "failed to update wiki_pages_fts index");
    }

    Ok(page)
}

/// Get a wiki page by repo and title.
pub async fn get_page(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
) -> Result<Option<wiki_page::Model>> {
    wiki_page_ops::find_by_repo_and_title(db, repo_id, title).await
}

/// List all wiki pages for a repo (title + updated_at only for index).
pub async fn list_pages(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<wiki_page::Model>> {
    wiki_page_ops::list_by_repo(db, repo_id).await
}

/// Update a wiki page.
pub async fn update_page(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
) -> Result<wiki_page::Model> {
    let updated =
        update_page_transactionally(db, repo_id, title, content, message, author_id, |_| {
            std::future::ready(())
        })
        .await?;

    // Update the cross-backend metadata FTS index (non-fatal).
    let page_id = updated.id;
    let page_title = updated.title.clone();
    let page_content = updated.content.clone();
    let backend = db.get_database_backend();
    let sql = metadata_fts_upsert_sql(backend, "wiki_pages_fts", "title, content");
    if let Err(e) = db
        .execute(sea_orm::Statement::from_sql_and_values(
            backend,
            rg_db::prepare_sql(backend, &sql),
            [page_id.into(), page_title.into(), page_content.into()],
        ))
        .await
    {
        tracing::warn!(error = %format!("{e:#}"), page_id = %page_id, "failed to update wiki_pages_fts index");
    }

    Ok(updated)
}

/// Serialize `read page -> snapshot -> overwrite` through the page's monotonic
/// edit token. The callback is a private test seam: production supplies a
/// ready future, while the regression test can stop both first attempts after
/// they have read the same token without depending on scheduler timing.
async fn update_page_transactionally<F, Fut>(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
    after_read: F,
) -> Result<wiki_page::Model>
where
    F: Fn(usize) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    const MAX_UPDATE_ATTEMPTS: usize = 32;

    for attempt in 1..=MAX_UPDATE_ATTEMPTS {
        let transaction = match db.begin().await {
            Ok(transaction) => transaction,
            Err(error)
                if attempt < MAX_UPDATE_ATTEMPTS
                    && rg_db::is_retryable_transaction_error(&error) =>
            {
                continue;
            }
            Err(error) => return Err(error).context("begin wiki page update transaction"),
        };

        let write_result: Result<Option<wiki_page::Model>> = async {
            let existing = wiki_page_ops::find_by_repo_and_title(&transaction, repo_id, title)
                .await
                .context("find wiki page for update")?
                .ok_or_else(|| crate::error::not_found("wiki page"))?;
            after_read(attempt).await;

            let next_version = existing
                .edit_version
                .checked_add(1)
                .context("wiki edit version exhausted")?;
            let revision = wiki_revision::ActiveModel {
                id: sea_orm::NotSet,
                wiki_page_id: sea_orm::Set(existing.id),
                content: sea_orm::Set(existing.content.clone()),
                message: sea_orm::Set(existing.message.clone()),
                author_id: sea_orm::Set(existing.author_id),
                version: sea_orm::Set(next_version),
                created_at: sea_orm::Set(Utc::now()),
            };
            wiki_revision_ops::create(&transaction, revision)
                .await
                .context("snapshot wiki page before update")?;

            wiki_page_ops::update_if_version(
                &transaction,
                &existing,
                content,
                message,
                author_id.or(existing.author_id),
                next_version,
                Utc::now(),
            )
            .await
        }
        .await;

        let updated = match write_result {
            Ok(Some(updated)) => updated,
            Ok(None) => {
                transaction
                    .rollback()
                    .await
                    .context("rollback stale wiki page update")?;
                if attempt == MAX_UPDATE_ATTEMPTS {
                    anyhow::bail!(
                        "serialize wiki page update after {MAX_UPDATE_ATTEMPTS} concurrent conflicts"
                    );
                }
                continue;
            }
            Err(error) => {
                let retryable = rg_db::is_unique_violation_anyhow(&error)
                    || rg_db::is_retryable_transaction_error_anyhow(&error);
                if let Err(rollback_error) = transaction.rollback().await {
                    return Err(error).context(format!(
                        "wiki page update failed and its transaction could not be rolled back: \
                         {rollback_error}"
                    ));
                }
                if retryable && attempt < MAX_UPDATE_ATTEMPTS {
                    continue;
                }
                if retryable {
                    return Err(error).context(format!(
                        "serialize wiki page update after {MAX_UPDATE_ATTEMPTS} concurrent conflicts"
                    ));
                }
                return Err(error);
            }
        };

        match transaction.commit().await {
            Ok(()) => return Ok(updated),
            Err(error)
                if attempt < MAX_UPDATE_ATTEMPTS
                    && rg_db::is_retryable_transaction_error(&error) =>
            {
                continue;
            }
            Err(error) if rg_db::is_retryable_transaction_error(&error) => {
                return Err(error).context(format!(
                    "commit wiki page update after {MAX_UPDATE_ATTEMPTS} concurrent conflicts"
                ));
            }
            Err(error) => return Err(error).context("commit wiki page update transaction"),
        }
    }

    unreachable!("the bounded wiki update loop returns or continues on every attempt")
}

/// List all revisions for a wiki page (newest first).
pub async fn list_revisions(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
) -> Result<Vec<wiki_revision::Model>> {
    let page = wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("find wiki page for revisions")?
        .ok_or_else(|| crate::error::not_found("wiki page"))?;
    wiki_revision_ops::list_by_page(db, page.id).await
}

/// Get a specific revision's content, scoped to the page that owns it.
///
/// `revision_id` is global, so the repository and title from the route decide
/// which revisions are addressable at all. Taking them here rather than trusting
/// the caller to compare afterwards is what keeps one repository's history out
/// of a URL that points at another one; a revision of a different page is
/// `Ok(None)`, indistinguishable from a revision that does not exist.
pub async fn get_revision(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
    revision_id: i64,
) -> Result<Option<wiki_revision::Model>> {
    let page = wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("find wiki page for revision")?
        .ok_or_else(|| crate::error::not_found("wiki page"))?;

    let Some(revision) = wiki_revision_ops::find_by_id(db, revision_id)
        .await
        .context("find wiki revision")?
    else {
        return Ok(None);
    };

    if revision.wiki_page_id != page.id {
        return Ok(None);
    }

    Ok(Some(revision))
}

/// Delete a wiki page.
pub async fn delete_page(db: &DatabaseConnection, repo_id: i64, title: &str) -> Result<()> {
    let existing = wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("find wiki page for delete")?
        .ok_or_else(|| crate::error::not_found("wiki page"))?;

    let page_id = existing.id;

    // Delete from the cross-backend metadata FTS index (non-fatal).
    let backend = db.get_database_backend();
    if let Err(e) = db
        .execute(sea_orm::Statement::from_sql_and_values(
            backend,
            rg_db::prepare_sql(backend, "DELETE FROM wiki_pages_fts WHERE rowid = ?"),
            [page_id.into()],
        ))
        .await
    {
        tracing::warn!(error = %format!("{e:#}"), page_id = %page_id, "failed to delete from wiki_pages_fts index");
    }

    // The lookup above and this `DELETE` are two statements, so a concurrent
    // delete can empty the row out from under it; zero rows reports `not_found`
    // rather than confirming a deletion this call did not perform.
    if wiki_page_ops::delete_by_id(db, page_id).await? {
        Ok(())
    } else {
        Err(crate::error::not_found("wiki page"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn same_snapshot_edits_form_a_complete_history_chain() {
        let dir = tempfile::tempdir().expect("tempdir");
        let db_path = dir.path().join("wiki-lost-update.db");
        let db = rg_db::connect_with_pool(
            &format!("sqlite://{}?mode=rwc", db_path.display()),
            rg_db::TEST_CONNECT_TIMEOUT_SECS,
            60,
            2,
        )
        .await
        .expect("connect sqlite");
        rg_db::run_migrations(&db).await.expect("run migrations");

        let user = rg_db::ops::user_ops::create_user(
            &db,
            "wikicas",
            "wikicas@example.invalid",
            "",
            "Wiki CAS",
        )
        .await
        .expect("create user");
        let repo = crate::repo::service::create_repo(
            &db,
            user.id,
            "wiki-cas",
            None,
            false,
            &dir.path().join("repos"),
            None,
        )
        .await
        .expect("create repo");
        create_page(&db, repo.id, "Home", "v0", None, Some(user.id))
            .await
            .expect("create page");

        let after_same_read = std::sync::Arc::new(tokio::sync::Barrier::new(2));
        let first_gate = after_same_read.clone();
        let first = update_page_transactionally(
            &db,
            repo.id,
            "Home",
            "A",
            None,
            Some(user.id),
            move |attempt| {
                let gate = first_gate.clone();
                async move {
                    if attempt == 1 {
                        gate.wait().await;
                    }
                }
            },
        );
        let second_gate = after_same_read.clone();
        let second = update_page_transactionally(
            &db,
            repo.id,
            "Home",
            "B",
            None,
            Some(user.id),
            move |attempt| {
                let gate = second_gate.clone();
                async move {
                    if attempt == 1 {
                        gate.wait().await;
                    }
                }
            },
        );

        let (first, second) = tokio::join!(first, second);
        first.expect("first same-snapshot edit succeeds");
        second.expect("second same-snapshot edit succeeds");

        let current = get_page(&db, repo.id, "Home")
            .await
            .expect("read current page")
            .expect("page exists");
        let revisions = list_revisions(&db, repo.id, "Home")
            .await
            .expect("read revisions");
        assert_eq!(
            revisions
                .iter()
                .map(|revision| revision.version)
                .collect::<Vec<_>>(),
            vec![2, 1]
        );
        assert_eq!(current.edit_version, 2);

        let mut preserved_states = revisions
            .iter()
            .map(|revision| revision.content.as_str())
            .chain(std::iter::once(current.content.as_str()))
            .collect::<Vec<_>>();
        preserved_states.sort_unstable();
        assert_eq!(preserved_states, vec!["A", "B", "v0"]);
    }
}
