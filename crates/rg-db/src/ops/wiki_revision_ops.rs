//! Database operations for wiki revisions.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::wiki_revision::{self, ActiveModel, Entity as WikiRevisionEntity, Model};

/// Create a new wiki revision.
pub async fn create<C>(db: &C, model: ActiveModel) -> Result<Model>
where
    C: ConnectionTrait,
{
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

/// Find a wiki revision by its ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>> {
    WikiRevisionEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find wiki revision by id")
}
