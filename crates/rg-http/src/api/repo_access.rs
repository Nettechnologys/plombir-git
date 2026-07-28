//! Shared repository-scoped authorization helpers for REST handlers.
//!
//! Two layers live here, and handlers are expected to use the second one:
//!
//! - The `require_*` functions below are the single implementation of every
//!   repository access rule. They stay `pub(crate)` only so the extractors can
//!   call them; a handler that calls one directly is caught by the source guard
//!   in `tests/integration/authz_extractor_guard.rs`.
//! - The extractors ([`RepoRead`], [`RepoAuthRead`], [`RepoWrite`],
//!   [`RepoAdmin`], [`CiRead`]) put that rule in the handler's *signature*.
//!   A handler that forgets its gate no longer compiles into a working route —
//!   it simply has no repository to work with.
//!
//! The non-REST transports — git LFS, the OCI registry, the job-log WebSocket —
//! cannot use either layer: each carries its credentials in a shape of its own
//! (an LFS action signature, an OCI-scoped bearer token, a `?token=` query
//! parameter), so they resolve *who is calling* themselves. What they must not
//! also decide is *who is allowed*: [`check_read_for`] / [`check_write_for`]
//! take the actor the transport already resolved and answer that question here,
//! so there is still exactly one implementation of the rule.
//!
//! ```ignore
//! pub async fn delete_page(
//!     State(state): State<AppState>,
//!     RepoWrite { repo, actor_id }: RepoWrite,
//! ) -> impl IntoResponse { ... }
//! ```

use std::collections::HashMap;
use std::marker::PhantomData;

use axum::extract::{FromRequestParts, Path};
use axum::http::request::Parts;
use axum::http::HeaderMap;

use crate::error::AppError;
use crate::AppState;

/// Resolve a repository from its route owner/name pair.
pub(crate) async fn resolve_repo(
    state: &AppState,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name)
        .await
        // A DB outage here must classify as 503 (retryable), not 500: route the
        // anyhow error through `AppError::from` (which downcasts to `DbErr` and
        // maps connection-level failures to `ServiceUnavailable`) rather than
        // the blanket `AppError::internal`. This is the first DB call of nearly
        // every repository route, so a 500 here would mask the outage.
        .map_err(AppError::from)?
        .ok_or_else(|| AppError::not_found("repository not found"))
}

/// Decide read access for an already-resolved repository.
///
/// Public repos stay anonymously readable; a private repo answers `401` to an
/// anonymous caller (a token would help) and `403` to an authenticated
/// outsider (a token would not). A failed check is neither — it propagates as
/// the underlying error instead of being collapsed into a denial.
pub(crate) async fn check_read(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
) -> Result<(), AppError> {
    check_read_inner(state, headers, repo, None).await
}

/// Same as [`check_read`], but a CI job token carrying `ci_scope` for this very
/// repository also passes the gate — used by the endpoints a running job talks
/// to with its job token instead of a user session.
pub(crate) async fn check_read_with_ci(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
    ci_scope: &str,
) -> Result<(), AppError> {
    check_read_inner(state, headers, repo, Some(ci_scope)).await
}

async fn check_read_inner(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
    ci_scope: Option<&str>,
) -> Result<(), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret);

    match check_read_for(state, repo, actor_id).await {
        Ok(()) => Ok(()),
        // A *denial* may still be overturned by a CI job token scoped to this
        // repository. A check that could not run may not: that is our failure,
        // and swallowing it here would answer 403 to a caller whose token was
        // never the problem.
        Err(denied)
            if is_access_denial(&denied)
                && actor_id.is_none()
                && ci_job_grants(state, headers, repo, ci_scope) =>
        {
            Ok(())
        }
        Err(error) => Err(error),
    }
}

/// Decide read access for an already-resolved repository and an actor the
/// caller has already identified.
///
/// The header-reading [`check_read`] is this function plus "who is calling".
/// Transports that answer that question differently (LFS, OCI, the job-log
/// WebSocket) come in here, so the *rule* stays single even where the
/// credential does not.
pub(crate) async fn check_read_for(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<(), AppError> {
    match rg_core::repo::service::can_read_repo(&state.db, repo, actor_id).await {
        Ok(true) => Ok(()),
        Ok(false) if repo.is_private && actor_id.is_none() => {
            Err(AppError::unauthorized("authentication required"))
        }
        Ok(false) => Err(AppError::forbidden("access denied")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// [`check_read_for`] for write access: an anonymous caller is `401` whatever
/// the repository's visibility, because no repository is anonymously writable.
pub(crate) async fn check_write_for(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<(), AppError> {
    let Some(actor_id) = actor_id else {
        return Err(AppError::unauthorized("authentication required"));
    };

    match rg_core::repo::service::can_write_repo(&state.db, repo, Some(actor_id)).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::forbidden("write access denied")),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Whether the gate said "no", as opposed to the gate failing to run.
///
/// The distinction is the whole point of [`check_read_for`] returning an error
/// rather than a `bool`: a caller that folds a decision back into a boolean
/// (the OCI registry mints a scope from one) must fold the *denial* only and
/// let a failed check stay a failure — see `oci::granted`.
pub(crate) fn is_access_denial(error: &AppError) -> bool {
    matches!(error, AppError::Unauthorized(_) | AppError::Forbidden(_))
}

/// Whether the request carries a CI job token scoped to this repository.
fn ci_job_grants(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
    ci_scope: Option<&str>,
) -> bool {
    match ci_scope {
        Some(scope) => {
            super::auth::extract_ci_job_claims(headers, &state.jwt_secret, repo.id, scope).is_some()
        }
        None => false,
    }
}

/// Require repository read access, while retaining anonymous access to public repos.
pub(crate) async fn require_read(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    let repo = resolve_repo(state, owner, name).await?;
    check_read(state, headers, &repo).await?;
    Ok(repo)
}

/// [`require_read`] with the CI job-token fallback of [`check_read_with_ci`].
pub(crate) async fn require_read_with_ci(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
    ci_scope: &str,
) -> Result<rg_db::entities::repository::Model, AppError> {
    let repo = resolve_repo(state, owner, name).await?;
    check_read_with_ci(state, headers, &repo, ci_scope).await?;
    Ok(repo)
}

/// Require an authenticated user who can read the repository.
pub(crate) async fn require_authenticated_read(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;

    check_read_for(state, &repo, Some(actor_id)).await?;
    Ok((repo, actor_id))
}

/// Require an authenticated user with repository write access.
pub(crate) async fn require_write(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;

    check_write_for(state, &repo, Some(actor_id)).await?;
    Ok((repo, actor_id))
}

/// Require an authenticated repository administrator.
pub(crate) async fn require_admin(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;
    match rg_core::repo::service::can_admin_repo(&state.db, &repo, Some(actor_id)).await {
        Ok(true) => Ok((repo, actor_id)),
        Ok(false) => Err(AppError::forbidden("repository admin access required")),
        Err(error) => Err(AppError::from(error)),
    }
}

// ---------------------------------------------------------------------------
// Typed access extractors
// ---------------------------------------------------------------------------
//
// Every extractor below delegates to the `require_*` function above it. That is
// deliberate: there is exactly one implementation of each access rule, and the
// extractor only decides *where* the rule is stated. Re-implementing the check
// inside the extractor is how the copies this phase exists to remove got made
// in the first place.

/// Pull the `owner` / repository-name pair a repository-scoped route carries.
///
/// Read through `Path<HashMap<_, _>>` rather than a positional
/// `Path<(String, String)>`, because the same gate has to work on
/// `/repos/{owner}/{name}` and on
/// `/repos/{owner}/{name}/issues/{number}/comments/{comment_id}` alike — a
/// tuple would demand the exact arity of each individual route.
///
/// Extracting `Path` here does not consume it: axum reads the captures out of
/// the request extensions, so the handler can still take its own `Path<...>`
/// for the segments it actually needs.
async fn route_repo(parts: &mut Parts, state: &AppState) -> Result<(String, String), AppError> {
    let Path(params) = Path::<HashMap<String, String>>::from_request_parts(parts, state)
        .await
        .map_err(|_| AppError::internal("route carries no path parameters to authorize against"))?;

    let owner = params.get("owner");
    // REST names the second segment `{name}`; git-over-HTTP and the OCI
    // registry name it `{repo}`. Accept both so a route cannot silently fall
    // through the gate over a naming choice.
    let name = params.get("name").or_else(|| params.get("repo"));

    match (owner, name) {
        (Some(owner), Some(name)) => Ok((owner.clone(), name.clone())),
        _ => Err(AppError::internal(
            "route is not repository-scoped: no {owner}/{name} captures",
        )),
    }
}

/// Read access to the repository, anonymous callers included — a public
/// repository stays readable without a token, a private one does not.
///
/// Mirrors [`require_read`].
pub struct RepoRead {
    pub repo: rg_db::entities::repository::Model,
}

impl FromRequestParts<AppState> for RepoRead {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let (owner, name) = route_repo(parts, state).await?;
        let repo = require_read(state, &parts.headers, &owner, &name).await?;
        Ok(Self { repo })
    }
}

/// The CI job-token scope a read gate accepts as a second key.
///
/// A running job talks to a handful of endpoints with its job token instead of
/// a user session; the scope names which family it is allowed to reach.
pub trait CiScope {
    const SCOPE: &'static str;
}

/// `repo:read` — repository contents (tree, blobs, log, branches, tags).
pub struct RepoContents;
impl CiScope for RepoContents {
    const SCOPE: &'static str = "repo:read";
}

/// `packages:read` — the package registry surfaces.
pub struct Packages;
impl CiScope for Packages {
    const SCOPE: &'static str = "packages:read";
}

/// [`RepoRead`] that also accepts a CI job token scoped to this repository.
///
/// Mirrors [`require_read_with_ci`]. Generic over the scope rather than
/// duplicated per scope, so the two variants cannot drift apart.
pub struct CiRead<S: CiScope> {
    pub repo: rg_db::entities::repository::Model,
    _scope: PhantomData<S>,
}

impl<S: CiScope> FromRequestParts<AppState> for CiRead<S> {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let (owner, name) = route_repo(parts, state).await?;
        let repo = require_read_with_ci(state, &parts.headers, &owner, &name, S::SCOPE).await?;
        Ok(Self {
            repo,
            _scope: PhantomData,
        })
    }
}

/// An authenticated caller who can read the repository.
///
/// Mirrors [`require_authenticated_read`], including its order: a missing token
/// is a `401` *before* the repository is looked up, so the gate does not double
/// as an existence oracle for private repositories.
pub struct RepoAuthRead {
    pub repo: rg_db::entities::repository::Model,
    pub actor_id: i64,
}

impl FromRequestParts<AppState> for RepoAuthRead {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let (owner, name) = route_repo(parts, state).await?;
        let (repo, actor_id) =
            require_authenticated_read(state, &parts.headers, &owner, &name).await?;
        Ok(Self { repo, actor_id })
    }
}

/// An authenticated caller with repository write access.
///
/// Mirrors [`require_write`].
pub struct RepoWrite {
    pub repo: rg_db::entities::repository::Model,
    pub actor_id: i64,
}

impl FromRequestParts<AppState> for RepoWrite {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let (owner, name) = route_repo(parts, state).await?;
        let (repo, actor_id) = require_write(state, &parts.headers, &owner, &name).await?;
        Ok(Self { repo, actor_id })
    }
}

/// An authenticated repository administrator.
///
/// Mirrors [`require_admin`].
pub struct RepoAdmin {
    pub repo: rg_db::entities::repository::Model,
    pub actor_id: i64,
}

impl FromRequestParts<AppState> for RepoAdmin {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let (owner, name) = route_repo(parts, state).await?;
        let (repo, actor_id) = require_admin(state, &parts.headers, &owner, &name).await?;
        Ok(Self { repo, actor_id })
    }
}
