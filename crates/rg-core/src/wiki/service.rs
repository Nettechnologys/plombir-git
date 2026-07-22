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
    // Check for duplicate title
    if wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("check existing wiki page")?
        .is_some()
    {
        anyhow::bail!("wiki page '{}' already exists in this repository", title);
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

    let page = wiki_page_ops::create(db, model).await?;

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
        tracing::warn!(error = %e, page_id = %page_id, "failed to update wiki_pages_fts index");
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
        .ok_or_else(|| anyhow::anyhow!("wiki page '{}' not found", title))?;

    // Save the current content as a revision before overwriting.
    let next_version = wiki_revision_ops::latest_version(db, existing.id)
        .await
        .unwrap_or(0)
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
    let _ = wiki_revision_ops::create(db, rev).await; // non-fatal: revision save failure doesn't block page update

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
        tracing::warn!(error = %e, page_id = %page_id, "failed to update wiki_pages_fts index");
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
        .ok_or_else(|| anyhow::anyhow!("wiki page '{}' not found", title))?;
    wiki_revision_ops::list_by_page(db, page.id).await
}

/// Get a specific revision's content.
pub async fn get_revision(
    db: &DatabaseConnection,
    revision_id: i64,
) -> Result<Option<wiki_revision::Model>> {
    wiki_revision_ops::find_by_id(db, revision_id).await
}

/// Delete a wiki page.
pub async fn delete_page(db: &DatabaseConnection, repo_id: i64, title: &str) -> Result<()> {
    let existing = wiki_page_ops::find_by_repo_and_title(db, repo_id, title)
        .await
        .context("find wiki page for delete")?
        .ok_or_else(|| anyhow::anyhow!("wiki page '{}' not found", title))?;

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
        tracing::warn!(error = %e, page_id = %page_id, "failed to delete from wiki_pages_fts index");
    }

    wiki_page_ops::delete_by_id(db, page_id).await
}
