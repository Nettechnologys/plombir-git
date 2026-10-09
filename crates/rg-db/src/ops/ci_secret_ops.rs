use crate::entities::ci_secret::{self, ActiveModel, Entity, Model};
use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

/// Every secret of a repository, in every scope.
///
/// This is the settings-page listing. Job assembly must not call it: it would
/// hand a job the secrets of environments it never passed the gate for
/// (`m20261009_000004_ci_secret_environments`); use [`list_for_job`].
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Model>> {
    Entity::find()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .order_by_asc(ci_secret::Column::Name)
        .all(db)
        .await
        .context("db: list CI secrets")
}

/// The secrets a job may be handed: its repository's repository-wide scope,
/// plus the scope of `environment_id` when the job declares an environment.
///
/// `None` — a job with no `environment:` — reads only the repository-wide
/// scope. A `Some` job has passed its environment's gate by the time a runner
/// calls this (protected environments hold the job in `waiting_approval` until
/// an approver releases it; see `ci_environment_ops::attach_job`), so its
/// environment's secrets are in scope.
pub async fn list_for_job(
    db: &DatabaseConnection,
    repo_id: i64,
    environment_id: Option<i64>,
) -> Result<Vec<Model>> {
    Entity::find()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .filter(match environment_id {
            Some(environment_id) => Condition::any()
                .add(ci_secret::Column::EnvironmentId.is_null())
                .add(ci_secret::Column::EnvironmentId.eq(environment_id)),
            None => scope_filter(None),
        })
        .order_by_asc(ci_secret::Column::Name)
        .all(db)
        .await
        .context("db: list CI secrets for job")
}

/// `environment_id IS NULL` for the repository-wide scope, an equality
/// otherwise. Kept in one place so every lookup and uniqueness retry agrees on
/// which column an absent environment means.
fn scope_filter(environment_id: Option<i64>) -> Condition {
    match environment_id {
        Some(environment_id) => {
            Condition::all().add(ci_secret::Column::EnvironmentId.eq(environment_id))
        }
        None => Condition::all().add(ci_secret::Column::EnvironmentId.is_null()),
    }
}

/// One scope's secret by name.
pub async fn find_by_repo_environment_and_name(
    db: &DatabaseConnection,
    repo_id: i64,
    environment_id: Option<i64>,
    name: &str,
) -> Result<Option<Model>> {
    Entity::find()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .filter(scope_filter(environment_id))
        .filter(ci_secret::Column::Name.eq(name))
        .one(db)
        .await
        .context("db: find CI secret")
}

/// Store a CI secret under `name` in one scope, creating it on first use.
///
/// `(repo_id, scope, name)` is UNIQUE (`uq_ci_secrets_repo_environment_name`,
/// over the `scope_key` projection so the repository-wide `NULL` scope is
/// covered too), and the lookup below is a separate statement from the insert
/// that follows it. Two admins saving the same secret at once — or one
/// impatient double-submit — both read `None` and both insert; one meets the
/// constraint. That loss says the secret this call wanted to store now exists,
/// so it is resolved by re-reading the winner's row and writing this call's
/// value onto it: last writer wins, as it would have with the two calls a
/// millisecond apart.
///
/// `created_by_id` deliberately stays the winner's. The row records who
/// introduced the secret; the loser is updating an existing one, and the
/// existing-row branch does not rewrite that field either.
///
/// Only a UNIQUE violation is treated this way. A foreign key failure (the
/// repo, actor or environment does not exist) or a broken connection stays an
/// error — the secret genuinely was not stored, and reporting success would
/// leave CI reading a value nobody wrote.
pub async fn upsert_in_environment(
    db: &DatabaseConnection,
    repo_id: i64,
    environment_id: Option<i64>,
    name: &str,
    encrypted_value: &str,
    actor_id: i64,
) -> Result<Option<Model>> {
    let now = chrono::Utc::now();
    if let Some(model) =
        find_by_repo_environment_and_name(db, repo_id, environment_id, name).await?
    {
        return update_existing(db, model.id, model.repo_id, encrypted_value, now).await;
    }

    let insert = ActiveModel {
        id: NotSet,
        repo_id: Set(repo_id),
        name: Set(name.to_owned()),
        encrypted_value: Set(encrypted_value.to_owned()),
        created_by_id: Set(Some(actor_id)),
        environment_id: Set(environment_id),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await;

    match insert {
        Ok(created) => Ok(Some(created)),
        Err(error) if crate::is_unique_violation(&error) => {
            match find_by_repo_environment_and_name(db, repo_id, environment_id, name).await? {
                Some(model) => {
                    update_existing(db, model.id, model.repo_id, encrypted_value, now).await
                }
                // Not there after all, so the collision was on some other
                // constraint. Report the original failure.
                None => Err(error).context("db: create CI secret"),
            }
        }
        Err(error) => Err(error).context("db: create CI secret"),
    }
}

/// Write this call's ciphertext onto one already-observed secret row.
///
/// The lookup in [`upsert_in_environment`] is a separate statement, so a
/// concurrent DELETE can win before this write. `None` reports that ordinary
/// absence without leaking SeaORM's backend-shaped `RecordNotUpdated`, and
/// this update-only primitive cannot answer the DELETE by recreating the
/// secret.
pub async fn update_existing(
    db: &DatabaseConnection,
    id: i64,
    repo_id: i64,
    encrypted_value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<Option<Model>> {
    let result = Entity::update_many()
        .col_expr(
            ci_secret::Column::EncryptedValue,
            Expr::value(encrypted_value.to_owned()),
        )
        .col_expr(ci_secret::Column::UpdatedAt, Expr::value(now))
        .filter(ci_secret::Column::Id.eq(id))
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .exec(db)
        .await
        .context("db: update CI secret")?;
    match result.rows_affected {
        0 | 1 => {}
        rows => {
            anyhow::bail!("db: CI secret update affected {rows} rows for id {id} in repo {repo_id}")
        }
    }

    // MySQL may report zero affected rows for a no-op UPDATE. Re-read the same
    // stable identity on every backend to distinguish that from a winning
    // DELETE without accepting a replacement row under the same secret name.
    Entity::find()
        .filter(ci_secret::Column::Id.eq(id))
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .one(db)
        .await
        .context("db: find updated CI secret")
}

/// Delete one scope's secret by name. Only that scope's row is touched: the
/// same name in another environment is a different secret.
pub async fn delete_by_repo_environment_and_name(
    db: &DatabaseConnection,
    repo_id: i64,
    environment_id: Option<i64>,
    name: &str,
) -> Result<bool> {
    let result = Entity::delete_many()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .filter(scope_filter(environment_id))
        .filter(ci_secret::Column::Name.eq(name))
        .exec(db)
        .await
        .context("db: delete CI secret")?;
    Ok(result.rows_affected > 0)
}
