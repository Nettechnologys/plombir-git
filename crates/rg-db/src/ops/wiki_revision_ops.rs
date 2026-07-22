//! Database operations for wiki revisions.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::wiki_revision::{self, ActiveModel, Entity as WikiRevisionEntity, Model};

/// Create a new wiki revision.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.insert(db).await.context("db: create wiki revision")
}

/// List all revisions for a wiki page, newest first.
pub async fn list_by_page(db: &DatabaseConnection, wiki_page_id: i64) -> Result<Vec<Model>> {
    WikiRevisionEntity::find()
        .filter(wiki_revision::Column::WikiPageId.eq(wiki_page_id))
        .order_by_desc(wiki_revision::Column::Version)
        .all(db)
        .await
        .context("db: list wiki revisions")
}

/// Get the latest version number for a wiki page (0 if none).
pub async fn latest_version(db: &DatabaseConnection, wiki_page_id: i64) -> Result<i32> {
    let rev = WikiRevisionEntity::find()
        .filter(wiki_revision::Column::WikiPageId.eq(wiki_page_id))
        .order_by_desc(wiki_revision::Column::Version)
        .one(db)
        .await
        .context("db: get latest wiki revision version")?;
    Ok(rev.map(|r| r.version).unwrap_or(0))
}

/// Find a wiki revision by its ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>> {
    WikiRevisionEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find wiki revision by id")
}
