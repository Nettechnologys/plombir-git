//! The access level every route declares, and the builder that makes the
//! declaration mandatory.
//!
//! An `axum::Router` cannot be read back: once a route is registered nothing
//! can ask the router which paths it serves, with which methods, behind which
//! gate. That is why "did we forget one?" used to be a question only a pair of
//! eyes could answer, over 245 routes — and why every access hole in this phase
//! was found by reading, one module at a time.
//!
//! [`RouteTable`] answers it by construction. It has no `route()`: the only way
//! to register a route is a method that takes an [`Access`] value, and the same
//! call records a [`RouteFact`]. Router and table are two outputs of one
//! statement, so a route cannot exist without a declared access level and the
//! declaration cannot drift away from the route it describes.
//!
//! The facts are what `tests/integration/route_access_sweep_tests.rs` walks:
//! one pass per persona — anonymous, outsider, owner — over every route the
//! server exposes.

use axum::handler::Handler;
use axum::routing::MethodRouter;
use axum::Router;

use crate::AppState;

/// What a route requires of its caller.
///
/// This is the *contract*, not a description of the code behind it: the sweep
/// test exists precisely to catch a route whose implementation is weaker than
/// the level declared here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Open to everyone, deliberately. The answer must not depend on who asks
    /// — or must already filter itself by what the caller may see.
    Public,
    /// Any authenticated user. Not scoped to a repository.
    User,
    /// Repository read. A public repository stays anonymously readable, a
    /// private one does not. Mirrors `api::repo_access::RepoRead`.
    RepoRead,
    /// Repository read that additionally requires authentication.
    /// Mirrors `api::repo_access::RepoAuthRead`.
    RepoAuthRead,
    /// Repository write. Mirrors `api::repo_access::RepoWrite`.
    RepoWrite,
    /// Repository administration. Mirrors `api::repo_access::RepoAdmin`.
    RepoAdmin,
    /// Repository ownership — deleting, forking out of, or transferring the
    /// repository. There is no extractor for this level yet; the handlers
    /// compare `repo.owner_id` themselves.
    RepoOwner,
    /// Read access to the organization in the path, following the same shape as
    /// [`Access::RepoRead`]: a public organization is anonymously readable, a
    /// private one is visible to its members only — and answers `404` rather
    /// than `403`, so it is not an existence oracle either.
    OrgRead,
    /// An owner/admin of the organization in the path.
    OrgAdmin,
    /// Instance administrator.
    InstanceAdmin,
    /// A different credential mechanism entirely — a runner token, an OCI
    /// bearer token, git-over-HTTP credentials, a CI job token, an LFS action
    /// token, a WebSocket ticket. The string is the sign-off: it names the
    /// mechanism, because these routes are the ones the sweep cannot drive with
    /// a session token and therefore cannot check.
    Foreign(&'static str),
}

impl Access {
    /// Whether the level is about *this* repository, i.e. whether the sweep's
    /// private-repository fixture is the right thing to point it at.
    pub fn is_repo_scoped(self) -> bool {
        matches!(
            self,
            Self::RepoRead
                | Self::RepoAuthRead
                | Self::RepoWrite
                | Self::RepoAdmin
                | Self::RepoOwner
        )
    }
}

/// One `(method, path, access)` row of the route table.
#[derive(Clone, Debug)]
pub struct RouteFact {
    /// Uppercase HTTP method, e.g. `"GET"`.
    pub method: &'static str,
    /// The full public path, nesting prefix included — what a client sends,
    /// not what the sub-router was built with.
    pub path: String,
    /// The access level the route declares.
    pub access: Access,
}

impl RouteFact {
    /// `GET /api/v1/repos/{owner}/{name}` — how the sweep names a route.
    pub fn label(&self) -> String {
        format!("{} {}", self.method, self.path)
    }
}

/// A router under construction, together with the access level of every route
/// put into it.
///
/// Deliberately without a `route()` method: `get`/`post`/`put`/`patch`/
/// `delete`/`head` all take an [`Access`] first, so "add a route" and "declare
/// its access level" are the same act.
pub(crate) struct RouteTable {
    /// Prefix this sub-router is nested under, so the recorded path is the one
    /// a client actually sends.
    prefix: &'static str,
    router: Router<AppState>,
    facts: Vec<RouteFact>,
}

/// Applied to a route's `MethodRouter` before it is registered — the per-route
/// body limit and the credential middleware a few routes carry.
///
/// A trait object rather than a function pointer: the wrappers close over the
/// `AppState` (the runner token check is state-carrying middleware) and over
/// the credential rate limiter.
type Wrap<'a> = &'a dyn Fn(MethodRouter<AppState>) -> MethodRouter<AppState>;

impl RouteTable {
    pub(crate) fn new(prefix: &'static str) -> Self {
        Self {
            prefix,
            router: Router::new(),
            facts: Vec::new(),
        }
    }

    /// The finished router and the facts about it.
    pub(crate) fn finish(self) -> (Router<AppState>, Vec<RouteFact>) {
        (self.router, self.facts)
    }

    fn add(
        mut self,
        access: Access,
        method: &'static str,
        path: &'static str,
        method_router: MethodRouter<AppState>,
    ) -> Self {
        self.facts.push(RouteFact {
            method,
            path: format!("{}{}", self.prefix, path),
            access,
        });
        self.router = self.router.route(path, method_router);
        self
    }

    pub(crate) fn get<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "GET", path, axum::routing::get(handler))
    }

    pub(crate) fn head<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "HEAD", path, axum::routing::head(handler))
    }

    pub(crate) fn post<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "POST", path, axum::routing::post(handler))
    }

    pub(crate) fn put<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "PUT", path, axum::routing::put(handler))
    }

    pub(crate) fn patch<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "PATCH", path, axum::routing::patch(handler))
    }

    pub(crate) fn delete<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "DELETE", path, axum::routing::delete(handler))
    }

    // ── Layered variants ───────────────────────────────────────────────────
    //
    // A handful of routes carry their own middleware: a raised body limit for
    // uploads, the stricter limiter on the credential endpoints, the runner
    // token check. `wrap` is applied to that one route's `MethodRouter`, which
    // is the same thing the chained `.route(path, post(h).layer(l))` form did.

    pub(crate) fn get_with<H, T>(
        self,
        access: Access,
        path: &'static str,
        handler: H,
        wrap: Wrap<'_>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "GET", path, wrap(axum::routing::get(handler)))
    }

    pub(crate) fn post_with<H, T>(
        self,
        access: Access,
        path: &'static str,
        handler: H,
        wrap: Wrap<'_>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "POST", path, wrap(axum::routing::post(handler)))
    }

    pub(crate) fn put_with<H, T>(
        self,
        access: Access,
        path: &'static str,
        handler: H,
        wrap: Wrap<'_>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "PUT", path, wrap(axum::routing::put(handler)))
    }

    pub(crate) fn patch_with<H, T>(
        self,
        access: Access,
        path: &'static str,
        handler: H,
        wrap: Wrap<'_>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(access, "PATCH", path, wrap(axum::routing::patch(handler)))
    }
}
