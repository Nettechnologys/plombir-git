//! Runner entity — maps to the `runners` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "runners")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    /// The one repository whose jobs this runner credential may claim.
    ///
    /// `None` exists only for rows issued before repository scoping was added;
    /// runtime authentication rejects those credentials until the operator
    /// explicitly re-registers them for an `owner/repo`.
    pub repo_id: Option<i64>,
    pub name: String,
    /// SHA-256 (hex) of the bearer token this runner authenticates with.
    ///
    /// The plaintext exists exactly once, in the response to
    /// `POST /runners/register`, and is never stored: nothing after issuance
    /// needs it, since every later use arrives from the outside and is checked
    /// by hashing the presented value (`ops::runner_ops::find_by_token`). Same
    /// shape as `access_token.token_hash` and `password_reset_token.token_hash`
    /// — a database dump hands out no working runner credential.
    pub token_hash: String,
    pub status: String,
    pub labels: String,
    pub last_seen_at: DateTimeUtc,
    pub version: Option<String>,
    pub os: Option<String>,
    pub arch: Option<String>,
    pub created_at: DateTimeUtc,
    pub updated_at: DateTimeUtc,
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::repository::Entity",
        from = "Column::RepoId",
        to = "super::repository::Column::Id"
    )]
    Repository,
}

impl ActiveModelBehavior for ActiveModel {}
