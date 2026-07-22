//! Label service — business logic for label CRUD.

use anyhow::{bail, Context, Result};
use chrono::Utc;
use sea_orm::{ActiveValue::Set, DatabaseConnection};

use rg_db::entities::label::{ActiveModel as LabelActiveModel, Model as Label};
use rg_db::entities::repository;
use rg_db::ops::{issue_label_ops, label_ops, repo_ops, user_ops};

/// List all labels for a repository.
pub async fn list_labels(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<Vec<Label>> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    label_ops::list_by_repo(db, repo.id).await
}

/// Get a single label by ID.
pub async fn get_label(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    label_id: i64,
) -> Result<Label> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    let label = label_ops::find_by_id(db, label_id)
        .await?
        .context("label not found")?;
    if label.repo_id != repo.id {
        bail!("label not found");
    }
    Ok(label)
}

/// Create a new label.
pub async fn create_label(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    name: String,
    color: String,
    description: Option<String>,
) -> Result<Label> {
    let repo = resolve_repo(db, owner, repo_name).await?;

    if name.trim().is_empty() {
        bail!("label name cannot be empty");
    }
    if !color.starts_with('#') || color.len() != 7 {
        bail!("color must be a hex string like #ff0000");
    }

    let now = Utc::now();
    let model = LabelActiveModel {
        repo_id: Set(repo.id),
        name: Set(name),
        color: Set(color),
        description: Set(description),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    label_ops::create(db, model).await
}

/// Update an existing label.
pub async fn update_label(
    db: &DatabaseConnection,
    label_id: i64,
    name: Option<String>,
    color: Option<String>,
    description: Option<Option<String>>,
) -> Result<Label> {
    let mut label = label_ops::find_by_id(db, label_id)
        .await?
        .context("label not found")?;

    if let Some(n) = name {
        if n.trim().is_empty() {
            bail!("label name cannot be empty");
        }
        label.name = n;
    }
    if let Some(c) = color {
        if !c.starts_with('#') || c.len() != 7 {
            bail!("invalid color: must be a hex string like #ff0000");
        }
        label.color = c;
    }
    if let Some(d) = description {
        label.description = d;
    }

    label.updated_at = Utc::now();

    let active: LabelActiveModel = label.into();
    label_ops::update(db, active).await
}

/// Delete a label.
pub async fn delete_label(db: &DatabaseConnection, label_id: i64) -> Result<()> {
    // Delete all issue_labels referencing this label first
    issue_label_ops::delete_by_label_id(db, label_id).await?;
    label_ops::delete_by_id(db, label_id).await
}

/// Get labels for an issue (batch query — avoids N+1).
pub async fn get_issue_labels(db: &DatabaseConnection, issue_id: i64) -> Result<Vec<Label>> {
    let label_ids = issue_label_ops::get_label_ids(db, issue_id).await?;
    label_ops::find_by_ids(db, &label_ids).await
}

/// Set labels for an issue.
pub async fn set_issue_labels(
    db: &DatabaseConnection,
    issue_id: i64,
    label_ids: Vec<i64>,
) -> Result<()> {
    issue_label_ops::set_labels(db, issue_id, label_ids).await
}

/// Resolve owner/repo_name to a repository model.
async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<repository::Model> {
    let user = user_ops::find_by_username(db, owner)
        .await?
        .context("owner not found")?;
    repo_ops::find_by_owner_and_name(db, user.id, repo_name)
        .await?
        .context("repository not found")
}
