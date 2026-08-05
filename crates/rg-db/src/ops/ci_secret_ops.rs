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
/// Store a repository CI secret under `name`, creating it on first use.
///
/// `(repo_id, name)` is UNIQUE (`uq_ci_secrets_repo_name`), and the lookup
/// below is a separate statement from the insert that follows it. Two admins
/// saving the same secret at once — or one impatient double-submit — both read
/// `None` and both insert; one meets the constraint. That loss says the secret
/// this call wanted to store now exists, so it is resolved by re-reading the
/// winner's row and writing this call's value onto it: last writer wins, as it
/// would have with the two calls a millisecond apart.
///
/// `created_by_id` deliberately stays the winner's. The row records who
/// introduced the secret; the loser is updating an existing one, and the
/// existing-row branch does not rewrite that field either.
///
/// Only a UNIQUE violation is treated this way. A foreign key failure (the
/// repo or actor does not exist) or a broken connection stays an error — the
/// secret genuinely was not stored, and reporting success would leave CI
/// reading a value nobody wrote.
pub async fn upsert(
    db: &DatabaseConnection,
    repo_id: i64,
    name: &str,
    encrypted_value: &str,
    actor_id: i64,
) -> Result<Model> {
    let now = chrono::Utc::now();
    if let Some(model) = find_by_repo_and_name(db, repo_id, name).await? {
        return apply_value(db, model, encrypted_value, now).await;
    }

    let insert = ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        name: Set(name.to_owned()),
        encrypted_value: Set(encrypted_value.to_owned()),
        created_by_id: Set(Some(actor_id)),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await;

    match insert {
        Ok(created) => Ok(created),
        Err(error) if crate::is_unique_violation(&error) => {
            match find_by_repo_and_name(db, repo_id, name).await? {
                Some(model) => apply_value(db, model, encrypted_value, now).await,
                // Not there after all, so the collision was on some other
                // constraint. Report the original failure.
                None => Err(error).context("db: create CI secret"),
            }
        }
        Err(error) => Err(error).context("db: create CI secret"),
    }
}

/// Write this call's ciphertext onto an existing secret row.
async fn apply_value(
    db: &DatabaseConnection,
    model: Model,
    encrypted_value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Model> {
    let mut active: ActiveModel = model.into();
    active.encrypted_value = Set(encrypted_value.to_owned());
    active.updated_at = Set(now);
    active.update(db).await.context("db: update CI secret")
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
