//! Label service — business logic for label CRUD.

use anyhow::Result;
use chrono::Utc;
use sea_orm::{ActiveValue::Set, DatabaseConnection};

use rg_db::entities::label::{ActiveModel as LabelActiveModel, Model as Label};
use rg_db::entities::repository;
use rg_db::ops::{issue_label_ops, label_ops};

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
///
/// Both "no such label" outcomes carry [`crate::error::NotFound`], so the HTTP
/// layer can answer `404` to them *without* having to answer `404` to a failed
/// query as well. The mismatched-repo branch stays a plain "label not found" on
/// purpose: telling the caller that label #7 exists but belongs to someone
/// else's repository is a leak.
pub async fn get_label(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    label_id: i64,
) -> Result<Label> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    let label = label_ops::find_by_id(db, label_id)
        .await?
        .ok_or_else(|| crate::error::not_found("label"))?;
    if label.repo_id != repo.id {
        return Err(crate::error::not_found("label"));
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
        return Err(crate::error::invalid_request("label name cannot be empty"));
    }
    if !color.starts_with('#') || color.len() != 7 {
        return Err(crate::error::invalid_request(
            "color must be a hex string like #ff0000",
        ));
    }

    let now = Utc::now();
    let model = LabelActiveModel {
        repo_id: Set(repo.id),
        name: Set(name.clone()),
        color: Set(color),
        description: Set(description),
        created_at: Set(now),
        updated_at: Set(now),
        ..Default::default()
    };

    // `idx_labels_repo_name_unique` refuses a repeated name, and there is no
    // read before this insert to notice it first — so unlike the create paths
    // that carry a pre-check, this is not a race that only a loser hits. Every
    // caller who reuses a label name lands here, and the unclassified `DbErr`
    // left as a `500`: the API said the server had broken for the single most
    // ordinary mistake a labels UI can make.
    //
    // Classified rather than pre-read: a pre-check would add a second statement
    // whose answer this one already has, and the sequential and concurrent
    // cases would then need two different messages for one outcome.
    label_ops::create(db, model).await.map_err(|error| {
        if rg_db::is_unique_violation_anyhow(&error) {
            label_already_exists(&name)
        } else {
            error
        }
    })
}

/// The answer to a name this repository has already used.
///
/// `Conflict`, not `InvalidRequest`: the name is well-formed and the request is
/// correct — an existing row refuses it, and only deleting or renaming that row
/// changes the answer. Same reading as `mirror already exists for this
/// repository` and the taken-username branch of registration.
///
/// The name is echoed because the caller just sent it; nothing else from the
/// database reaches the client, so no constraint or `db:` text leaks (H-05).
fn label_already_exists(name: &str) -> anyhow::Error {
    crate::error::conflict(format!("label '{name}' already exists in this repository"))
}

/// Update an existing label.
///
/// Takes `owner`/`repo_name` rather than the label id alone: the HTTP layer
/// checks write access against a repository, so the label it then mutates has to
/// be the one that lives in it. With a bare id the two halves referred to
/// different rows, and write access to any one repository was enough to rename a
/// label in every other. Scoping in the signature makes that unbypassable —
/// there is no way left to call this without naming the repository.
pub async fn update_label(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    label_id: i64,
    name: Option<String>,
    color: Option<String>,
    description: Option<Option<String>>,
) -> Result<Label> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    let label = label_ops::find_by_id(db, label_id)
        .await?
        .ok_or_else(|| crate::error::not_found("label"))?;
    // Same reasoning as `get_label`: a label belonging to another repository is
    // reported as absent, not forbidden, so the route cannot be used to probe
    // which label ids exist.
    if label.repo_id != repo.id {
        return Err(crate::error::not_found("label"));
    }

    // The edits go on an `ActiveModel` as `Set(…)`, not on the `Model` before
    // converting it. `Model::into()` marks every field `Unchanged`, and sea-orm
    // omits `Unchanged` columns from the `UPDATE` — so mutating the model first
    // built a statement with nothing to set. `PATCH` then answered `200` with
    // the *old* row echoed back and wrote nothing at all.
    let mut active: LabelActiveModel = label.into();

    // Typed rather than `bail!`: the HTTP layer answers `400` only to
    // `InvalidRequest`, so a plain `anyhow!` here would arrive as a 500 and a
    // failed query would arrive as a 400.
    // Kept for the classification below — `active.name` is a `Set(…)` from here
    // on and the message needs the name the caller sent.
    let mut renamed_to = None;
    if let Some(n) = name {
        if n.trim().is_empty() {
            return Err(crate::error::invalid_request("label name cannot be empty"));
        }
        renamed_to = Some(n.clone());
        active.name = Set(n);
    }
    if let Some(c) = color {
        if !c.starts_with('#') || c.len() != 7 {
            return Err(crate::error::invalid_request(
                "invalid color: must be a hex string like #ff0000",
            ));
        }
        active.color = Set(c);
    }
    if let Some(d) = description {
        active.description = Set(d);
    }

    active.updated_at = Set(Utc::now());

    // A rename onto a name the repository already uses hits the same unique
    // index as `create_label`, one route over. Renaming `bug` to `enhancement`
    // is the same ordinary mistake as creating a second `enhancement`, and it
    // was answered the same wrong way — an unclassified 500.
    label_ops::update(db, active).await.map_err(|error| {
        match renamed_to {
            Some(name) if rg_db::is_unique_violation_anyhow(&error) => label_already_exists(&name),
            // A write that failed for any other reason — or one that never
            // touched the name — stays an error. Reporting an outage as the
            // caller's duplicate is the misattribution this whole class costs.
            _ => error,
        }
    })
}

/// Delete a label.
///
/// Repository-scoped for the same reason as [`update_label`] — and here the
/// unscoped version was strictly worse: it never read the label at all, so a
/// `DELETE` through any writable repository removed the row outright.
pub async fn delete_label(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
    label_id: i64,
) -> Result<()> {
    let repo = resolve_repo(db, owner, repo_name).await?;
    let label = label_ops::find_by_id(db, label_id)
        .await?
        .ok_or_else(|| crate::error::not_found("label"))?;
    if label.repo_id != repo.id {
        return Err(crate::error::not_found("label"));
    }

    // Delete all issue_labels referencing this label first
    issue_label_ops::delete_by_label_id(db, label_id).await?;
    // The scope check above is a separate statement, so a concurrent delete can
    // empty the row out from under it; reporting `Ok(())` for zero rows would
    // confirm a deletion this call did not perform.
    if label_ops::delete_by_id(db, label_id).await? {
        Ok(())
    } else {
        Err(crate::error::not_found("label"))
    }
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

/// Resolve label names to the ids of this repository's labels.
///
/// One helper for every path that turns names into ids — reading and writing
/// alike. The three copies of `filter(|l| names.contains(&l.name))` that
/// preceded it disagreed on what an unknown name meant: the filter on the read
/// path narrowed a search, the two on the write path dropped the label and
/// still answered `201`/`200` listing it. A name the repository does not have
/// is refused here, once, with the name in the message.
///
/// Repeats collapse: `["bug", "bug"]` is one label, not a condition that can
/// never match and not a duplicate insert.
pub async fn resolve_label_ids(
    db: &DatabaseConnection,
    repo_id: i64,
    names: &[String],
) -> Result<Vec<i64>> {
    if names.is_empty() {
        return Ok(Vec::new());
    }

    // A failed read is not "the repository has no such label": answering
    // `invalid_request` to a dead connection would blame the caller for our
    // outage, so the query's own error propagates.
    let all_labels = label_ops::list_by_repo(db, repo_id).await?;

    let mut ids: Vec<i64> = Vec::with_capacity(names.len());
    for name in names {
        let Some(label) = all_labels.iter().find(|l| &l.name == name) else {
            return Err(crate::error::invalid_request(format!(
                "unknown label: {}",
                name.chars().take(64).collect::<String>()
            )));
        };
        if !ids.contains(&label.id) {
            ids.push(label.id);
        }
    }
    Ok(ids)
}

/// Resolve owner/repo_name to a repository model.
///
/// The two "no such row" outcomes are typed rather than `.context(…)`-tagged:
/// every caller of this helper flattens its `anyhow::Error` at the HTTP layer,
/// and a context string is indistinguishable there from the `db: …` chain
/// `rg_db::ops` wraps a failed query in.
async fn resolve_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo_name: &str,
) -> Result<repository::Model> {
    crate::repo::service::find_repo_by_owner_name(db, owner, repo_name)
        .await?
        .ok_or_else(|| crate::error::not_found("repository"))
}
