use crate::entities::ci_secret::{self, ActiveModel, Entity, Model};
use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

/// Every secret of a repository, in every scope.
///
/// This is the settings listing. Building a job's environment must not use it:
/// it would hand the job the secrets of environments it never passed the gate
/// of. That is [`list_for_job`].
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Model>> {
    Entity::find()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .order_by_asc(ci_secret::Column::Name)
        .order_by_asc(ci_secret::Column::EnvironmentId)
        .all(db)
        .await
        .context("db: list CI secrets")
}

/// The secrets a job may be handed, one per name.
///
/// A job reads its repository's repository-wide scope, plus the scope of
/// `environment_id` when it declares an environment. A job with an environment
/// has passed that environment's gate by the time a runner asks: a protected
/// environment holds it in `waiting_approval` until it is approved
/// (`ci_environment_ops::attach_job`).
///
/// A name set in both scopes resolves here, to the environment's value, so no
/// consumer has to: both runners fold the list into a map, and with two rows of
/// one name the winner would be whichever the backend happened to return last.
/// A production job then deploys with the repository-wide `DEPLOY_TOKEN` its
/// own environment exists to override.
pub async fn list_for_job(
    db: &DatabaseConnection,
    repo_id: i64,
    environment_id: Option<i64>,
) -> Result<Vec<Model>> {
    let rows = Entity::find()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .filter(
            Condition::any()
                .add(ci_secret::Column::EnvironmentId.is_null())
                .add_option(environment_id.map(|id| ci_secret::Column::EnvironmentId.eq(id))),
        )
        .all(db)
        .await
        .context("db: list CI secrets for a job")?;
    Ok(environment_shadows_repository(rows))
}

/// One row per name, the environment's row winning over the repository-wide
/// one, in name order.
fn environment_shadows_repository(rows: Vec<Model>) -> Vec<Model> {
    let mut by_name = std::collections::BTreeMap::<String, Model>::new();
    for row in rows {
        match by_name.entry(row.name.clone()) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(row);
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                if row.environment_id.is_some() {
                    entry.insert(row);
                }
            }
        }
    }
    by_name.into_values().collect()
}

/// The names of one environment's secrets, for the record of what deleting
/// that environment takes with it.
pub async fn names_in_environment(
    db: &DatabaseConnection,
    environment_id: i64,
) -> Result<Vec<String>> {
    Entity::find()
        .select_only()
        .column(ci_secret::Column::Name)
        .filter(ci_secret::Column::EnvironmentId.eq(environment_id))
        .order_by_asc(ci_secret::Column::Name)
        .into_tuple()
        .all(db)
        .await
        .context("db: list an environment's CI secrets")
}

/// `environment_id IS NULL` for the repository-wide scope, an equality for an
/// environment: one place deciding what an absent environment means.
fn scope(environment_id: Option<i64>) -> Condition {
    Condition::all().add(match environment_id {
        Some(id) => ci_secret::Column::EnvironmentId.eq(id),
        None => ci_secret::Column::EnvironmentId.is_null(),
    })
}

/// One scope's secret by name.
pub async fn find_by_repo_and_name(
    db: &DatabaseConnection,
    repo_id: i64,
    environment_id: Option<i64>,
    name: &str,
) -> Result<Option<Model>> {
    Entity::find()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .filter(scope(environment_id))
        .filter(ci_secret::Column::Name.eq(name))
        .one(db)
        .await
        .context("db: find CI secret")
}
/// Store a CI secret under `name` in one scope, creating it on first use.
///
/// `(repo_id, scope, name)` is UNIQUE (`uq_ci_secrets_repo_environment_name`,
/// over the generated `scope_key`, so the repository-wide `NULL` scope is
/// covered too), and the lookup
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
/// repo, environment or actor does not exist) or a broken connection stays an error — the
/// secret genuinely was not stored, and reporting success would leave CI
/// reading a value nobody wrote.
pub async fn upsert(
    db: &DatabaseConnection,
    repo_id: i64,
    environment_id: Option<i64>,
    name: &str,
    encrypted_value: &str,
    actor_id: i64,
) -> Result<Option<Model>> {
    let now = chrono::Utc::now();
    if let Some(model) = find_by_repo_and_name(db, repo_id, environment_id, name).await? {
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
            match find_by_repo_and_name(db, repo_id, environment_id, name).await? {
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
/// The lookup in [`upsert`] is a separate statement, so a concurrent DELETE
/// can win before this write. `None` reports that ordinary absence without
/// leaking SeaORM's backend-shaped `RecordNotUpdated`, and this update-only
/// primitive cannot answer the DELETE by recreating the secret.
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
/// Delete one scope's secret by name. The same name in another scope is a
/// different secret and stays.
pub async fn delete_by_repo_and_name(
    db: &DatabaseConnection,
    repo_id: i64,
    environment_id: Option<i64>,
    name: &str,
) -> Result<bool> {
    let result = Entity::delete_many()
        .filter(ci_secret::Column::RepoId.eq(repo_id))
        .filter(scope(environment_id))
        .filter(ci_secret::Column::Name.eq(name))
        .exec(db)
        .await
        .context("db: delete CI secret")?;
    Ok(result.rows_affected > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn secret(id: i64, name: &str, environment_id: Option<i64>) -> Model {
        let now = chrono::Utc::now();
        Model {
            id,
            repo_id: 1,
            name: name.to_string(),
            encrypted_value: format!("value-{id}"),
            created_by_id: None,
            environment_id,
            created_at: now,
            updated_at: now,
        }
    }

    /// Whichever order the backend returns two rows of one name in, the
    /// environment's row is the one a job gets.
    #[test]
    fn the_environment_value_shadows_the_repository_wide_one_in_either_order() {
        let repository = secret(1, "DEPLOY_TOKEN", None);
        let environment = secret(2, "DEPLOY_TOKEN", Some(11));
        let other = secret(3, "NPM_TOKEN", None);
        for rows in [
            vec![repository.clone(), environment.clone(), other.clone()],
            vec![other.clone(), environment.clone(), repository.clone()],
        ] {
            let resolved = environment_shadows_repository(rows);
            assert_eq!(
                resolved.iter().map(|row| row.id).collect::<Vec<_>>(),
                vec![2, 3],
                "one row per name, the environment's winning, in name order"
            );
        }
    }
}
