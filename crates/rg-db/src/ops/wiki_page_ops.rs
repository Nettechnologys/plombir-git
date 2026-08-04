//! Database operations for wiki pages.

use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::wiki_page::{self, ActiveModel, Entity as WikiEntity, Model as WikiPage};

/// Find a wiki page by (repo_id, title).
pub async fn find_by_repo_and_title<C>(
    db: &C,
    repo_id: i64,
    title: &str,
) -> Result<Option<WikiPage>>
where
    C: ConnectionTrait,
{
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
pub async fn create<C>(db: &C, model: ActiveModel) -> Result<WikiPage>
where
    C: ConnectionTrait,
{
    model.insert(db).await.context("db: create wiki page")
}

/// Update a wiki page.
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<WikiPage> {
    model.update(db).await.context("db: update wiki page")
}

/// Update one page only while its edit token still names the state the caller read.
///
/// Returning `Ok(None)` is an ordinary concurrent-edit loss. The caller must
/// roll back every side effect derived from `expected` and retry from a fresh
/// read; in particular, a revision snapshot must not survive this result.
pub async fn update_if_version<C>(
    db: &C,
    expected: &WikiPage,
    content: &str,
    message: Option<&str>,
    author_id: Option<i64>,
    next_version: i32,
    updated_at: chrono::DateTime<chrono::Utc>,
) -> Result<Option<WikiPage>>
where
    C: ConnectionTrait,
{
    let result = WikiEntity::update_many()
        .col_expr(wiki_page::Column::Content, Expr::value(content))
        .col_expr(
            wiki_page::Column::Message,
            Expr::value(message.map(str::to_string)),
        )
        .col_expr(wiki_page::Column::AuthorId, Expr::value(author_id))
        .col_expr(wiki_page::Column::Sha, Expr::value(Option::<String>::None))
        .col_expr(wiki_page::Column::EditVersion, Expr::value(next_version))
        .col_expr(wiki_page::Column::UpdatedAt, Expr::value(updated_at))
        .filter(wiki_page::Column::Id.eq(expected.id))
        .filter(wiki_page::Column::EditVersion.eq(expected.edit_version))
        .exec(db)
        .await
        .context("db: compare-and-swap wiki page")?;

    if result.rows_affected != 1 {
        return Ok(None);
    }

    WikiEntity::find_by_id(expected.id)
        .one(db)
        .await
        .context("db: read compare-and-swap wiki page")
}

/// Delete a wiki page by id. `Ok(false)` means no such row.
///
/// The caller's lookup and this `DELETE` are two statements: reporting
/// `rows_affected` is what stops a route from confirming a deletion that a
/// concurrent request had already performed.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = WikiEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete wiki page")?;
    Ok(result.rows_affected > 0)
}
