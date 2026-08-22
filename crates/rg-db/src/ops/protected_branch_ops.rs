//! Database operations for protected branches.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::protected_branch::{
    self, ActiveModel, Entity as PbEntity, Model as ProtectedBranch,
};
use crate::user_grants::{self, Target};

/// A branch rule paired with the normalized, mirror-verified push allow-list.
#[derive(Clone, Debug)]
pub struct Rule {
    pub protection: ProtectedBranch,
    pub allowed_push_user_ids: Vec<i64>,
}

async fn with_grants(db: &impl ConnectionTrait, protection: ProtectedBranch) -> Result<Rule> {
    let allowed_push_user_ids = user_grants::load_verified(
        db,
        Target::ProtectedBranch(protection.id),
        protection.allowed_push_user_ids.as_deref(),
    )
    .await?;
    Ok(Rule {
        protection,
        allowed_push_user_ids,
    })
}

/// Find a protected branch rule by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<ProtectedBranch>> {
    PbEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find protected branch by id")
}

/// Find a protected branch rule by repo and branch name.
pub async fn find_by_repo_and_branch(
    db: &DatabaseConnection,
    repo_id: i64,
    branch_name: &str,
) -> Result<Option<ProtectedBranch>> {
    PbEntity::find()
        .filter(protected_branch::Column::RepoId.eq(repo_id))
        .filter(protected_branch::Column::BranchName.eq(branch_name))
        .one(db)
        .await
        .context("db: find protected branch by repo and branch")
}

// `find_rule_by_repo_and_branch` used to live here. Its one caller was
// `branch_protection::service::check_push_allowed`, the drifted second copy of
// the push gate; the live path loads every rule of the repository at once
// through `list_rules_by_repo`, because a push carries many refs
// (card_ab36709fa0c7).

/// List all protected branch rules for a repo.
pub async fn list_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<ProtectedBranch>> {
    PbEntity::find()
        .filter(protected_branch::Column::RepoId.eq(repo_id))
        .order_by_asc(protected_branch::Column::BranchName)
        .all(db)
        .await
        .context("db: list protected branches by repo")
}

/// List branch rules with normalized allow-lists, failing on mirror drift.
pub async fn list_rules_by_repo(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<Rule>> {
    let protections = list_by_repo(db, repo_id).await?;
    let mut rules = Vec::with_capacity(protections.len());
    for protection in protections {
        rules.push(with_grants(db, protection).await?);
    }
    Ok(rules)
}

/// Create a rule and both allow-list representations in one transaction.
pub async fn create_with_push_grants(
    db: &DatabaseConnection,
    model: ActiveModel,
    allowed_push_user_ids: Option<Vec<i64>>,
) -> Result<ProtectedBranch> {
    let transaction = db
        .begin()
        .await
        .context("db: begin protected branch grant write")?;
    let result: Result<ProtectedBranch> = async {
        let created = model
            .insert(&transaction)
            .await
            .context("db: create protected branch")?;
        user_grants::replace(
            &transaction,
            Target::ProtectedBranch(created.id),
            allowed_push_user_ids.as_deref(),
        )
        .await?;
        PbEntity::find_by_id(created.id)
            .one(&transaction)
            .await
            .context("db: reload protected branch after grant write")?
            .context("db: protected branch disappeared during grant write")
    }
    .await;
    match result {
        Ok(created) => {
            transaction
                .commit()
                .await
                .context("db: commit protected branch grant write")?;
            Ok(created)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(error).context(format!(
                    "db: roll back protected branch grant write: {rollback_error}"
                ));
            }
            Err(error)
        }
    }
}

/// Update a rule and, when supplied, replace both allow-list representations.
pub async fn update_with_push_grants(
    db: &DatabaseConnection,
    model: ActiveModel,
    allowed_push_user_ids: Option<Vec<i64>>,
) -> Result<ProtectedBranch> {
    let transaction = db
        .begin()
        .await
        .context("db: begin protected branch grant update")?;
    let result: Result<ProtectedBranch> = async {
        let updated = model
            .update(&transaction)
            .await
            .context("db: update protected branch")?;
        if let Some(ids) = allowed_push_user_ids.as_deref() {
            user_grants::replace(&transaction, Target::ProtectedBranch(updated.id), Some(ids))
                .await?;
        }
        PbEntity::find_by_id(updated.id)
            .one(&transaction)
            .await
            .context("db: reload protected branch after grant update")?
            .context("db: protected branch disappeared during grant update")
    }
    .await;
    match result {
        Ok(updated) => {
            transaction
                .commit()
                .await
                .context("db: commit protected branch grant update")?;
            Ok(updated)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(error).context(format!(
                    "db: roll back protected branch grant update: {rollback_error}"
                ));
            }
            Err(error)
        }
    }
}

/// Delete a protected branch rule by ID. `Ok(false)` means no such row.
///
/// The caller's scope lookup and this `DELETE` are two statements: reporting
/// `rows_affected` is what stops a route from confirming a deletion that a
/// concurrent request had already performed.
pub async fn delete_by_id(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = PbEntity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete protected branch")?;
    Ok(result.rows_affected > 0)
}
