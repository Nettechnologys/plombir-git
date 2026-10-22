//! Imported assignment entity — maps to the `imported_assignments` table.
//!
//! Who an imported issue or pull request was assigned to, or whose review it
//! requested, on the platform it came from: a byline, never a local
//! assignment (see `m20261022_000003_imported_assignments`).

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "imported_assignments")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The issue this byline belongs to, or `None` for a pull request's.
    pub issue_id: Option<i64>,
    /// The pull request this byline belongs to, or `None` for an issue's.
    pub pull_request_id: Option<i64>,
    /// `assignee` or `requested_reviewer`.
    pub role: String,
    /// The login on the source.
    pub login: String,
    /// The source platform's numeric account id, when it sent one.
    pub source_user_id: Option<i64>,
    /// The platform instance that issued the id, spelled as
    /// `original_service` is on the other imported rows.
    pub service: String,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::issue::Entity",
        from = "Column::IssueId",
        to = "super::issue::Column::Id"
    )]
    Issue,
    #[sea_orm(
        belongs_to = "super::pull_request::Entity",
        from = "Column::PullRequestId",
        to = "super::pull_request::Column::Id"
    )]
    PullRequest,
}

impl Related<super::issue::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::Issue.def()
    }
}

impl Related<super::pull_request::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::PullRequest.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
