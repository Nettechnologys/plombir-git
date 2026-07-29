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
//! A route whose path names no repository at all — `/artifacts/{id}` and its
//! kind — reaches one through [`AnchoredRead`] / [`AnchoredWrite`]: the domain
//! module says how its id walks to a repository ([`RepoAnchor`]), and the
//! decision still happens here. That is the third shape, not a third rule.
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
use std::future::Future;
use std::marker::PhantomData;

use axum::extract::{FromRequest, FromRequestParts, Path, Request};
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
        Err(denied) if is_access_denial(&denied) && actor_id.is_none() => {
            if ci_job_grants(state, headers, repo, ci_scope).await? {
                Ok(())
            } else {
                Err(denied)
            }
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

/// [`check_read_for`] for administrative access.
pub(crate) async fn check_admin_for(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<(), AppError> {
    let Some(actor_id) = actor_id else {
        return Err(AppError::unauthorized("authentication required"));
    };

    match rg_core::repo::service::can_admin_repo(&state.db, repo, Some(actor_id)).await {
        Ok(true) => Ok(()),
        Ok(false) => Err(AppError::forbidden("repository admin access required")),
        Err(error) => Err(AppError::from(error)),
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

/// Fold a gate decision into a boolean — the denial only.
///
/// A check that could not *run* stays an error, because the alternative is the
/// bug this module keeps finding: `unwrap_or(false)` turns a database outage
/// into "access denied" and sends the caller off to re-issue a token that was
/// never the problem.
fn decided(outcome: Result<(), AppError>) -> Result<bool, AppError> {
    match outcome {
        Ok(()) => Ok(true),
        Err(error) if is_access_denial(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

// ---------------------------------------------------------------------------
// Predicate form: access as a *question*, not as a gate
// ---------------------------------------------------------------------------
//
// The extractors answer "may this request proceed at all". A handler sometimes
// needs the weaker question — "is this caller also a writer?" — on top of a
// gate it has already passed: an issue may be filed by any reader but only a
// writer may set its labels; a comment may be edited by its author *or* by a
// writer. Those handlers used to reach past this module into
// `rg_core::repo::service::can_*_repo` and phrase the rule themselves, which is
// the same gate in a second dialect: the source guard in
// `tests/integration/authz_extractor_guard.rs` cannot see a decision written
// from scratch, and the two dialects drift.
//
// So the question is answered here too. `may_*` is the same rule as
// `check_*_for` — literally, it is that function with the denial folded into
// `false` — and a handler asking it is still asking the layer.

/// May this actor read the repository? See the module note above on when the
/// predicate form is the right one.
pub(crate) async fn may_read(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<bool, AppError> {
    decided(check_read_for(state, repo, actor_id).await)
}

/// May this actor write to the repository?
pub(crate) async fn may_write(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<bool, AppError> {
    decided(check_write_for(state, repo, actor_id).await)
}

/// May this actor administer the repository?
pub(crate) async fn may_admin(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: Option<i64>,
) -> Result<bool, AppError> {
    decided(check_admin_for(state, repo, actor_id).await)
}

/// Whether the request carries a CI job token scoped to this repository *and
/// still belonging to a running job*.
///
/// The signature half of that is free; the second half costs one primary-key
/// read and is the whole point. A job token is minted for an hour, and its
/// signature keeps verifying for that hour no matter what happened to the
/// pipeline — a cancelled job, or a token that leaked into the job's own log,
/// went on opening a private repository until the clock ran out. The OIDC
/// exchange has always re-read the row; this is the same check, from the other
/// consumer, through the same helper so the two cannot drift.
///
/// A failed lookup is not a denial: it propagates, so a database outage cannot
/// silently downgrade a valid job token into "no CI token here".
async fn ci_job_grants(
    state: &AppState,
    headers: &HeaderMap,
    repo: &rg_db::entities::repository::Model,
    ci_scope: Option<&str>,
) -> Result<bool, AppError> {
    let Some(scope) = ci_scope else {
        return Ok(false);
    };
    let Some(claims) =
        super::auth::extract_ci_job_claims(headers, &state.jwt_secret, repo.id, scope)
    else {
        return Ok(false);
    };
    match super::auth::ci_job_binding(state, &claims).await {
        Ok(_) => Ok(true),
        Err(error) if is_access_denial(&error) => Ok(false),
        Err(error) => Err(error),
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
    check_admin_for(state, &repo, Some(actor_id)).await?;
    Ok((repo, actor_id))
}

/// Require the repository's owner — not merely someone with write or admin
/// rights on it.
///
/// Deleting a repository, and transferring it to someone else, are the two
/// operations where "may change what is inside" is not enough: they dispose of
/// the thing itself. `Access::RepoOwner` has named that level in the route
/// table all along while the handlers compared `repo.owner_id` in their own
/// bodies — a rule stated in prose next to the route and re-derived in code
/// inside it, which is exactly the split this module exists to close.
///
/// Ownership is compared against `repo.owner_id` directly rather than through
/// `can_admin_repo`: an organization admin administers the repository, and a
/// collaborator may write to it, but neither of them owns it.
pub(crate) async fn require_owner(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;

    if repo.owner_id != actor_id {
        return Err(AppError::forbidden("repository owner access required"));
    }
    Ok((repo, actor_id))
}

// ---------------------------------------------------------------------------
// Namespace gate: the target is named by the request *body*, not by the route
// ---------------------------------------------------------------------------
//
// Every rule above starts from a repository the route already named, which is
// why a path-based extractor can carry it. A handful of routes do not work that
// way: they take the `owner/name` they are about to write into out of the JSON
// body, and the repository may not even exist yet. `POST /imports` is the one
// that made this visible — `target_owner` arrived from the body and was passed
// straight to the import service, which happily filled *someone else's*
// namespace with issues, releases and branches, or created a repository under
// their account.
//
// The rule such a route needs is not "may I write to this repo" alone: it is
// "may I write into this namespace at all", and that has two halves — the
// repository exists (then it is the ordinary write gate) or it does not (then
// it is the right to create under that owner, the same rule `create_repo`
// applies to its `org` field).

/// A request body that names the account or organization it is aimed at.
///
/// Split out of [`TargetNamespace`] because the two body-named routes need
/// different halves of the target. An import names a whole `owner/name` in its
/// payload; a transfer names only the *namespace* — the repository's name comes
/// from the route and does not change — and asking it for a name it does not
/// carry would only invite the handler to invent one.
pub trait TargetOwner {
    /// The account or organization the write is aimed at.
    fn target_owner(&self) -> &str;
}

/// A request body whose target namespace may be *absent*, meaning the caller's
/// own account.
///
/// `POST /repos` is why this half-open shape exists: its `org` field is
/// optional, and an omitted one is not a missing target — it *is* the target,
/// spelled "under me". A body forced to produce a `&str` would have the handler
/// look up the caller's own username to satisfy the trait, which is a second
/// name resolution in the one place this module exists to keep singular.
pub trait TargetOwnerOrSelf {
    /// The account or organization the write is aimed at, or `None` for the
    /// caller's own account.
    fn target_owner_or_self(&self) -> Option<&str>;
}

/// A body that always names its target speaks both dialects, so the create gate
/// has a single bound and the two shapes cannot grow two rules.
impl<T: TargetOwner> TargetOwnerOrSelf for T {
    fn target_owner_or_self(&self) -> Option<&str> {
        Some(self.target_owner())
    }
}

/// A request body that names the `owner/name` it wants to write into.
///
/// Implemented next to the request type it belongs to, so the derivation of the
/// target name (an import may leave it out and have it read off the source URL)
/// stays with the handler that owns the payload.
pub trait TargetNamespace: TargetOwner {
    /// The repository name inside that namespace, already defaulted.
    fn target_name(&self) -> String;
}

/// Require the right to write into `owner/name` when that pair comes from the
/// request body rather than from the route.
///
/// An existing repository is decided by the ordinary write gate; a name that is
/// still free is decided by [`require_namespace_create`]. Both answers come
/// from this module, so a body-named target is not a second dialect of the
/// rule.
pub(crate) async fn require_namespace_write(
    state: &AppState,
    actor_id: i64,
    owner: &str,
    name: &str,
) -> Result<(), AppError> {
    let existing = rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, name)
        .await
        .map_err(AppError::from)?;

    match existing {
        Some(repo) => check_write_for(state, &repo, Some(actor_id)).await,
        None => require_namespace_create(state, actor_id, Some(owner))
            .await
            .map(|_| ()),
    }
}

/// Require the right to create a *new* repository under `owner/`.
///
/// Two namespaces qualify: the caller's own account, and an organization the
/// caller belongs to — the rule `create_repo` applies to its `org` field.
/// Returns the organization id when the namespace is one, so a caller that has
/// to record it does not resolve the name a second time.
///
/// `None` *is* the caller's own account: a body that leaves its namespace out
/// (`POST /repos` without an `org`) names the one namespace authentication has
/// already settled, so there is no name to resolve and nothing left to decide.
/// It is spelled as an absent owner rather than as the caller's username so the
/// handler never has to produce that username to ask the question.
///
/// The owner name is resolved exactly the way the write path resolves it
/// (username first, then organization), so the gate cannot end up looser than
/// the thing it guards. An owner that is neither is a denial rather than a
/// `404`: a caller with no right to that namespace learns nothing about
/// whether the account exists.
pub(crate) async fn require_namespace_create(
    state: &AppState,
    actor_id: i64,
    owner: Option<&str>,
) -> Result<Option<i64>, AppError> {
    let Some(owner) = owner else {
        return Ok(None);
    };

    if let Some(user) = rg_db::ops::user_ops::find_by_username(&state.db, owner)
        .await
        .map_err(AppError::from)?
    {
        return if user.id == actor_id {
            Ok(None)
        } else {
            Err(AppError::forbidden(
                "you may not create a repository under this owner",
            ))
        };
    }

    if let Some(org) = rg_db::ops::org_ops::get_org_by_name(&state.db, owner)
        .await
        .map_err(AppError::from)?
    {
        return match rg_db::ops::org_ops::is_org_member(&state.db, org.id, actor_id)
            .await
            .map_err(AppError::from)?
        {
            true => Ok(Some(org.id)),
            false => Err(AppError::forbidden(
                "you are not a member of this organization",
            )),
        };
    }

    Err(AppError::forbidden(
        "you may not create a repository under this owner",
    ))
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

/// The repository's owner.
///
/// Mirrors [`require_owner`]. Stronger than [`RepoAdmin`]: an organization
/// admin administers the repository without owning it.
pub struct RepoOwner {
    pub repo: rg_db::entities::repository::Model,
    pub actor_id: i64,
}

impl FromRequestParts<AppState> for RepoOwner {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let (owner, name) = route_repo(parts, state).await?;
        let (repo, actor_id) = require_owner(state, &parts.headers, &owner, &name).await?;
        Ok(Self { repo, actor_id })
    }
}

/// An authenticated caller who may write into the namespace their *body* names.
///
/// Mirrors [`require_namespace_write`]. This is the one extractor that reads the
/// request body, because that is where the target lives: it deserializes the
/// payload, asks the gate about the `owner/name` the payload names, and hands
/// the handler both the caller and the already-parsed body. A handler that
/// takes it cannot forget the check, and a handler that forgets to take it has
/// no body to work with.
///
/// Being a body extractor, it must be the *last* argument of the handler.
pub struct NamespaceWrite<B> {
    pub actor_id: i64,
    pub body: B,
}

impl<B> FromRequest<AppState> for NamespaceWrite<B>
where
    B: TargetNamespace + serde::de::DeserializeOwned + Send + 'static,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        // Authentication first, body second: an anonymous caller is turned away
        // by `401` without the server parsing anything it was never going to
        // act on — and without the shape of the payload deciding which of the
        // two answers they get.
        let actor_id = super::auth::extract_user_id(req.headers(), &state.jwt_secret)
            .ok_or_else(|| AppError::unauthorized("authentication required"))?;

        let axum::Json(body) = axum::Json::<B>::from_request(req, state)
            .await
            .map_err(|rejection| AppError::bad_request(rejection.body_text()))?;

        require_namespace_write(state, actor_id, body.target_owner(), &body.target_name()).await?;

        Ok(Self { actor_id, body })
    }
}

/// An authenticated caller who may place a *new* repository into the namespace
/// their *body* names.
///
/// Mirrors [`require_namespace_create`], and is [`NamespaceWrite`]'s sibling for
/// the routes whose target cannot already exist. `POST /repos/{owner}/{name}/
/// transfer` is the one that made it necessary: `RepoOwner` gates the *source*
/// — the route names it — while the destination arrives as `new_owner` in the
/// payload, where no path extractor reaches it. Nothing asked whether the
/// caller had any business in that namespace, so a repository could be pushed
/// under a stranger's account, complete with its contents, and look like
/// theirs.
///
/// The rule is the create rule rather than the write rule on purpose: a
/// transfer *adds* a repository to the destination (the service refuses a name
/// already taken there), and "may add a repository under this owner" is exactly
/// what `create_repo` asks about its own `org` field — which is why
/// `POST /repos` takes this extractor too rather than keeping the second copy
/// of the membership rule it used to carry in its body (card_1e1ed1ee06f1).
///
/// Being a body extractor, it must be the *last* argument of the handler.
pub struct NamespaceCreate<B> {
    pub actor_id: i64,
    /// The destination organization's id, when the namespace is one — already
    /// resolved by the gate, so a handler that has to store it does not look
    /// the name up a second time and cannot resolve it differently.
    pub org_id: Option<i64>,
    pub body: B,
}

impl<B> FromRequest<AppState> for NamespaceCreate<B>
where
    B: TargetOwnerOrSelf + serde::de::DeserializeOwned + Send + 'static,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        // Same order as `NamespaceWrite`: authenticate, then parse, then gate —
        // so an anonymous caller is `401` without the server parsing a payload
        // it was never going to act on.
        let actor_id = super::auth::extract_user_id(req.headers(), &state.jwt_secret)
            .ok_or_else(|| AppError::unauthorized("authentication required"))?;

        let axum::Json(body) = axum::Json::<B>::from_request(req, state)
            .await
            .map_err(|rejection| AppError::bad_request(rejection.body_text()))?;

        let org_id = require_namespace_create(state, actor_id, body.target_owner_or_self()).await?;

        Ok(Self {
            actor_id,
            org_id,
            body,
        })
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

// ---------------------------------------------------------------------------
// Anchored extractors: the repository is named by a *row*, not by the route
// ---------------------------------------------------------------------------
//
// Every extractor above rests on [`route_repo`], which needs `{owner}/{name}`
// in the path. A few routes do not have it: `/artifacts/{id}` addresses a row
// by its instance-wide primary key, and the repository that admits the caller
// is four tables away — artifact → job → stage → pipeline → repository. Those
// routes had no extractor to take, so each wrote a prologue of its own:
// resolve the row, walk to the repository, read the session out of the headers,
// decide. The walk really is theirs. The decision never was, and writing it by
// hand is how `require_artifact_write` came to re-derive "who is calling" from
// `extract_user_id` while the route table said `RepoWrite`.
//
// So the two halves are split. A [`RepoAnchor`] says how *this* id reaches its
// repository and lives next to the domain that knows how — `api::artifacts` for
// an artifact — while [`AnchoredRead`] / [`AnchoredWrite`] hand what it resolved
// to the same `check_*` the path-based extractors end in. The route gets its
// access level back into the handler's signature; the rule stays singular.

/// A route that names its repository indirectly — through the instance-wide id
/// of a row that lives inside it.
///
/// An implementation resolves, and decides nothing: it turns the id into the
/// row and the repository that owns it, and the extractor over it turns away a
/// caller with no right to that repository. A row that is missing (or expired,
/// or otherwise not to be served) is the implementation's to report, and it
/// reports `404` — the id is instance-wide, so "no such row" and "not in a
/// repository you may see" have to be the same answer.
pub trait RepoAnchor: Send + Sync + 'static {
    /// The row the id addresses. Handed to the handler beside the repository,
    /// so a gate that had to fetch it is not paid for twice.
    type Row: Send + 'static;

    /// The path parameter carrying the id.
    const PARAM: &'static str;

    /// Resolve the row and the repository that owns it.
    fn resolve(
        state: &AppState,
        id: i64,
    ) -> impl Future<Output = Result<(Self::Row, rg_db::entities::repository::Model), AppError>> + Send;
}

/// Pull the instance-wide id an anchored route carries.
///
/// Read through `Path<HashMap<_, _>>` for the same reason [`route_repo`] is:
/// the gate has to work whatever *else* the route captures, and a positional
/// tuple would demand the exact arity of each individual route.
async fn route_id(parts: &mut Parts, state: &AppState, param: &str) -> Result<i64, AppError> {
    let Path(params) = Path::<HashMap<String, String>>::from_request_parts(parts, state)
        .await
        .map_err(|_| AppError::internal("route carries no path parameters to authorize against"))?;

    let Some(raw) = params.get(param) else {
        return Err(AppError::internal(format!(
            "route is not anchored: no {{{param}}} capture to resolve a repository from"
        )));
    };
    raw.parse::<i64>()
        .map_err(|_| AppError::bad_request(format!("{param} must be a number")))
}

/// Read access to the repository an anchored row belongs to, anonymous callers
/// included — the row of a public repository stays readable without a token,
/// the row of a private one does not.
///
/// Mirrors [`RepoRead`] for the routes whose path names no repository.
pub struct AnchoredRead<A: RepoAnchor> {
    pub row: A::Row,
    pub repo: rg_db::entities::repository::Model,
}

impl<A: RepoAnchor> FromRequestParts<AppState> for AnchoredRead<A> {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let id = route_id(parts, state, A::PARAM).await?;
        let (row, repo) = A::resolve(state, id).await?;
        check_read(state, &parts.headers, &repo).await?;
        Ok(Self { row, repo })
    }
}

/// An authenticated caller with write access to the repository an anchored row
/// belongs to.
///
/// Mirrors [`RepoWrite`], and keeps [`RepoAuthRead`]'s order: a missing token is
/// a `401` *before* anything is looked up, so the gate does not double as an
/// existence oracle for the rows of private repositories.
pub struct AnchoredWrite<A: RepoAnchor> {
    pub row: A::Row,
    pub repo: rg_db::entities::repository::Model,
    pub actor_id: i64,
}

impl<A: RepoAnchor> FromRequestParts<AppState> for AnchoredWrite<A> {
    type Rejection = AppError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let actor_id = super::auth::extract_user_id(&parts.headers, &state.jwt_secret)
            .ok_or_else(|| AppError::unauthorized("authentication required"))?;
        let id = route_id(parts, state, A::PARAM).await?;
        let (row, repo) = A::resolve(state, id).await?;
        check_write_for(state, &repo, Some(actor_id)).await?;
        Ok(Self {
            row,
            repo,
            actor_id,
        })
    }
}
