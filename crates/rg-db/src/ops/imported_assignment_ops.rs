//! Database operations for the bylines of who an imported issue or pull
//! request was assigned to on its source.

use anyhow::{Context, Result};
use sea_orm::*;

use crate::entities::imported_assignment::{
    self, ActiveModel, Entity as ImportedAssignmentEntity, Model as ImportedAssignment,
};

/// What an imported issue or pull request is: the byline's other half.
#[derive(Debug, Clone, Copy)]
pub enum AssignedObject {
    Issue(i64),
    PullRequest(i64),
}

/// The person a byline names, as the source spells them.
#[derive(Debug, Clone)]
pub struct SourcePerson<'a> {
    pub login: &'a str,
    pub source_user_id: Option<i64>,
    pub service: &'a str,
}

/// Record `people` as holding `role` on `object`.
pub async fn record(
    db: &impl ConnectionTrait,
    object: AssignedObject,
    role: &str,
    people: &[SourcePerson<'_>],
) -> Result<()> {
    if people.is_empty() {
        return Ok(());
    }
    let (issue_id, pull_request_id) = match object {
        AssignedObject::Issue(id) => (Some(id), None),
        AssignedObject::PullRequest(id) => (None, Some(id)),
    };
    ImportedAssignmentEntity::insert_many(people.iter().map(|person| ActiveModel {
        id: NotSet,
        issue_id: Set(issue_id),
        pull_request_id: Set(pull_request_id),
        role: Set(role.to_string()),
        login: Set(person.login.to_string()),
        source_user_id: Set(person.source_user_id),
        service: Set(person.service.to_string()),
    }))
    .exec(db)
    .await
    .context("db: record imported assignments")?;
    Ok(())
}

/// The bylines of `object`, in the order they were recorded.
pub async fn list_for(
    db: &impl ConnectionTrait,
    object: AssignedObject,
) -> Result<Vec<ImportedAssignment>> {
    let query = match object {
        AssignedObject::Issue(id) => {
            ImportedAssignmentEntity::find().filter(imported_assignment::Column::IssueId.eq(id))
        }
        AssignedObject::PullRequest(id) => ImportedAssignmentEntity::find()
            .filter(imported_assignment::Column::PullRequestId.eq(id)),
    };
    query
        .order_by_asc(imported_assignment::Column::Id)
        .all(db)
        .await
        .context("db: list imported assignments")
}
