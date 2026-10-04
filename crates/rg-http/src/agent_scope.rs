//! What a narrowed Personal Access Token may reach — enforced as a layer, not
//! by each handler (card_60a80311d512).
//!
//! A token's *scopes* (`repo`, `user`, `admin`) pick API families and are
//! decided from the path in `pat_auth`. The narrowing an agent's token carries
//! goes further, and is decided here:
//!
//! - **Repositories.** A token confined to named repositories reaches a route
//!   only when the route is about one of them. Every route carries its declared
//!   [`Access`] level, so this layer — mounted on every route by
//!   [`crate::route_table::RouteTable`] — knows which routes are about a
//!   repository and which repository, without asking the handler. A route that
//!   is *not* about one repository (a listing, a search, account settings, an
//!   organization) is refused outright: its answer is drawn from everything the
//!   account can see, which is exactly what the confinement is meant to cut.
//!   Public routes, whose answer does not depend on who asks, stay open.
//!   The layer sees only the repository in the path, so a handler that reaches
//!   a second one — a fork pull request's head — confines that one itself
//!   ([`confine_pull_request_head`]), and a route that would mint a new
//!   repository refuses a confined token in `NamespaceCreate`.
//! - **MCP tools.** A token confined to named tools works only through this
//!   instance's MCP endpoint ([`MCP_ENDPOINT_PATH`]), which dispatches each tool
//!   call to the API in-process and marks those inner requests with
//!   [`McpToolCall`] — an extension no client can set. Decided in `pat_auth`,
//!   before routing, and again by the endpoint before it dispatches.
//! - **Protected branches.** Decided where the branch is known, in `rg-core`,
//!   from the [`rg_core::auth::credential_context`] the PAT middleware publishes.
//!
//! Every refusal answers `403` and writes `agent.scope_denied` to the audit log.

use std::collections::HashSet;
use std::sync::Arc;

use axum::extract::{FromRequestParts, RawPathParams, Request};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sea_orm::DatabaseConnection;

use crate::error::AppError;
use crate::route_table::Access;

/// The one REST route a tool-confined token may call directly.
pub const MCP_ENDPOINT_PATH: &str = "/api/v1/mcp";

/// Whether `path` is the MCP endpoint, as a layer inside the `/api/v1` nest
/// sees it — axum hands nested layers the path with the prefix stripped, so
/// both spellings have to be recognised.
pub(crate) fn is_mcp_endpoint(path: &str) -> bool {
    path == MCP_ENDPOINT_PATH || Some(path) == MCP_ENDPOINT_PATH.strip_prefix("/api/v1")
}

/// Marks a request the MCP endpoint dispatched in-process on an agent's behalf,
/// and names the tool it serves.
///
/// Lives only in request extensions: there is no header or query spelling of
/// it, so a client cannot claim an inner call it did not make through the
/// endpoint.
#[derive(Clone, Debug)]
pub struct McpToolCall {
    pub tool: String,
}

/// The Personal Access Token behind the current request, resolved once by the
/// PAT middleware and carried in request extensions to the layers below it.
#[derive(Clone)]
pub struct TokenGrant(Arc<GrantInner>);

struct GrantInner {
    token: rg_db::entities::access_token::Model,
    owner: rg_db::entities::user::Model,
    /// `Some` when the token is confined to repositories — possibly to none.
    repositories: Option<HashSet<i64>>,
    db: DatabaseConnection,
}

impl TokenGrant {
    /// Resolve the narrowing of `token`, reading its repository allow-list
    /// when it has one.
    pub(crate) async fn load(
        db: &DatabaseConnection,
        token: rg_db::entities::access_token::Model,
        owner: rg_db::entities::user::Model,
    ) -> anyhow::Result<Self> {
        let repositories = if token.repo_restricted {
            let mut by_token =
                rg_db::ops::token_ops::repository_ids_by_token(db, &[token.id]).await?;
            Some(
                by_token
                    .remove(&token.id)
                    .unwrap_or_default()
                    .into_iter()
                    .collect(),
            )
        } else {
            None
        };
        Ok(Self(Arc::new(GrantInner {
            token,
            owner,
            repositories,
            db: db.clone(),
        })))
    }

    pub fn token_id(&self) -> i64 {
        self.0.token.id
    }

    pub fn owner(&self) -> &rg_db::entities::user::Model {
        &self.0.owner
    }

    /// Whether the token is confined to repositories.
    pub fn is_repo_restricted(&self) -> bool {
        self.0.repositories.is_some()
    }

    /// Whether the token may reach `repository_id`.
    pub fn admits_repository(&self, repository_id: i64) -> bool {
        self.0
            .repositories
            .as_ref()
            .is_none_or(|allowed| allowed.contains(&repository_id))
    }

    /// The MCP tools the token is confined to, when it is.
    pub fn mcp_tools(&self) -> Option<Vec<&str>> {
        self.0.token.mcp_tool_list()
    }

    /// Whether the token may call `tool` through the MCP endpoint.
    pub fn admits_tool(&self, tool: &str) -> bool {
        self.mcp_tools().is_none_or(|tools| tools.contains(&tool))
    }

    pub fn denies_protected_writes(&self) -> bool {
        self.0.token.deny_protected_merge
    }

    /// Whether the token may be used outside the MCP endpoint at all.
    pub fn is_mcp_only(&self) -> bool {
        self.0.token.mcp_tools.is_some()
    }

    /// The narrowing published to `rg-core` for the duration of the request.
    pub(crate) fn credential_context(
        &self,
        mcp_tool: Option<String>,
    ) -> rg_core::auth::credential_context::CredentialContext {
        rg_core::auth::credential_context::CredentialContext {
            user_id: self.0.owner.id,
            token_id: Some(self.0.token.id),
            mcp_tool,
            deny_protected_writes: self.denies_protected_writes(),
        }
    }

    /// Refuse the request: a `403` naming `reason`, written to the audit log
    /// as `agent.scope_denied` with what was asked for.
    pub(crate) async fn deny(
        &self,
        headers: &axum::http::HeaderMap,
        message: &str,
        details: serde_json::Value,
    ) -> AppError {
        self.record_denial(headers, details).await;
        AppError::forbidden(message)
    }

    /// Journal a refusal a transport answers in a shape of its own — git's
    /// plain-text `403`, a refused ref in a push report.
    pub(crate) async fn record_denial(
        &self,
        headers: &axum::http::HeaderMap,
        details: serde_json::Value,
    ) {
        record_scope_denial(&self.0.db, &self.0.owner, self.0.token.id, headers, details).await;
    }
}

/// Refuse `action` on a fork pull request whose head repository the token may
/// not reach.
///
/// The route layer admits a pull-request route by the base repository in its
/// path. Diffing, merging, approving CI for or committing to a fork PR reads or
/// writes the head repository as well, and a token confined to the base must
/// not reach a repository outside its list through it. A same-repository PR has
/// its head in the base and always passes.
pub(crate) async fn confine_pull_request_head(
    grant: Option<&TokenGrant>,
    headers: &axum::http::HeaderMap,
    pr: &rg_db::entities::pull_request::Model,
    action: &str,
) -> Result<(), AppError> {
    let head_repo_id = pr.head_repo_id.unwrap_or(pr.repo_id);
    match grant {
        Some(grant) if !grant.admits_repository(head_repo_id) => Err(grant
            .deny(
                headers,
                "this token may not access the pull request head repository",
                serde_json::json!({
                    "reason": "repository_not_allowed",
                    "action": action,
                    "base_repo_id": pr.repo_id,
                    "head_repo_id": head_repo_id,
                }),
            )
            .await),
        _ => Ok(()),
    }
}

/// Write one `agent.scope_denied` row for a token that asked for more than it
/// was given.
pub(crate) async fn record_scope_denial(
    db: &DatabaseConnection,
    owner: &rg_db::entities::user::Model,
    token_id: i64,
    headers: &axum::http::HeaderMap,
    details: serde_json::Value,
) {
    let actor = rg_core::audit::AuditActor::resolve_after_the_fact(db, owner.id).await;
    let mut details = match details {
        serde_json::Value::Object(object) => object,
        other => {
            let mut object = serde_json::Map::new();
            object.insert("value".to_string(), other);
            object
        }
    };
    details.insert("token_id".to_string(), token_id.into());
    rg_core::audit::record(
        db,
        &actor,
        rg_core::auth::credential_context::SCOPE_DENIED_ACTION,
        Some("token"),
        Some(token_id),
        actor.name(),
        Some(headers),
        Some(serde_json::Value::Object(details)),
    )
    .await;
}

/// The per-route layer: refuse a repository-confined token any route that is
/// not about one of its repositories.
///
/// `gateway` is the MCP endpoint, the one non-repository route a confined
/// token is meant to call — the tool calls it dispatches come back through
/// this layer one by one, each judged on its own route.
pub(crate) async fn enforce(access: Access, gateway: bool, req: Request, next: Next) -> Response {
    let Some(grant) = req.extensions().get::<TokenGrant>().cloned() else {
        return next.run(req).await;
    };
    if !grant.is_repo_restricted() || gateway || matches!(access, Access::Public) {
        return next.run(req).await;
    }

    let method = req.method().to_string();
    let path = req.uri().path().to_string();
    let (mut parts, body) = req.into_parts();

    // A repository-level route is about one repository, and so is a route of a
    // foreign credential mechanism whose path names one — the LFS protocol is
    // declared by its credential rather than by a repository level. Anything
    // else answers from everything the account can see.
    if !(access.is_repo_scoped() || matches!(access, Access::Foreign(_))) {
        return grant
            .deny(
                &parts.headers,
                "this token is confined to specific repositories and may not use this route",
                serde_json::json!({
                    "reason": "route_outside_repositories",
                    "method": method,
                    "path": path,
                }),
            )
            .await
            .into_response();
    }

    let params = RawPathParams::from_request_parts(&mut parts, &())
        .await
        .ok()
        .map(|params| {
            params
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    let param = |name: &str| {
        params
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.clone())
    };
    let owner = param("owner");
    let name = param("name").or_else(|| param("repo"));

    let repository = match (&owner, &name) {
        (Some(owner), Some(name)) => {
            match rg_core::repo::service::find_repo_by_owner_name(&grant.0.db, owner, name).await {
                Ok(repository) => repository,
                Err(error) => return AppError::from(error).into_response(),
            }
        }
        // A route that names its repository through a row id
        // (`/artifacts/{id}`), or a foreign route that names none, cannot be
        // judged before its handler runs; a confined token is refused it
        // rather than trusted with it.
        _ => None,
    };
    if repository
        .as_ref()
        .is_some_and(|repository| grant.admits_repository(repository.id))
    {
        return next.run(Request::from_parts(parts, body)).await;
    }

    // An unknown repository and one outside the allow-list get the same answer,
    // so the confinement is not an existence oracle the account did not have.
    let target = match (&owner, &name) {
        (Some(owner), Some(name)) => format!("{owner}/{name}"),
        _ => path.clone(),
    };
    grant
        .deny(
            &parts.headers,
            &format!("this token may not access {target}"),
            serde_json::json!({
                "reason": "repository_not_allowed",
                "method": method,
                "path": path,
                "repository": target,
            }),
        )
        .await
        .into_response()
}
