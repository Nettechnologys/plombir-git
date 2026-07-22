//! Database operations for runners.

use anyhow::{Context, Result};
use chrono::Utc;
use sea_orm::sea_query::Expr;
use sea_orm::*;

use crate::entities::runner::{ActiveModel, Column, Entity as RunnerEntity, Model as Runner};

/// Register a new runner.
///
/// Generates a unique token and creates a runner record.
pub async fn register_runner(
    db: &DatabaseConnection,
    name: &str,
    labels: &str,
    version: Option<&str>,
    os: Option<&str>,
    arch: Option<&str>,
) -> Result<Runner> {
    let now = Utc::now();
    let token = generate_token();

    let active_model = ActiveModel {
        id: NotSet,
        name: Set(name.to_string()),
        token: Set(token),
        status: Set("offline".to_string()),
        labels: Set(labels.to_string()),
        last_seen_at: Set(now),
        version: Set(version.map(|v| v.to_string())),
        os: Set(os.map(|v| v.to_string())),
        arch: Set(arch.map(|v| v.to_string())),
        created_at: Set(now),
        updated_at: Set(now),
    };

    active_model.insert(db).await.context("db: register runner")
}

/// Update runner heartbeat (last_seen_at).
pub async fn update_heartbeat(db: &DatabaseConnection, runner_id: i64) -> Result<()> {
    let now = Utc::now();

    RunnerEntity::update_many()
        .col_expr(Column::LastSeenAt, Expr::value(now))
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Id.eq(runner_id))
        .exec(db)
        .await
        .context("db: update runner heartbeat")?;

    Ok(())
}

/// Update runner status.
pub async fn update_status(db: &DatabaseConnection, runner_id: i64, status: &str) -> Result<()> {
    let now = Utc::now().naive_utc();

    RunnerEntity::update_many()
        .col_expr(Column::Status, Expr::value(status.to_string()))
        .col_expr(Column::UpdatedAt, Expr::value(now))
        .filter(Column::Id.eq(runner_id))
        .exec(db)
        .await
        .context("db: update runner status")?;

    Ok(())
}

/// Find a runner by ID.
pub async fn find_by_id(db: &DatabaseConnection, id: i64) -> Result<Option<Runner>> {
    RunnerEntity::find_by_id(id)
        .one(db)
        .await
        .context("db: find runner by id")
}

/// Find a runner by token.
pub async fn find_by_token(db: &DatabaseConnection, token: &str) -> Result<Option<Runner>> {
    RunnerEntity::find()
        .filter(Column::Token.eq(token))
        .one(db)
        .await
        .context("db: find runner by token")
}

/// List online runners (last_seen_at within 90 seconds).
pub async fn list_online_runners(db: &DatabaseConnection) -> Result<Vec<Runner>> {
    let cutoff = Utc::now().naive_utc() - chrono::Duration::seconds(90);

    RunnerEntity::find()
        .filter(Column::LastSeenAt.gt(cutoff))
        .all(db)
        .await
        .context("db: list online runners")
}

/// List all runners (for admin).
pub async fn list_all(db: &DatabaseConnection) -> Result<Vec<Runner>> {
    RunnerEntity::find()
        .order_by_desc(Column::LastSeenAt)
        .all(db)
        .await
        .context("db: list all runners")
}

/// Delete a runner by ID.
pub async fn delete_runner(db: &DatabaseConnection, runner_id: i64) -> Result<bool> {
    let result = RunnerEntity::delete_by_id(runner_id).exec(db).await?;
    Ok(result.rows_affected > 0)
}

/// Generate a unique token for runner authentication.
fn generate_token() -> String {
    // Use UUID v4 to generate a unique token (36 chars with hyphens)
    // Remove hyphens to get 32-char token
    uuid::Uuid::new_v4().to_string().replace('-', "")
}
