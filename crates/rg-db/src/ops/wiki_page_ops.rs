//! Database operations for wiki pages.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::wiki_page::{self, ActiveModel, Entity as WikiEntity, Model as WikiPage};

/// Find a wiki page by (repo_id, title).
pub async fn find_by_repo_and_title(
    db: &DatabaseConnection,
    repo_id: i64,
    title: &str,
) -> Result<Option<WikiPage>> {
    WikiEntity::find()
        .filter(wiki_page::Column::RepoId.eq(repo_id))
        .filter(wiki_page::Column::Title.eq(title))
        .one(db)
        .await
        .context("db: find wiki page by repo and title")
}

/// List all wiki pages for a repo.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<WikiPage>> {
    WikiEntity::find()
        .filter(wiki_page::Column::RepoId.eq(repo_id))
        .order_by_asc(wiki_page::Column::Title)
        .all(db)
        .await
        .context("db: list wiki pages by repo")
}

/// Create a new wiki page.
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<WikiPage> {
    model.insert(db).await.context("db: create wiki page")
}

/// Update a wiki page.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<WikiPage> {
    model.update(db).await.context("db: update wiki page")
}

/// Delete a wiki page by id.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    WikiEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete wiki page")?;
    Ok(())
}
