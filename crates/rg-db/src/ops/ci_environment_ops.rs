use crate::entities::{
    ci_environment, ci_environment_approval, pipeline_job,
    user::{self, Entity as UserEntity},
};
use crate::user_grants::{self, Target};
use anyhow::{Context, Result};
use sea_orm::sea_query::{Expr, Query};
use sea_orm::*;

pub async fn list(db: &DatabaseConnection, repo_id: i64) -> Result<Vec<ci_environment::Model>> {
    ci_environment::Entity::find()
        .filter(ci_environment::Column::RepoId.eq(repo_id))
        .order_by_asc(ci_environment::Column::Name)
        .all(db)
        .await
        .context("db: list CI environments")
}
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<ci_environment::Model>> {
    ci_environment::Entity::find_by_id(id)
        .one(db)
        .await
        .context("db: find CI environment")
}
/// Connection-agnostic: pipeline creation resolves a job's environment inside
/// the transaction that writes the job.
pub async fn find_by_name(
    db: &impl ConnectionTrait,
    repo_id: i64,
    name: &str,
) -> Result<Option<ci_environment::Model>> {
    ci_environment::Entity::find()
        .filter(ci_environment::Column::RepoId.eq(repo_id))
        .filter(ci_environment::Column::Name.eq(name))
        .one(db)
        .await
        .context("db: find CI environment by name")
}
/// Every environment name this repository carries, in listing order.
///
/// Connection-agnostic for the same reason as [`find_by_name`]: a job that
/// names an environment the repository does not have is refused from inside
/// the pipeline transaction, and the refusal has to tell the author which
/// names they could have meant.
pub async fn list_names(db: &impl ConnectionTrait, repo_id: i64) -> Result<Vec<String>> {
    ci_environment::Entity::find()
        .select_only()
        .column(ci_environment::Column::Name)
        .filter(ci_environment::Column::RepoId.eq(repo_id))
        .order_by_asc(ci_environment::Column::Name)
        .into_tuple::<String>()
        .all(db)
        .await
        .context("db: list CI environment names")
}
pub async fn create_with_approvers(
    db: &DatabaseConnection,
    model: ci_environment::ActiveModel,
    allowed_approver_ids: Vec<i64>,
) -> Result<ci_environment::Model> {
    let transaction = db
        .begin()
        .await
        .context("db: begin CI environment grant write")?;
    let result: Result<ci_environment::Model> = async {
        let created = model
            .insert(&transaction)
            .await
            .context("db: create CI environment")?;
        user_grants::replace(
            &transaction,
            Target::CiEnvironment(created.id),
            Some(&allowed_approver_ids),
        )
        .await?;
        ci_environment::Entity::find_by_id(created.id)
            .one(&transaction)
            .await
            .context("db: reload CI environment after grant write")?
            .context("db: CI environment disappeared during grant write")
    }
    .await;
    match result {
        Ok(created) => {
            transaction
                .commit()
                .await
                .context("db: commit CI environment grant write")?;
            Ok(created)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(error).context(format!(
                    "db: roll back CI environment grant write: {rollback_error}"
                ));
            }
            Err(error)
        }
    }
}
pub async fn update(
    db: &DatabaseConnection,
    model: ci_environment::ActiveModel,
) -> Result<ci_environment::Model> {
    model.update(db).await.context("db: update CI environment")
}
/// Update an environment and its normalized approver set in one transaction.
///
/// The HTTP layer's repository-scoped lookup is a separate statement. A
/// concurrent delete can therefore win before this transaction begins; `None`
/// is that ordinary absent outcome. The explicit `(id, repo_id)` predicate also
/// keeps the write anchored to the repository that was authorized by the
/// caller instead of trusting a previously-loaded active model.
#[allow(clippy::too_many_arguments)]
pub async fn update_with_approvers(
    db: &DatabaseConnection,
    id: i64,
    repo_id: i64,
    name: String,
    protected: bool,
    required_approvals: i32,
    updated_at: chrono::DateTime<chrono::Utc>,
    allowed_approver_ids: Vec<i64>,
) -> Result<Option<ci_environment::Model>> {
    let transaction = db
        .begin()
        .await
        .context("db: begin CI environment grant update")?;
    let result: Result<Option<ci_environment::Model>> = async {
        let update = ci_environment::Entity::update_many()
            .col_expr(ci_environment::Column::Name, Expr::value(name))
            .col_expr(ci_environment::Column::Protected, Expr::value(protected))
            .col_expr(
                ci_environment::Column::RequiredApprovals,
                Expr::value(required_approvals),
            )
            .col_expr(ci_environment::Column::UpdatedAt, Expr::value(updated_at))
            .filter(ci_environment::Column::Id.eq(id))
            .filter(ci_environment::Column::RepoId.eq(repo_id))
            .exec(&transaction)
            .await
            .context("db: update CI environment")?;

        match update.rows_affected {
            // MySQL can report zero for a no-op UPDATE. Re-read inside the
            // transaction to distinguish that from a concurrent DELETE without
            // depending on its affected-row connection setting.
            0 => {
                let still_exists = ci_environment::Entity::find()
                    .filter(ci_environment::Column::Id.eq(id))
                    .filter(ci_environment::Column::RepoId.eq(repo_id))
                    .one(&transaction)
                    .await
                    .context("db: verify zero-row CI environment update")?;
                if still_exists.is_none() {
                    return Ok(None);
                }
            }
            1 => {}
            rows => anyhow::bail!(
                "db: CI environment update affected {rows} rows for id {id} in repo {repo_id}"
            ),
        }

        user_grants::replace(
            &transaction,
            Target::CiEnvironment(id),
            Some(&allowed_approver_ids),
        )
        .await?;
        let updated = ci_environment::Entity::find()
            .filter(ci_environment::Column::Id.eq(id))
            .filter(ci_environment::Column::RepoId.eq(repo_id))
            .one(&transaction)
            .await
            .context("db: reload CI environment after grant update")?
            .context("db: CI environment disappeared during grant update")?;
        Ok(Some(updated))
    }
    .await;
    match result {
        Ok(updated) => {
            transaction
                .commit()
                .await
                .context("db: commit CI environment grant update")?;
            Ok(updated)
        }
        Err(error) => {
            if let Err(rollback_error) = transaction.rollback().await {
                return Err(error).context(format!(
                    "db: roll back CI environment grant update: {rollback_error}"
                ));
            }
            Err(error)
        }
    }
}

/// Read the normalized approver list and verify its JSON wire mirror.
pub async fn allowed_approver_ids(
    db: &DatabaseConnection,
    environment: &ci_environment::Model,
) -> Result<Vec<i64>> {
    user_grants::load_verified(
        db,
        Target::CiEnvironment(environment.id),
        environment.allowed_approver_ids.as_deref(),
    )
    .await
}
/// Delete an environment, reporting whether this call is the one that removed it.
///
/// The caller looks the environment up and checks it for pipeline history
/// before getting here, and those are separate statements from this one: a
/// concurrent delete can win in between. `false` means the row was already
/// gone, which is not the same outcome as "deleted" and must not be answered
/// as one.
pub async fn delete(db: &DatabaseConnection, id: i64) -> Result<bool> {
    let result = ci_environment::Entity::delete_by_id(id)
        .exec(db)
        .await
        .context("db: delete CI environment")?;
    Ok(result.rows_affected > 0)
}

/// Attach a job to its environment, gating it behind approval when the
/// environment is protected.
///
/// Connection-agnostic: this is part of building a job, so it runs inside the
/// pipeline-creation transaction — a protected job must never become visible
/// as plain `pending` first.
pub async fn attach_job(
    db: &impl ConnectionTrait,
    job_id: i64,
    environment: Option<&ci_environment::Model>,
    environment_name: &str,
) -> Result<()> {
    let mut update = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .col_expr(
            pipeline_job::Column::EnvironmentName,
            Expr::value(environment_name),
        );
    if let Some(environment) = environment {
        update = update.col_expr(
            pipeline_job::Column::EnvironmentId,
            Expr::value(environment.id),
        );
        if environment.protected {
            update = update.col_expr(
                pipeline_job::Column::Status,
                Expr::value("waiting_approval"),
            );
        }
    }
    update
        .exec(db)
        .await
        .context("db: attach job environment")?;
    Ok(())
}

pub async fn add_approval(
    db: &DatabaseConnection,
    job_id: i64,
    environment_id: i64,
    approved_by: i64,
) -> Result<bool> {
    let model = ci_environment_approval::ActiveModel {
        job_id: Set(job_id),
        environment_id: Set(environment_id),
        approved_by: Set(Some(approved_by)),
        created_at: Set(chrono::Utc::now()),
        ..Default::default()
    };
    match model.insert(db).await {
        Ok(_) => Ok(true),
        // `(job_id, approved_by)` is UNIQUE: this approver has already signed
        // off on this job, so there is nothing to add and `false` says so.
        // Classified from the backend's error code — the message text this used
        // to match on is worded differently per backend, and MySQL's duplicate
        // -entry message does not contain the word "unique" at all.
        Err(error) if crate::is_unique_violation(&error) => Ok(false),
        Err(error) => Err(error).context("db: add environment approval"),
    }
}
/// Count approvals that still carry a current authorization verdict.
///
/// Approval rows are durable history and survive account deletion with a null
/// actor. A missing, deactivated, or retiring approver must not keep a waiting
/// deployment authorized, even though [`list_approvals`] still returns the row.
pub async fn count_approvals(db: &DatabaseConnection, job_id: i64) -> Result<u64> {
    UserEntity::find()
        .filter(
            user::Column::Id.in_subquery(
                Query::select()
                    .column(ci_environment_approval::Column::ApprovedBy)
                    .from(ci_environment_approval::Entity)
                    .and_where(ci_environment_approval::Column::JobId.eq(job_id))
                    .to_owned(),
            ),
        )
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::DeletedAt.is_null())
        .count(db)
        .await
        .context("db: count live environment approvers")
}
pub async fn release_approved_job(db: &DatabaseConnection, job_id: i64) -> Result<bool> {
    let result = pipeline_job::Entity::update_many()
        .filter(pipeline_job::Column::Id.eq(job_id))
        .filter(pipeline_job::Column::Status.eq("waiting_approval"))
        .col_expr(pipeline_job::Column::Status, Expr::value("pending"))
        .col_expr(
            pipeline_job::Column::UpdatedAt,
            Expr::value(chrono::Utc::now().naive_utc()),
        )
        .exec(db)
        .await
        .context("db: release approved environment job")?;
    Ok(result.rows_affected == 1)
}

pub async fn has_jobs(db: &DatabaseConnection, environment_id: i64) -> Result<bool> {
    Ok(pipeline_job::Entity::find()
        .filter(pipeline_job::Column::EnvironmentId.eq(environment_id))
        .count(db)
        .await
        .context("db: count environment jobs")?
        > 0)
}
