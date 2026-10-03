//! One transactional writer for every user-id authorization allow-list.
//!
//! The three APIs keep their historical JSON columns because those columns are
//! part of their response contracts.  The same grants also live in normalized
//! relations with foreign keys.  [`replace`] is the only production writer of
//! both representations and updates them in one transaction.

use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{bail, Context, Result};
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use thiserror::Error;

use crate::entities::{
    ci_environment, ci_environment_approver_grant, protected_branch, protected_branch_push_grant,
    protected_tag, protected_tag_push_grant, user,
};

/// The relational allow-list being read or replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Target {
    ProtectedBranch(i64),
    ProtectedTag(i64),
    CiEnvironment(i64),
}

impl Target {
    fn label(self) -> &'static str {
        match self {
            Self::ProtectedBranch(_) => "protected branch push grant",
            Self::ProtectedTag(_) => "protected tag push grant",
            Self::CiEnvironment(_) => "CI environment approver grant",
        }
    }
}

/// Request-level failures shared by branch, tag and environment writers.
#[derive(Clone, Debug, Error, PartialEq, Eq)]
pub enum InvalidPrincipal {
    #[error("user grant list contains duplicate user {0}")]
    Duplicate(i64),
    #[error("grant user {0} does not exist")]
    Missing(i64),
    #[error("grant user {0} is inactive")]
    Inactive(i64),
    #[error("grant user {0} is being retired")]
    Retiring(i64),
    #[error("CI environment approver {0} must be a human account")]
    BotApprover(i64),
}

/// Turn a shared validation error inside an `anyhow` chain into its stable
/// client-facing message. Database failures deliberately return `None`.
pub fn invalid_principal_message(error: &anyhow::Error) -> Option<String> {
    error
        .downcast_ref::<InvalidPrincipal>()
        .map(ToString::to_string)
}

fn unique_ids(ids: &[i64]) -> Result<Vec<i64>> {
    let mut seen = HashSet::new();
    let mut unique = Vec::with_capacity(ids.len());
    for &id in ids {
        if !seen.insert(id) {
            return Err(InvalidPrincipal::Duplicate(id).into());
        }
        unique.push(id);
    }
    Ok(unique)
}

async fn write_json_mirror(
    transaction: &DatabaseTransaction,
    target: Target,
    serialized: Option<String>,
) -> Result<()> {
    match target {
        Target::ProtectedBranch(id) => {
            protected_branch::Entity::update_many()
                .col_expr(
                    protected_branch::Column::AllowedPushUserIds,
                    Expr::value(serialized),
                )
                .filter(protected_branch::Column::Id.eq(id))
                .exec(transaction)
                .await
                .context("db: write protected branch grant mirror")?;
        }
        Target::ProtectedTag(id) => {
            protected_tag::Entity::update_many()
                .col_expr(
                    protected_tag::Column::AllowedUserIds,
                    Expr::value(serialized),
                )
                .filter(protected_tag::Column::Id.eq(id))
                .exec(transaction)
                .await
                .context("db: write protected tag grant mirror")?;
        }
        Target::CiEnvironment(id) => {
            ci_environment::Entity::update_many()
                .col_expr(
                    ci_environment::Column::AllowedApproverIds,
                    Expr::value(serialized),
                )
                .filter(ci_environment::Column::Id.eq(id))
                .exec(transaction)
                .await
                .context("db: write CI environment grant mirror")?;
        }
    }
    Ok(())
}

async fn validate_and_lock_users(
    transaction: &DatabaseTransaction,
    target: Target,
    ids: &[i64],
) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }

    // On PostgreSQL/MySQL this becomes SELECT ... FOR UPDATE, so retirement or
    // deletion cannot pass the validation and commit before the grants are
    // inserted. SeaQuery deliberately omits the clause on SQLite; the mirror
    // UPDATE above has already made this transaction a writer there, which
    // serializes every competing retirement/delete write until commit.
    let users = user::Entity::find()
        .filter(user::Column::Id.is_in(ids.iter().copied()))
        .lock_exclusive()
        .all(transaction)
        .await
        .context("db: lock user grant principals")?;
    let by_id: HashMap<_, _> = users.into_iter().map(|user| (user.id, user)).collect();

    for id in ids {
        let Some(principal) = by_id.get(id) else {
            return Err(InvalidPrincipal::Missing(*id).into());
        };
        if principal.deleted_at.is_some() {
            return Err(InvalidPrincipal::Retiring(*id).into());
        }
        if !principal.is_active {
            return Err(InvalidPrincipal::Inactive(*id).into());
        }
        if matches!(target, Target::CiEnvironment(_)) && principal.is_bot() {
            return Err(InvalidPrincipal::BotApprover(*id).into());
        }
    }
    Ok(())
}

async fn delete_relational(transaction: &DatabaseTransaction, target: Target) -> Result<()> {
    match target {
        Target::ProtectedBranch(id) => {
            protected_branch_push_grant::Entity::delete_many()
                .filter(protected_branch_push_grant::Column::ProtectedBranchId.eq(id))
                .exec(transaction)
                .await
                .context("db: clear protected branch push grants")?;
        }
        Target::ProtectedTag(id) => {
            protected_tag_push_grant::Entity::delete_many()
                .filter(protected_tag_push_grant::Column::ProtectedTagId.eq(id))
                .exec(transaction)
                .await
                .context("db: clear protected tag push grants")?;
        }
        Target::CiEnvironment(id) => {
            ci_environment_approver_grant::Entity::delete_many()
                .filter(ci_environment_approver_grant::Column::EnvironmentId.eq(id))
                .exec(transaction)
                .await
                .context("db: clear CI environment approver grants")?;
        }
    }
    Ok(())
}

async fn insert_relational(
    transaction: &DatabaseTransaction,
    target: Target,
    ids: &[i64],
) -> Result<()> {
    for &user_id in ids {
        match target {
            Target::ProtectedBranch(protected_branch_id) => {
                protected_branch_push_grant::ActiveModel {
                    protected_branch_id: Set(protected_branch_id),
                    user_id: Set(user_id),
                }
                .insert(transaction)
                .await
                .context("db: insert protected branch push grant")?;
            }
            Target::ProtectedTag(protected_tag_id) => {
                protected_tag_push_grant::ActiveModel {
                    protected_tag_id: Set(protected_tag_id),
                    user_id: Set(user_id),
                }
                .insert(transaction)
                .await
                .context("db: insert protected tag push grant")?;
            }
            Target::CiEnvironment(environment_id) => {
                ci_environment_approver_grant::ActiveModel {
                    environment_id: Set(environment_id),
                    user_id: Set(user_id),
                }
                .insert(transaction)
                .await
                .context("db: insert CI environment approver grant")?;
            }
        }
    }
    Ok(())
}

/// Atomically replace one allow-list's JSON mirror and normalized FK rows.
///
/// Callers must own the transaction through commit. Writing the mirror before
/// validation is intentional: it obtains SQLite's database writer lock. An
/// invalid principal rolls the mirror back with the rest of the transaction.
pub async fn replace(
    transaction: &DatabaseTransaction,
    target: Target,
    ids: Option<&[i64]>,
) -> Result<()> {
    let configured = ids.is_some();
    let ids = match ids {
        Some(ids) => unique_ids(ids)?,
        None => Vec::new(),
    };
    let serialized = if configured {
        Some(serde_json::to_string(&ids).context("serialize user grant list")?)
    } else {
        None
    };

    write_json_mirror(transaction, target, serialized).await?;
    validate_and_lock_users(transaction, target, &ids).await?;
    delete_relational(transaction, target).await?;
    insert_relational(transaction, target, &ids).await
}

async fn relational_ids(db: &impl sea_orm::ConnectionTrait, target: Target) -> Result<Vec<i64>> {
    let ids = match target {
        Target::ProtectedBranch(id) => protected_branch_push_grant::Entity::find()
            .select_only()
            .column(protected_branch_push_grant::Column::UserId)
            .filter(protected_branch_push_grant::Column::ProtectedBranchId.eq(id))
            .order_by_asc(protected_branch_push_grant::Column::UserId)
            .into_tuple()
            .all(db)
            .await
            .context("db: load protected branch push grants")?,
        Target::ProtectedTag(id) => protected_tag_push_grant::Entity::find()
            .select_only()
            .column(protected_tag_push_grant::Column::UserId)
            .filter(protected_tag_push_grant::Column::ProtectedTagId.eq(id))
            .order_by_asc(protected_tag_push_grant::Column::UserId)
            .into_tuple()
            .all(db)
            .await
            .context("db: load protected tag push grants")?,
        Target::CiEnvironment(id) => ci_environment_approver_grant::Entity::find()
            .select_only()
            .column(ci_environment_approver_grant::Column::UserId)
            .filter(ci_environment_approver_grant::Column::EnvironmentId.eq(id))
            .order_by_asc(ci_environment_approver_grant::Column::UserId)
            .into_tuple()
            .all(db)
            .await
            .context("db: load CI environment approver grants")?,
    };
    Ok(ids)
}

/// Load the normalized list and fail if its compatibility mirror drifted.
pub async fn load_verified(
    db: &impl sea_orm::ConnectionTrait,
    target: Target,
    json_mirror: Option<&str>,
) -> Result<Vec<i64>> {
    let relational = relational_ids(db, target).await?;
    let mirrored: Vec<i64> = match json_mirror {
        Some(json) => serde_json::from_str(json)
            .with_context(|| format!("stored {} JSON mirror is unreadable", target.label()))?,
        None => Vec::new(),
    };
    let mirrored: BTreeSet<_> = mirrored.into_iter().collect();
    let relational_set: BTreeSet<_> = relational.iter().copied().collect();
    if mirrored != relational_set {
        bail!(
            "stored {} JSON mirror disagrees with its relational grants",
            target.label()
        );
    }
    Ok(relational)
}
