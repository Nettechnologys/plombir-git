//! `vars.*`: the configuration variables of repositories and organizations.
//!
//! A repository's variable overrides its organization's of the same name, as
//! on GitHub ([`for_repo`]). Values are stored as written — they are
//! configuration, not credentials — and the API shows them back.

use std::collections::BTreeMap;

use crate::entities::{ci_variable, org_ci_variable, organization, repository};
use anyhow::{Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::*;

/// Whose variables a call reads or writes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Owner {
    Repository(i64),
    Organization(i64),
}

/// One variable, whichever table holds it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Variable {
    pub name: String,
    pub value: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

impl From<ci_variable::Model> for Variable {
    fn from(model: ci_variable::Model) -> Self {
        Self {
            name: model.name,
            value: model.value,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl From<org_ci_variable::Model> for Variable {
    fn from(model: org_ci_variable::Model) -> Self {
        Self {
            name: model.name,
            value: model.value,
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

/// An owner's variables, in name order.
pub async fn list(db: &DatabaseConnection, owner: Owner) -> Result<Vec<Variable>> {
    Ok(match owner {
        Owner::Repository(repo_id) => ci_variable::Entity::find()
            .filter(ci_variable::Column::RepoId.eq(repo_id))
            .order_by_asc(ci_variable::Column::Name)
            .all(db)
            .await
            .context("db: list repository CI variables")?
            .into_iter()
            .map(Variable::from)
            .collect(),
        Owner::Organization(org_id) => org_ci_variable::Entity::find()
            .filter(org_ci_variable::Column::OrgId.eq(org_id))
            .order_by_asc(org_ci_variable::Column::Name)
            .all(db)
            .await
            .context("db: list organization CI variables")?
            .into_iter()
            .map(Variable::from)
            .collect(),
    })
}

async fn find(db: &DatabaseConnection, owner: Owner, name: &str) -> Result<Option<i64>> {
    Ok(match owner {
        Owner::Repository(repo_id) => ci_variable::Entity::find()
            .filter(ci_variable::Column::RepoId.eq(repo_id))
            .filter(ci_variable::Column::Name.eq(name))
            .one(db)
            .await
            .context("db: find repository CI variable")?
            .map(|model| model.id),
        Owner::Organization(org_id) => org_ci_variable::Entity::find()
            .filter(org_ci_variable::Column::OrgId.eq(org_id))
            .filter(org_ci_variable::Column::Name.eq(name))
            .one(db)
            .await
            .context("db: find organization CI variable")?
            .map(|model| model.id),
    })
}

/// Write `value` onto the row `id` of `owner`; whether the row was still there.
async fn update_value(
    db: &DatabaseConnection,
    owner: Owner,
    id: i64,
    value: &str,
    now: chrono::DateTime<chrono::Utc>,
) -> Result<bool> {
    let rows = match owner {
        Owner::Repository(repo_id) => {
            ci_variable::Entity::update_many()
                .col_expr(ci_variable::Column::Value, Expr::value(value.to_owned()))
                .col_expr(ci_variable::Column::UpdatedAt, Expr::value(now))
                .filter(ci_variable::Column::Id.eq(id))
                .filter(ci_variable::Column::RepoId.eq(repo_id))
                .exec(db)
                .await
                .context("db: update repository CI variable")?
                .rows_affected
        }
        Owner::Organization(org_id) => {
            org_ci_variable::Entity::update_many()
                .col_expr(
                    org_ci_variable::Column::Value,
                    Expr::value(value.to_owned()),
                )
                .col_expr(org_ci_variable::Column::UpdatedAt, Expr::value(now))
                .filter(org_ci_variable::Column::Id.eq(id))
                .filter(org_ci_variable::Column::OrgId.eq(org_id))
                .exec(db)
                .await
                .context("db: update organization CI variable")?
                .rows_affected
        }
    };
    Ok(rows > 0)
}

/// Whether [`upsert`] created the variable or replaced its value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Written {
    Created,
    Updated,
}

/// Store `value` under `name` for `owner`, creating the variable on first use.
///
/// `(owner, name)` is UNIQUE, and the lookup is a separate statement from the
/// insert: two concurrent first writes both read nothing and one meets the
/// constraint. That loss means the variable now exists, so it is resolved by
/// writing this call's value onto the winner's row — last writer wins, as it
/// would a millisecond apart. A variable deleted between the lookup and the
/// update is written again rather than reported as stored and missing.
pub async fn upsert(
    db: &DatabaseConnection,
    owner: Owner,
    name: &str,
    value: &str,
    actor_id: i64,
) -> Result<Written> {
    for _ in 0..3 {
        let now = chrono::Utc::now();
        if let Some(id) = find(db, owner, name).await? {
            if update_value(db, owner, id, value, now).await? {
                return Ok(Written::Updated);
            }
            continue;
        }
        let inserted = match owner {
            Owner::Repository(repo_id) => ci_variable::ActiveModel {
                id: NotSet,
                repo_id: Set(repo_id),
                name: Set(name.to_owned()),
                value: Set(value.to_owned()),
                created_by_id: Set(Some(actor_id)),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(db)
            .await
            .map(|_| ()),
            Owner::Organization(org_id) => org_ci_variable::ActiveModel {
                id: NotSet,
                org_id: Set(org_id),
                name: Set(name.to_owned()),
                value: Set(value.to_owned()),
                created_by_id: Set(Some(actor_id)),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(db)
            .await
            .map(|_| ()),
        };
        match inserted {
            Ok(()) => return Ok(Written::Created),
            Err(error) if crate::is_unique_violation(&error) => continue,
            Err(error) => return Err(error).context("db: create CI variable"),
        }
    }
    anyhow::bail!("db: CI variable `{name}` kept changing under three writes in a row")
}

/// Remove `owner`'s variable `name`; whether there was one.
pub async fn delete(db: &DatabaseConnection, owner: Owner, name: &str) -> Result<bool> {
    let rows = match owner {
        Owner::Repository(repo_id) => {
            ci_variable::Entity::delete_many()
                .filter(ci_variable::Column::RepoId.eq(repo_id))
                .filter(ci_variable::Column::Name.eq(name))
                .exec(db)
                .await
                .context("db: delete repository CI variable")?
                .rows_affected
        }
        Owner::Organization(org_id) => {
            org_ci_variable::Entity::delete_many()
                .filter(org_ci_variable::Column::OrgId.eq(org_id))
                .filter(org_ci_variable::Column::Name.eq(name))
                .exec(db)
                .await
                .context("db: delete organization CI variable")?
                .rows_affected
        }
    };
    Ok(rows > 0)
}

/// What `vars.*` reads for a workflow of `repo_id`: its organization's
/// variables — while the organization is not deleted — with the repository's
/// own laid over them, name by name.
pub async fn for_repo(db: &DatabaseConnection, repo_id: i64) -> Result<BTreeMap<String, String>> {
    let repo = repository::Entity::find_by_id(repo_id)
        .one(db)
        .await
        .context("db: find repository for CI variables")?
        .context("repository not found while reading its CI variables")?;
    let mut values = BTreeMap::new();
    if let Some(org_id) = repo.org_id {
        let live = organization::Entity::find_by_id(org_id)
            .one(db)
            .await
            .context("db: find organization for CI variables")?
            .is_some_and(|org| org.deleted_at.is_none());
        if live {
            for variable in list(db, Owner::Organization(org_id)).await? {
                values.insert(variable.name, variable.value);
            }
        }
    }
    for variable in list(db, Owner::Repository(repo_id)).await? {
        values.insert(variable.name, variable.value);
    }
    Ok(values)
}
