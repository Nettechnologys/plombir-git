//! Wiki service — CRUD operations for repository wiki pages.
//!
//! Each repository can have an associated wiki. Wiki pages are stored in the
//! database for fast querying and optionally mirrored to a `.wiki.git` bare
//! repository on disk for version control.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::{ConnectionTrait, DatabaseConnection};

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
    let already_exists = || {
        crate::error::invalid_request(format!(
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
    let existing = wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("find wiki page for update")?
        .ok_or_else(|| crate::error::not_found("wiki page"))?;

    // Save the current content as a revision before overwriting.
    //
    // A failed lookup is not "this page has no history": `latest_version`
    // already says that with `Ok(0)`, so the only thing `unwrap_or(0)` ever
    // caught was the query failing — and it filed the snapshot as version 1 on
    // top of the version 1 that was already there. Nothing downstream can tell
    // the two apart: `wiki_revisions` is indexed by page alone, with no unique
    // key on (wiki_page_id, version), so the history list and "restore this
    // version" then pick between duplicates arbitrarily.
    //
    // A revision that fails to *insert* is still non-fatal below — we know
    // where it belonged, we just could not store it. Not knowing the number is
    // different, and it costs the caller only a retry: the page has not been
    // overwritten yet, so nothing is lost by refusing here.
    let next_version = wiki_revision_ops::latest_version(db, existing.id)
        .await
        .context("find latest wiki revision version")?
        + 1;
    let rev = wiki_revision::ActiveModel {
        id: sea_orm::NotSet,
        wiki_page_id: sea_orm::Set(existing.id),
        content: sea_orm::Set(existing.content.clone()),
        message: sea_orm::Set(existing.message.clone()),
        author_id: sea_orm::Set(existing.author_id),
        version: sea_orm::Set(next_version),
        created_at: sea_orm::Set(Utc::now()),
    };
    // Non-fatal: a lost revision must not block the edit the user asked for.
    // It is still a lost revision — the pre-edit content becomes unrecoverable
    // the moment the page below is overwritten, so say which one went missing.
    if let Err(error) = wiki_revision_ops::create(db, rev).await {
        tracing::warn!(
            wiki_page_id = existing.id,
            repo_id,
            title = %existing.title,
            version = next_version,
            error = %format!("{error:#}"),
            "wiki revision not saved — the page is still being updated, so its previous content is lost from the history"
        );
    }

    let model = wiki_page::ActiveModel {
        id: sea_orm::Set(existing.id),
        repo_id: sea_orm::Set(existing.repo_id),
        title: sea_orm::Set(existing.title),
        content: sea_orm::Set(content.to_string()),
        message: sea_orm::Set(message.map(|s| s.to_string())),
        author_id: sea_orm::Set(author_id.or(existing.author_id)),
        sha: sea_orm::Set(None),
        created_at: sea_orm::Set(existing.created_at),
        updated_at: sea_orm::Set(Utc::now()),
    };

    let updated = wiki_page_ops::update(db, model).await?;

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
