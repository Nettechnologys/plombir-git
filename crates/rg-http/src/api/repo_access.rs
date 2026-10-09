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

/// The deploy-key half of [`check_read_for`] / [`check_write_for`].
///
/// A deploy key is not an account, so the account gates cannot answer for it:
/// the key opens the one repository it was added to, and writes only when it was
/// not added read-only. It reaches this crate solely through Git LFS — a URL
/// issued against `git-lfs-authenticate` on the SSH port — and the row is
/// re-read here so that deleting the key, or narrowing it to read-only, takes
/// effect on the next request rather than when the URL runs out.
pub(crate) async fn check_deploy_key_for(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    key_id: i64,
    write: bool,
) -> Result<(), AppError> {
    match rg_db::ops::deploy_key_ops::find_by_id(&state.db, key_id).await {
        Ok(Some(key)) if rg_core::repo::service::deploy_key_permits(&key, repo.id, write) => Ok(()),
        Ok(Some(_)) => Err(AppError::forbidden(if write {
            "write access denied"
        } else {
            "access denied"
        })),
        Ok(None) => Err(AppError::unauthorized("the deploy key has been removed")),
        Err(error) => {
            tracing::error!(
                key_id,
                repo_id = repo.id,
                error = %format!("{error:#}"),
                "could not read the deploy key behind an LFS request"
            );
            Err(AppError::service_unavailable(
                "could not verify the deploy key",
            ))
        }
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

/// Whether `actor_id` administers `repo` — the second half of a row-level
/// rule whose first half the domain decides ("is it yours").
///
/// For routes where an authenticated reader may act on rows they authored and
/// an administrator on everybody's: editing or deleting a comment
/// (card_60961272e1ba). The rule stays [`check_admin_for`]'s; a refusal is
/// `false`, a failure of the check itself stays an error.
pub(crate) async fn administers(
    state: &AppState,
    repo: &rg_db::entities::repository::Model,
    actor_id: i64,
) -> Result<bool, AppError> {
    match check_admin_for(state, repo, Some(actor_id)).await {
        Ok(()) => Ok(true),
        Err(AppError::Forbidden(_) | AppError::Unauthorized(_) | AppError::NotFound(_)) => {
            Ok(false)
        }
        Err(other) => Err(other),
    }
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
/// Ownership is `rg_core::repo::service::can_own_repo`, not `can_admin_repo`
/// and not a comparison against `repo.owner_id`: an organization admin
/// administers the repository, and a collaborator may write to it, but neither
/// of them owns it — and on an organization repository `owner_id` is not a
/// grant at all. It names the organization's owner at creation, who kept this
/// level after being removed from the organization while the members actually
/// holding the `owner` role were refused (security audit #5). The rule is the
/// membership role, decided in the predicate like every other.
pub(crate) async fn require_owner(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    name: &str,
) -> Result<(rg_db::entities::repository::Model, i64), AppError> {
    let actor_id = super::auth::extract_user_id(headers, &state.jwt_secret)
        .ok_or_else(|| AppError::unauthorized("authentication required"))?;
    let repo = resolve_repo(state, owner, name).await?;

    match rg_core::repo::service::can_own_repo(&state.db, &repo, Some(actor_id)).await {
        Ok(true) => Ok((repo, actor_id)),
        Ok(false) => Err(AppError::forbidden("repository owner access required")),
        // A failed membership lookup is ours, not a refusal.
        Err(e) => Err(AppError::from(e)),
    }
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

    /// Produce the payload represented by a genuinely empty request body.
    ///
    /// Most namespace-creating routes require a JSON body and keep the default
    /// `None`. A route with an established no-body form may opt in without
    /// wrapping this extractor in `Option`: that wrapper would also swallow the
    /// extractor's authentication and authorization rejection, which is exactly
    /// the rule this type exists to keep mandatory.
    fn from_empty_body() -> Option<Self>
    where
        Self: Sized,
    {
        None
    }
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
/// Returns the canonical namespace name and the organization id when the
/// namespace is one, so a caller that has to record either does not resolve the
/// request spelling a second time.
///
/// `None` *is* the caller's own account: a body that leaves its namespace out
/// (`POST /repos` without an `org`) names the one namespace authentication has
/// already settled, so there is no name to resolve and nothing left to decide.
/// It is spelled as an absent owner rather than as the caller's username so the
/// handler never has to produce that username to ask the question.
///
/// The owner name is resolved exactly the way the write path resolves it
/// (username first, then organization), so the gate cannot end up looser than
/// the thing it guards.
///
/// Every refusal is the same `403` with the same body, and that sameness is the
/// whole of what the gate hides: *which* of the three reasons applied — a
/// stranger's account, an organization the caller is not in, or a name nobody
/// has taken. It does not hide whether the name is in use, and no gate on this
/// route could: the namespace is global, so `POST /orgs` answers
/// `organization name 'x' is already taken` to any account that can log in.
/// This comment used to promise that "a caller with no right to that namespace
/// learns nothing about whether the account exists" while the code below split
/// the denial into two texts — false twice over, by the neighbouring route and
/// by this function's own body (card_2179245d41db). The one text is kept
/// because a reason-by-reason denial is still worth not handing out for free,
/// not because it makes the name a secret.
pub(crate) struct NamespaceCreateGrant {
    org_id: Option<i64>,
    /// Present when the body explicitly names a namespace. An omitted owner is
    /// the caller's own account and stays `None`; the service can read its
    /// canonical username from `actor_id` without a second name resolution.
    namespace_name: Option<String>,
}

pub(crate) async fn require_namespace_create(
    state: &AppState,
    actor_id: i64,
    owner: Option<&str>,
) -> Result<NamespaceCreateGrant, AppError> {
    let Some(owner) = owner else {
        return Ok(NamespaceCreateGrant {
            org_id: None,
            namespace_name: None,
        });
    };

    // An account claimed for retirement is not a namespace anything may still
    // enter either, so it denies with the same one text as the other three
    // reasons. As with the organization below, the authoritative refusal is the
    // one `create_repo` makes against the same marker after its row commits;
    // this only saves the caller a Git init it would lose anyway.
    if let Some(user) = rg_db::ops::user_ops::find_active_by_username(&state.db, owner)
        .await
        .map_err(AppError::from)?
    {
        return if user.id == actor_id {
            Ok(NamespaceCreateGrant {
                org_id: None,
                namespace_name: Some(user.username),
            })
        } else {
            Err(AppError::forbidden(
                "you may not create a repository under this owner",
            ))
        };
    }

    // An organization claimed for retirement is not a namespace anything may
    // still enter, so it denies exactly like a name nobody has taken — the same
    // one text, keeping the gate free of a retiring/absent oracle. The
    // authoritative refusal is the one `create_repo` makes against the same
    // marker; this only saves the caller a Git init it would lose anyway.
    if let Some(org) = rg_db::ops::org_ops::find_active_org_by_name(&state.db, owner)
        .await
        .map_err(AppError::from)?
    {
        return match rg_db::ops::org_ops::is_org_member(&state.db, org.id, actor_id)
            .await
            .map_err(AppError::from)?
        {
            true => Ok(NamespaceCreateGrant {
                org_id: Some(org.id),
                namespace_name: Some(org.name),
            }),
            // Same text as the other two denials on purpose: the status was
            // already `403` for all three, so a second wording bought the
            // caller a membership/existence split under a single code — an
            // oracle one level below where the route sweep looks.
            false => Err(AppError::forbidden(
                "you may not create a repository under this owner",
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
/// A token confined to repositories is refused here outright, whatever the
/// namespace. Its allow-list names repositories that already exist, so it can
/// never list the destination a fork or a transfer adds a repository to. The
/// route layer cannot see that half: `POST /repos` is not about a repository
/// and is refused there, but `POST /repos/{owner}/{name}/fork` and `/transfer`
/// name their *source*, and the layer admits them by it (card_06b1b53d1df0).
///
/// Being a body extractor, it must be the *last* argument of the handler.
pub struct NamespaceCreate<B> {
    pub actor_id: i64,
    /// The destination organization's id, when the namespace is one — already
    /// resolved by the gate, so a handler that has to store it does not look
    /// the name up a second time and cannot resolve it differently.
    pub org_id: Option<i64>,
    /// Canonical spelling of an explicitly named destination. This comes from
    /// the row the gate authorized, not the request bytes; it keeps storage and
    /// audit paths aligned even on a case-insensitive database backend.
    pub namespace_name: Option<String>,
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
        let confined = req
            .extensions()
            .get::<crate::agent_scope::TokenGrant>()
            .filter(|grant| grant.is_repo_restricted())
            .cloned()
            .map(|grant| {
                let refused = (req.method().to_string(), req.uri().path().to_string());
                (grant, refused, req.headers().clone())
            });

        let body = if let Some(empty_body) = B::from_empty_body() {
            // `POST /fork` predates its optional destination payload. Buffering
            // lets that one request type distinguish a truly empty body from a
            // non-empty body that merely omitted `Content-Type`: the latter
            // must still be rejected rather than silently treated as "under
            // me". `Bytes` applies Axum's normal body limit before the request
            // is reconstructed for the ordinary JSON extractor.
            let headers = req.headers().clone();
            let bytes = axum::body::Bytes::from_request(req, state)
                .await
                .map_err(|rejection| AppError::bad_request(rejection.body_text()))?;
            if bytes.is_empty() {
                empty_body
            } else {
                let mut req = Request::new(axum::body::Body::from(bytes));
                *req.headers_mut() = headers;
                let axum::Json(body) = axum::Json::<B>::from_request(req, state)
                    .await
                    .map_err(|rejection| AppError::bad_request(rejection.body_text()))?;
                body
            }
        } else {
            let axum::Json(body) = axum::Json::<B>::from_request(req, state)
                .await
                .map_err(|rejection| AppError::bad_request(rejection.body_text()))?;
            body
        };

        // After the body, so the refusal can record where the repository was
        // headed; before the namespace lookup, so it answers the same for a
        // namespace that exists and one that does not.
        if let Some((grant, (method, path), headers)) = confined {
            return Err(grant
                .deny(
                    &headers,
                    "this token is confined to specific repositories and may not add a repository to a namespace",
                    serde_json::json!({
                        "reason": "repository_creation_not_allowed",
                        "method": method,
                        "path": path,
                        "namespace": body.target_owner_or_self(),
                    }),
                )
                .await);
        }

        let NamespaceCreateGrant {
            org_id,
            namespace_name,
        } = require_namespace_create(state, actor_id, body.target_owner_or_self()).await?;

        Ok(Self {
            actor_id,
            org_id,
            namespace_name,
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
/// caller with no right to that repository. A row that is missing is the
/// implementation's to report, and it reports [`RepoAnchor::masked`] — the id is
/// instance-wide, so "no such row" and "not in a repository you may see" have to
/// be the same answer.
///
/// Both halves of that sentence are load-bearing, and only the first one used to
/// be implemented. `resolve` answered `404` and the gate behind it answered
/// `403`, so any account that could log in could walk `{id}` and read off which
/// rows the instance holds — the enumeration oracle of card_1419723e0606. The
/// answer is one value now, [`masked`](RepoAnchor::masked), returned by the
/// implementation for an absent row and by the extractors for a refused one, so
/// the two cannot be told apart and cannot drift into two texts.
pub trait RepoAnchor: Send + Sync + 'static {
    /// The row the id addresses. Handed to the handler beside the repository,
    /// so a gate that had to fetch it is not paid for twice.
    type Row: Send + 'static;

    /// The path parameter carrying the id.
    const PARAM: &'static str;

    /// The one answer for an id the caller may not know the fate of: absent,
    /// or naming a row in a repository they cannot see.
    ///
    /// A `404` in the noun of the domain. [`resolve`](RepoAnchor::resolve) must
    /// return *this* for a missing row rather than build its own, because the
    /// masking is only worth what the two answers have in common: an outsider
    /// who gets `artifact not found` for one id and `not found` for another has
    /// still learned which is which.
    fn masked() -> AppError;

    /// Resolve the row and the repository that owns it.
    ///
    /// Reports a missing row as [`masked`](RepoAnchor::masked) and decides
    /// nothing else. In particular it does not judge whether a row it *did*
    /// find is still to be served — that is [`admit`](RepoAnchor::admit), which
    /// runs after the gate.
    fn resolve(
        state: &AppState,
        id: i64,
    ) -> impl Future<Output = Result<(Self::Row, rg_db::entities::repository::Model), AppError>> + Send;

    /// Whether a row the caller is allowed to see is nonetheless not to be
    /// served — an artifact past its retention date, for the one anchor that
    /// has such a state.
    ///
    /// Separate from [`resolve`](RepoAnchor::resolve), and after the gate, for
    /// the reason the gate is masked at all: `404 artifact expired` and
    /// `404 artifact not found` are two answers, and a caller who can tell them
    /// apart can still enumerate the rows that once existed. Past the gate the
    /// distinction costs nothing — the caller can read the repository, so being
    /// told *why* the row is gone reveals nothing they could not already see —
    /// and is worth keeping, because "expired" is the answer an owner needs.
    fn admit(_row: &Self::Row) -> Result<(), AppError> {
        Ok(())
    }
}

/// Reduce a refusal on an anchored route to the answer an absent id gets.
///
/// Only a *denial* is masked. A check that could not run stays what it was, for
/// the reason [`decided`] exists: reporting a database outage as `404` sends the
/// caller off to look for a row that is very much there.
fn masked_denial<A: RepoAnchor>(error: AppError) -> AppError {
    if is_access_denial(&error) {
        A::masked()
    } else {
        error
    }
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
///
/// Every refusal is [`RepoAnchor::masked`] — the `401` an anonymous caller draws
/// on a private repository as much as the `403` an authenticated outsider draws.
/// The path-based [`RepoRead`] keeps both codes, and the difference is not an
/// inconsistency: there the caller supplied `{owner}/{name}` and learns nothing
/// from being refused by name, while here the caller supplied an integer, and
/// *any* answer other than the one an absent id gets confirms the integer hit a
/// row. That is also why the `401` cannot be kept for the anonymous case the way
/// [`AnchoredWrite`] keeps it: this gate has nothing to authenticate before the
/// row is resolved, so its `401` would arrive strictly after the lookup — an
/// oracle with no token required at all.
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
        check_read(state, &parts.headers, &repo)
            .await
            .map_err(masked_denial::<A>)?;
        A::admit(&row)?;
        Ok(Self { row, repo })
    }
}

/// An authenticated caller with write access to the repository an anchored row
/// belongs to.
///
/// Mirrors [`RepoWrite`], and keeps [`RepoAuthRead`]'s order: a missing token is
/// a `401` *before* anything is looked up, so the gate does not double as an
/// existence oracle for the rows of private repositories.
///
/// Past that `401` it takes visibility and permission as two questions, in that
/// order — the cut `OrgAdmin` draws for organizations:
///
/// - a caller who cannot *read* the repository is answered
///   [`RepoAnchor::masked`], because the row is not theirs to know about;
/// - a caller who can read it but not write is answered `403`, unmasked. They
///   can already see the row, so refusing them by permission tells them nothing
///   they did not have, and a `404` here would only make a real denial unreadable.
///
/// Both steps are needed, and the second alone was what this extractor had.
/// Masking that only one gate of a resource performs is not masking at all: the
/// caller picks the level by picking the verb, so `DELETE` confirmed with `403`
/// exactly what `GET` refuses to confirm.
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
        check_read_for(state, &repo, Some(actor_id))
            .await
            .map_err(masked_denial::<A>)?;
        A::admit(&row)?;
        check_write_for(state, &repo, Some(actor_id)).await?;
        Ok(Self {
            row,
            repo,
            actor_id,
        })
    }
}

/// The pull-request head a request names in its body or query, other than the
/// repository in its path.
pub trait HeadRefSource {
    /// `branch` of the path's repository, or `owner:branch` of a fork of it.
    fn head_ref(&self) -> &str;
}

/// A query string, as a body-position extractor — what [`PullHead`] wraps for
/// a `GET` that names its head in the query.
pub struct InQuery<T>(pub T);

impl<T> FromRequest<AppState> for InQuery<T>
where
    T: serde::de::DeserializeOwned + Send,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        let (mut parts, _body) = req.into_parts();
        let axum::extract::Query(query) =
            axum::extract::Query::<T>::from_request_parts(&mut parts, state)
                .await
                .map_err(|rejection| AppError::bad_request(rejection.body_text()))?;
        Ok(Self(query))
    }
}

/// A pull-request head, resolved and gated: the branch, and the fork it lives
/// in when it is not the path's repository.
///
/// The route layer judges the repository in the path. A `<owner>:<branch>`
/// head names a second repository, and a fork is a repository of its own —
/// a collaborator of a private parent is not a reader of somebody's private
/// fork of it. So the fork passes [`check_read`], the very gate its own pages
/// pass, for every caller, and a token's repository confinement on top. An
/// unreadable fork answers exactly like one that does not exist: `400`, so the
/// gate is no existence oracle (card_bb2ef2307588).
///
/// `E` is the extractor carrying the head — `Json<_>` or `Query<_>` — and is
/// handed back whole. Being a body extractor, it must be the *last* argument.
pub struct PullHead<E> {
    pub branch: String,
    pub fork: Option<rg_db::entities::repository::Model>,
    pub inner: E,
}

impl<E> FromRequest<AppState> for PullHead<E>
where
    E: FromRequest<AppState> + HeadRefSource + Send,
    E::Rejection: std::fmt::Display,
{
    type Rejection = AppError;

    async fn from_request(req: Request, state: &AppState) -> Result<Self, Self::Rejection> {
        let (mut parts, body) = req.into_parts();
        let (owner, name) = route_repo(&mut parts, state).await?;
        let base = resolve_repo(state, &owner, &name).await?;
        let headers = parts.headers.clone();
        let parts_path = parts.uri.path().to_string();
        let grant = parts
            .extensions
            .get::<crate::agent_scope::TokenGrant>()
            .cloned();
        let inner = E::from_request(Request::from_parts(parts, body), state)
            .await
            .map_err(|rejection| AppError::bad_request(rejection.to_string()))?;

        let head_ref = inner.head_ref().to_string();
        // A token confined to repositories is asked first, by name, before the
        // head is resolved: an absent and a disallowed head read alike, so the
        // resolver is no existence oracle for it.
        if let (Some(grant), Some((head_owner, _))) = (
            grant.as_ref().filter(|grant| grant.is_repo_restricted()),
            head_ref.split_once(':'),
        ) {
            let named =
                rg_core::repo::service::find_repo_by_owner_name(&state.db, head_owner, &base.name)
                    .await?;
            if !named.is_some_and(|named| grant.admits_repository(named.id)) {
                return Err(grant
                    .deny(
                        &headers,
                        "this token may not access the pull request head repository",
                        serde_json::json!({
                            "reason": "repository_not_allowed",
                            "path": parts_path,
                            "base_repo_id": base.id,
                        }),
                    )
                    .await);
            }
        }
        let (branch, fork_id) =
            rg_core::pull_request::resolve_head_ref(&state.db, base.id, &head_ref).await?;
        let Some(fork_id) = fork_id else {
            return Ok(Self {
                branch,
                fork: None,
                inner,
            });
        };
        let absent = || {
            AppError::from(rg_core::error::invalid_request(format!(
                "no repository found for head owner '{}'",
                head_ref.split_once(':').map_or("", |(owner, _)| owner)
            )))
        };
        let fork = rg_db::ops::repo_ops::find_by_id(&state.db, fork_id)
            .await?
            .ok_or_else(absent)?;
        if let Some(grant) = grant.filter(|grant| !grant.admits_repository(fork.id)) {
            return Err(grant
                .deny(
                    &headers,
                    "this token may not access the pull request head repository",
                    serde_json::json!({
                        "reason": "repository_not_allowed",
                        "base_repo_id": base.id,
                        "head_repo_id": fork.id,
                    }),
                )
                .await);
        }
        match check_read(state, &headers, &fork).await {
            Ok(()) => Ok(Self {
                branch,
                fork: Some(fork),
                inner,
            }),
            Err(AppError::NotFound(_) | AppError::Forbidden(_) | AppError::Unauthorized(_)) => {
                Err(absent())
            }
            Err(other) => Err(other),
        }
    }
}
