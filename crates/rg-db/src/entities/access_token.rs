//! Access token entity — maps to the `access_tokens` table.

use sea_orm::entity::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, PartialEq, DeriveEntityModel, Serialize, Deserialize)]
#[sea_orm(table_name = "access_tokens")]
pub struct Model {
    #[sea_orm(primary_key)]
    pub id: i64,
    pub user_id: i64,
    pub name: String,
    /// SHA-256 hash of the raw token (the raw token is shown only once at creation)
    pub token_hash: String,
    /// Comma-separated scopes: "repo,user,admin"
    pub scopes: String,
    pub expires_at: Option<DateTimeUtc>,
    pub last_used_at: Option<DateTimeUtc>,
    pub created_at: DateTimeUtc,
    /// Whether the token may reach only the repositories listed for it in
    /// `access_token_repositories`. Kept apart from those rows so a token whose
    /// last repository was deleted stays confined to nothing rather than
    /// becoming unrestricted.
    pub repo_restricted: bool,
    /// Comma-separated MCP tool names. `Some` makes the token usable only
    /// through this instance's MCP endpoint, and only for these tools; `None`
    /// is an ordinary token.
    pub mcp_tools: Option<String>,
    /// Whether the token is refused every write that lands on a protected
    /// branch: a merge into one, a push to one, a server-side commit on one.
    pub deny_protected_merge: bool,
    /// Every ref this token may write, when it is confined (ADR-0001 D3). A
    /// value ending in `/` admits the refs under it, any other value exactly
    /// that one ref. `None` is an ordinary token.
    pub write_ref_prefix: Option<String>,
    /// Whether a token confined to MCP tools may use the git transport too.
    /// REST outside the MCP endpoint stays closed to it either way.
    pub allow_git: bool,
    /// The agent run attempt the token was issued for. Deleting the run
    /// deletes the token.
    #[sea_orm(unique)]
    pub agent_run_attempt_id: Option<i64>,
}

impl Model {
    /// The MCP tools this token is confined to, or `None` when it is not.
    pub fn mcp_tool_list(&self) -> Option<Vec<&str>> {
        self.mcp_tools.as_deref().map(|tools| {
            tools
                .split(',')
                .map(str::trim)
                .filter(|tool| !tool.is_empty())
                .collect()
        })
    }

    /// Whether anything narrows this token beyond its scopes.
    pub fn is_restricted(&self) -> bool {
        self.repo_restricted
            || self.mcp_tools.is_some()
            || self.deny_protected_merge
            || self.write_ref_prefix.is_some()
    }
}

#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
pub enum Relation {
    #[sea_orm(
        belongs_to = "super::user::Entity",
        from = "Column::UserId",
        to = "super::user::Column::Id"
    )]
    User,
}

impl Related<super::user::Entity> for Entity {
    fn to() -> RelationDef {
        Relation::User.def()
    }
}

impl ActiveModelBehavior for ActiveModel {}
