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
use tower_http::limit::RequestBodyLimitLayer;

use crate::AppState;

/// The names the credential middlewares answer to in the route table.
///
/// One constant per layer, used on both sides — by the [`Wrap`] constructor that
/// attaches the middleware and by the [`Access::Foreign`] level that claims it —
/// so the two cannot drift into two spellings of one name.
pub const RUNNER_AUTH_LAYER: &str = "authenticate_runner";
/// See [`RUNNER_AUTH_LAYER`].
pub const DOCS_AUTH_LAYER: &str = "docs_auth_middleware";

/// What a route requires of its caller.
///
/// This is the *contract*, not a description of the code behind it: the sweep
/// test exists precisely to catch a route whose implementation is weaker than
/// the level declared here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Access {
    /// Open to everyone, deliberately, *and the answer does not depend on who
    /// asks*: static content, a template list, a discovery document, a login
    /// form's counterpart. If the reply would differ per caller, the route is
    /// [`Access::PublicFiltered`] instead.
    Public,
    /// Open to everyone, but the answer **does** depend on who asks: the route
    /// serves a slice of instance-wide data and filters it down to what the
    /// caller may see. `/repos/explore`, `/repos/{owner}` and `/search` are
    /// this shape, and the filtering *is* their entire security property.
    ///
    /// The distinction exists because the persona passes cannot tell the two
    /// apart. A public row is owed `Expect::Allowed`, which every non-denial
    /// satisfies, so no public route can fail them however it answers — fine
    /// for static content, worthless for a data gate. Declaring the shape here
    /// hands `no_public_route_names_the_private_repo` a list it can hold to a
    /// stronger promise: answer an anonymous caller with real data, and never
    /// with data that caller may not see.
    PublicFiltered,
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
    /// Repository ownership — disposing of the repository itself, by deleting
    /// or transferring it. Stronger than [`Access::RepoAdmin`]: an
    /// organization admin administers a repository without owning it. Mirrors
    /// `api::repo_access::RepoOwner`.
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
    /// token, a WebSocket ticket. These are the routes the sweep cannot drive
    /// with a session token and therefore cannot check, so the sign-off names
    /// the mechanism — see [`ForeignGate`].
    Foreign(ForeignGate),
}

/// Where a [`Access::Foreign`] route's credential is actually checked.
///
/// `Foreign` is the one level the sweep answers `Expect::Unchecked` to, which
/// makes it the one level that can quietly become an *exemption*: `POST
/// /runners/register` declared `"CI runner: runner token via
/// `authenticate_runner`"` while carrying no such layer — the real gate was an
/// instance-admin session — and bought itself that exemption on the strength of
/// a free-text string nobody compared to anything (card_cd6f512e2e52).
///
/// So the string is a *claim* now, and `tests/integration/foreign_gate_guard.rs`
/// holds every `Foreign` route to it: the named layer has to be on the route,
/// or the named module has to be where the handler actually lives. A route that
/// can say neither has no business being `Foreign`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ForeignGate {
    /// The credential is checked by a middleware layer on the route itself.
    /// `layer` is the name that layer was registered under — see
    /// [`Wrap::credential`] — and the guard compares the two.
    Middleware {
        layer: &'static str,
        note: &'static str,
    },
    /// The handler checks the credential itself, because the protocol carries
    /// it in a shape no shared extractor understands: git-over-HTTP
    /// credentials, an OCI scope token, an LFS action signature, a CI job
    /// token, a WebSocket ticket. `module` is the source file the handler lives
    /// in, relative to `crates/rg-http/src/`, and the guard checks that it
    /// really does.
    Handler {
        module: &'static str,
        note: &'static str,
    },
}

impl ForeignGate {
    /// The prose half of the sign-off: what the mechanism is.
    pub fn note(self) -> &'static str {
        match self {
            Self::Middleware { note, .. } | Self::Handler { note, .. } => note,
        }
    }
}

impl Access {
    /// Whether the route is reachable with no credentials at all — either
    /// shape of "public".
    pub fn is_public(self) -> bool {
        matches!(self, Self::Public | Self::PublicFiltered)
    }

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
    /// The name of the credential middleware this route was registered with,
    /// if any — see [`Wrap::credential`]. `None` covers both "no layer at all"
    /// and "a layer that carries no credential", such as a body limit: neither
    /// can stand in for the check a `Foreign` route claims.
    pub credential: Option<&'static str>,
    /// `std::any::type_name` of the handler — `rg_http::oci::list_tags`.
    ///
    /// Recorded by the same statement that registers the route, so a route
    /// cannot claim to be gated inside a module its handler does not live in.
    /// Best-effort by definition (the compiler owes nobody a stable spelling),
    /// which is why only a test reads it.
    pub handler: &'static str,
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
/// The layer is held as a trait object rather than a function pointer: the
/// wrappers close over the `AppState` (the runner token check is state-carrying
/// middleware) and over the credential rate limiter.
///
/// What the wrapper carries besides the closure is its *name*, and only when it
/// is a credential check. A body limit and a runner-token gate are the same
/// type to `axum` and were the same thing to this table, so "is the layer this
/// route claims actually on it?" had no answer. Now it does: the name travels
/// into the route's [`RouteFact`] and the `Foreign` guard reads it back.
///
/// The name and the middleware are minted together, by the constructors below.
/// Handing both to a general-purpose `credential(name, closure)` would have left
/// the same gap one level down — the guard would read the *name* and believe it,
/// while `credential("authenticate_runner", &body_limit)` passed every check
/// green with no authentication anywhere on the route. So that constructor is
/// private and each public one applies the layer it names.
pub(crate) struct Wrap<'a> {
    credential: Option<&'static str>,
    apply: Box<dyn Fn(MethodRouter<AppState>) -> MethodRouter<AppState> + 'a>,
}

impl<'a> Wrap<'a> {
    /// A layer that carries no credential: a body limit, a rate limiter.
    pub(crate) fn plain(
        apply: impl Fn(MethodRouter<AppState>) -> MethodRouter<AppState> + 'a,
    ) -> Self {
        Self {
            credential: None,
            apply: Box::new(apply),
        }
    }

    /// A layer that *is* this route's credential check.
    ///
    /// `layer` is what [`ForeignGate::Middleware`] is held to. Private: the
    /// point of the name is that it cannot be claimed by a wrapper that does
    /// not do the check, and the only way to keep that true is to let nobody
    /// outside this module pair the two.
    fn credential(
        layer: &'static str,
        apply: impl Fn(MethodRouter<AppState>) -> MethodRouter<AppState> + 'a,
    ) -> Self {
        Self {
            credential: Some(layer),
            apply: Box::new(apply),
        }
    }
}

/// The credential layers themselves — the only wrappers allowed to answer to a
/// name in [`RouteFact::credential`].
///
/// Each one is the whole pair: the constant a `Foreign` level claims, and the
/// middleware that makes the claim true. A route gets both or neither.
impl Wrap<'static> {
    /// The runner-token gate: `authenticate_runner` refuses the request unless
    /// the bearer token belongs to the runner named in the path.
    pub(crate) fn runner_auth(state: &AppState) -> Self {
        Self::credential(RUNNER_AUTH_LAYER, runner_auth_layer(state))
    }

    /// The runner-token gate over a raised body limit, for the cache upload.
    ///
    /// The limit rides along; it does not replace the check the route declares,
    /// so the wrapper still answers to [`RUNNER_AUTH_LAYER`]. The gate is
    /// applied last, i.e. outermost: an unauthenticated caller is turned away
    /// before a gigabyte of body is read.
    pub(crate) fn runner_auth_with_body_limit(state: &AppState, body_limit: usize) -> Self {
        let gate = runner_auth_layer(state);
        Self::credential(RUNNER_AUTH_LAYER, move |mr: MethodRouter<AppState>| {
            gate(mr.layer(RequestBodyLimitLayer::new(body_limit)))
        })
    }

    /// The API-docs gate.
    ///
    /// Outermost first: `pat_auth_middleware` translates a PAT into the bearer
    /// token `docs_auth_middleware` then checks, so it has to run before it —
    /// i.e. be applied last.
    pub(crate) fn docs_auth(state: &AppState) -> Self {
        let state = state.clone();
        Self::credential(DOCS_AUTH_LAYER, move |mr: MethodRouter<AppState>| {
            mr.layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::pat_auth::docs_auth_middleware,
            ))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                crate::pat_auth::pat_auth_middleware,
            ))
        })
    }
}

/// The runner-token middleware as a `MethodRouter` wrapper, shared by the plain
/// runner gate and the one that carries a body limit — so "the cache upload is
/// also gated" is not a second spelling of the same layer.
fn runner_auth_layer(
    state: &AppState,
) -> impl Fn(MethodRouter<AppState>) -> MethodRouter<AppState> + 'static {
    let state = state.clone();
    move |mr: MethodRouter<AppState>| {
        mr.layer(axum::middleware::from_fn_with_state(
            state.clone(),
            crate::api::runners::authenticate_runner,
        ))
    }
}

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

    /// Hand the table to `f` and take it back — for a family of routes that is
    /// generated from a list rather than spelled out one call at a time.
    ///
    /// It adds no way around the [`Access`] argument: `f` only has the same
    /// `get`/`post`/… methods every other caller has.
    pub(crate) fn with(self, f: impl FnOnce(Self) -> Self) -> Self {
        f(self)
    }

    fn add(
        mut self,
        access: Access,
        method: &'static str,
        path: &'static str,
        method_router: MethodRouter<AppState>,
        credential: Option<&'static str>,
        handler: &'static str,
    ) -> Self {
        self.facts.push(RouteFact {
            method,
            path: format!("{}{}", self.prefix, path),
            access,
            credential,
            handler,
        });
        self.router = self.router.route(path, method_router);
        self
    }

    pub(crate) fn get<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "GET",
            path,
            axum::routing::get(handler),
            None,
            std::any::type_name::<H>(),
        )
    }

    pub(crate) fn head<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "HEAD",
            path,
            axum::routing::head(handler),
            None,
            std::any::type_name::<H>(),
        )
    }

    pub(crate) fn post<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "POST",
            path,
            axum::routing::post(handler),
            None,
            std::any::type_name::<H>(),
        )
    }

    pub(crate) fn put<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "PUT",
            path,
            axum::routing::put(handler),
            None,
            std::any::type_name::<H>(),
        )
    }

    pub(crate) fn patch<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "PATCH",
            path,
            axum::routing::patch(handler),
            None,
            std::any::type_name::<H>(),
        )
    }

    pub(crate) fn delete<H, T>(self, access: Access, path: &'static str, handler: H) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "DELETE",
            path,
            axum::routing::delete(handler),
            None,
            std::any::type_name::<H>(),
        )
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
        wrap: &Wrap<'_>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "GET",
            path,
            (wrap.apply)(axum::routing::get(handler)),
            wrap.credential,
            std::any::type_name::<H>(),
        )
    }

    pub(crate) fn post_with<H, T>(
        self,
        access: Access,
        path: &'static str,
        handler: H,
        wrap: &Wrap<'_>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "POST",
            path,
            (wrap.apply)(axum::routing::post(handler)),
            wrap.credential,
            std::any::type_name::<H>(),
        )
    }

    pub(crate) fn put_with<H, T>(
        self,
        access: Access,
        path: &'static str,
        handler: H,
        wrap: &Wrap<'_>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "PUT",
            path,
            (wrap.apply)(axum::routing::put(handler)),
            wrap.credential,
            std::any::type_name::<H>(),
        )
    }

    pub(crate) fn patch_with<H, T>(
        self,
        access: Access,
        path: &'static str,
        handler: H,
        wrap: &Wrap<'_>,
    ) -> Self
    where
        H: Handler<T, AppState>,
        T: 'static,
    {
        self.add(
            access,
            "PATCH",
            path,
            (wrap.apply)(axum::routing::patch(handler)),
            wrap.credential,
            std::any::type_name::<H>(),
        )
    }
}
