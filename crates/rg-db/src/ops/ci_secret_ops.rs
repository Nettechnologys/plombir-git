use crate::entities::ci_secret::{self, ActiveModel, Entity, Model};
use anyhow::{Context, Result};
use sea_orm::*;

pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Model>> {
    Entity::find()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .order_by_asc(ci_secret::Column::Name)
        .all(db)
        .await
        .context("db: list CI secrets")
}
pub async fn find_by_repo_and_name(
    db: &DatabaseConnection,
    repo_id: i64,
    name: &str,
) -> Result<Option<Model>> {
    Entity::find()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .filter(ci_secret::Column::Name.eq(name))
        .one(db)
        .await
        .context("db: find CI secret")
}
pub async fn upsert(
    db: &DatabaseConnection,
    repo_id: i64,
    name: &str,
    encrypted_value: &str,
    actor_id: i64,
) -> Result<Model> {
    let now = chrono::Utc::now();
    if let Some(model) = find_by_repo_and_name(db, repo_id, name).await? {
        let mut active: ActiveModel = model.into();
        active.encrypted_value = Set(encrypted_value.to_owned());
        active.updated_at = Set(now);
        active.update(db).await.context("db: update CI secret")
    } else {
        ActiveModel {
            id: NotSet,
            repo_id: Set(repo_id),
            name: Set(name.to_owned()),
            encrypted_value: Set(encrypted_value.to_owned()),
            created_by_id: Set(actor_id),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .context("db: create CI secret")
    }
}
pub async fn delete_by_repo_and_name(
    db: &DatabaseConnection,
    repo_id: i64,
    name: &str,
) -> Result<bool> {
    let result = Entity::delete_many()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .filter(ci_secret::Column::Name.eq(name))
        .exec(db)
        .await
        .context("db: delete CI secret")?;
    Ok(result.rows_affected > 0)
}
