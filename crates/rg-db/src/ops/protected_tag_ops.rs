use crate::entities::protected_tag::{self, ActiveModel, Entity, Model};
use crate::user_grants::{self, Target};
use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

/// A tag rule paired with the normalized, mirror-verified push allow-list.
#[derive(Clone, Debug)]
pub struct Rule {
    pub protection: Model,
    pub allowed_user_ids: Vec<i64>,
}

async fn with_grants(db: &impl ConnectionTrait, protection: Model) -> Result<Rule> {
    let allowed_user_ids = user_grants::load_verified(
        db,
        Target::ProtectedTag(protection.id),
        protection.allowed_user_ids.as_deref(),
    )
    .await?;
    Ok(Rule {
        protection,
        allowed_user_ids,
    })
}

pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Model>> {
    Entity::find()
        .filter(protected_tag::Column::RepoId.eq(repo_id))
        .order_by_asc(protected_tag::Column::Pattern)
        .all(db)
        .await
        .context("db: list protected tags")
}
pub async fn list_rules_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Rule>> {
    let protections = list_by_repo(db, repo_id).await?;
    let mut rules = Vec::with_capacity(protections.len());
    for protection in protections {
        rules.push(with_grants(db, protection).await?);
    }
    Ok(rules)
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
pub async fn create_with_push_grants(
    db: &DatabaseConnection,
    model: ActiveModel,
    allowed_user_ids: Option<Vec<i64>>,
) -> Result<Model> {
    let transaction = db
        .begin()
        .await
        .context("db: begin protected tag grant write")?;
    let result: Result<Model> = async {
        let created = model
            .insert(&transaction)
            .await
            .context("db: create protected tag")?;
        user_grants::replace(
            &transaction,
            Target::ProtectedTag(created.id),
            allowed_user_ids.as_deref(),
        )
        .await?;
        Entity::find_by_id(created.id)
            .one(&transaction)
            .await
            .context("db: reload protected tag after grant write")?
            .context("db: protected tag disappeared during grant write")
    }
    .await;
    match result {
        Ok(created) => {
            transaction
                .commit()
                .await
                .context("db: commit protected tag grant write")?;
            Ok(created)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(error).context(format!(
                    "db: roll back protected tag grant write: {rollback_error}"
                ));
            }
            Err(error)
        }
    }
}
pub async fn update_with_push_grants(
    db: &DatabaseConnection,
    model: Model,
    allowed_user_ids: Vec<i64>,
) -> Result<Option<Model>> {
    let id = model.id;
    let repo_id = model.repo_id;
    let transaction = db
        .begin()
        .await
        .context("db: begin protected tag grant update")?;
    let result: Result<Option<Model>> = async {
        let update = Entity::update_many()
            .col_expr(
                protected_tag::Column::UpdatedAt,
                Expr::value(model.updated_at),
            )
            .filter(protected_tag::Column::Id.eq(id))
            .filter(protected_tag::Column::RepoId.eq(repo_id))
            .exec(&transaction)
            .await
            .context("db: update protected tag")?;

        match update.rows_affected {
            // MySQL may report zero for a no-op UPDATE. Re-read inside this
            // transaction before classifying zero as a winning DELETE.
            0 => {
                let still_exists = Entity::find()
                    .filter(protected_tag::Column::Id.eq(id))
                    .filter(protected_tag::Column::RepoId.eq(repo_id))
                    .one(&transaction)
                    .await
                    .context("db: verify zero-row protected tag update")?;
                if still_exists.is_none() {
                    return Ok(None);
                }
            }
            1 => {}
            rows => anyhow::bail!(
                "db: protected tag update affected {rows} rows for id {id} in repo {repo_id}"
            ),
        }

        user_grants::replace(
            &transaction,
            Target::ProtectedTag(id),
            Some(&allowed_user_ids),
        )
        .await?;
        let updated = Entity::find()
            .filter(protected_tag::Column::Id.eq(id))
            .filter(protected_tag::Column::RepoId.eq(repo_id))
            .one(&transaction)
            .await
            .context("db: reload protected tag after grant update")?
            .context("db: protected tag disappeared during grant update")?;
        Ok(Some(updated))
    }
    .await;
    match result {
        Ok(Some(updated)) => {
            transaction
                .commit()
                .await
                .context("db: commit protected tag grant update")?;
            Ok(Some(updated))
        }
        Ok(None) => {
            transaction
                .rollback()
                .await
                .context("db: roll back absent protected tag grant update")?;
            Ok(None)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(error).context(format!(
                    "db: roll back protected tag grant update: {rollback_error}"
                ));
            }
            Err(error)
        }
    }
}
/// Delete a tag protection rule by id. `Ok(false)` means no such row.
///
/// The caller's scope lookup and this `DELETE` are two statements: reporting
/// `rows_affected` is what stops a route from confirming a deletion that a
/// concurrent request had already performed.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = Entity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete protected tag")?;
    Ok(result.rows_affected > 0)
}
