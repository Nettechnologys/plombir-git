use crate::entities::protected_tag::{self, ActiveModel, Entity, Model};
use anyhow::{Context, Result};
use sea_orm::*;

pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Model>> {
    Entity::find()
        .filter(protected_tag::Column::RepoId.eq(repo_id))
        .order_by_asc(protected_tag::Column::Pattern)
        .all(db)
        .await
        .context("db: list protected tags")
}
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Model>> {
    Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find protected tag")
}
pub async fn create(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.insert(db).await.context("db: create protected tag")
}
pub async fn update(db: &DatabaseConnection, model: ActiveModel) -> Result<Model> {
    model.update(db).await.context("db: update protected tag")
}
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<()> {
    Entity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete protected tag")?;
    Ok(())
}
