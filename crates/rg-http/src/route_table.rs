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

use std::fmt;

use axum::extract::DefaultBodyLimit;
use axum::handler::Handler;
use axum::http::StatusCode;
use axum::response::Response;
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
    /// An owner/admin of the organization in the path. Masks a private
    /// organization exactly as [`Access::OrgRead`] does — an outsider is `404`,
    /// not `403` — because otherwise the masking is defeated by changing the
    /// verb on the same path.
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
    ///
    /// `gates` is the other half, and the half that took a second card to
    /// arrive: living in the named file was all this variant ever claimed, and
    /// a handler that lives there while checking nothing satisfied it. So the
    /// route also names the function its handler has to *reach* — directly or
    /// through the helpers of its own module — and
    /// `tests/integration/foreign_gate_guard.rs` walks the call graph to
    /// confirm it. Several names mean "at least one of these", which is what a
    /// protocol whose read and write paths gate separately actually promises.
    ///
    /// An empty slice is the one route shape with nothing to reach: a constant
    /// answer that reads neither the caller nor the database — the registry's
    /// `GET /v2/` discovery challenge. It is checked in the other direction
    /// instead, so it cannot become a way to opt out of the check.
    Handler {
        module: &'static str,
        gates: &'static [&'static str],
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

    /// Whether the level is about the *organization* named in the path.
    ///
    /// The twin of [`is_repo_scoped`](Self::is_repo_scoped), and it exists for
    /// the same reason: a level that proves something about a *container* leaves
    /// every global id in the path unproven, so the id-scope sweeps select their
    /// population by asking which container the gate settled. There was no way
    /// to ask that about an organization, and the three sweeps that existed all
    /// keyed on `is_repo_scoped` — which is exactly why the org axis went
    /// unswept while every one of them read as covering the table
    /// (card_40e6878aea09).
    pub fn is_org_scoped(self) -> bool {
        matches!(self, Self::OrgRead | Self::OrgAdmin)
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

    /// A non-credential wrapper carrying a complete request-body boundary.
    pub(crate) fn body_limit(body_limit: usize) -> Self {
        Self::plain(move |mr| apply_body_limit(mr, body_limit))
    }

    /// A layer that *is* this route's credential check.
    ///
    /// `layer` is what [`ForeignGate::Middleware`] is held to. Private: the
    /// point of the name is that it cannot be claimed by a wrapper that does
    /// not do the check, and the only way to keep that true is to let nobody
    /// outside this module pair the two.
    ///
    /// The visibility is load-bearing rather than tidy — half of
    /// `foreign_gate_guard` reads the name back and believes it — so it is
    /// asserted: `foreign_gate_guard::the_credential_constructor_is_private` is
    /// what fails if a `pub` lands here, and its sibling is what fails if the
    /// call moves out of the block of named constructors below.
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

/// Apply both halves of one declared request-body boundary.
///
/// `RequestBodyLimitLayer` is the transport ceiling, but buffered extractors
/// such as `Bytes` and `Multipart` independently inherit Axum's 2 MiB
/// `DefaultBodyLimit`. Keeping the pair here prevents a route from advertising
/// a larger limit while still failing before its handler at 2 MiB.
fn apply_body_limit<S>(mr: MethodRouter<S>, body_limit: usize) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
{
    mr.layer::<_, std::convert::Infallible>(DefaultBodyLimit::max(body_limit))
        .layer::<_, std::convert::Infallible>(RequestBodyLimitLayer::new(body_limit))
        // Outermost of the three, so it sees whichever half refused: the
        // transport ceiling answers a declared `Content-Length` before the
        // request is routed, the extractor answers a body that only turns out
        // to be too long while it is read. Both leave a `413` nobody can put a
        // number on; this attaches the one this route declared.
        .layer::<_, std::convert::Infallible>(axum::middleware::map_response(
            move |mut response: Response| async move {
                if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
                    response
                        .extensions_mut()
                        .insert(DeclaredBodyLimit(body_limit));
                }
                response
            },
        ))
}

/// The ceiling a route declared, carried out on the refusal that enforced it.
///
/// Both halves of [`apply_body_limit`] refuse an oversized body before any
/// handler runs, and neither says *which* limit was crossed: tower-http writes
/// `length limit exceeded`, Axum's extractor writes `Failed to buffer the
/// request body`. The number exists only at the call that declared it, so it
/// travels on the response and the envelope layers of `/api/v1`
/// ([`crate::error::api_rejection_envelope`]) and `/v2`
/// ([`crate::oci::oci_transport_refusal_envelope`]) read it back. That is the
/// whole difference between the client being told `HTTP 413` and being told
/// which limit its upload has to fit under.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DeclaredBodyLimit(pub(crate) usize);

impl fmt::Display for DeclaredBodyLimit {
    /// Binary units, but only where they are exact. `512 MiB` reads as
    /// `512 MiB`; a limit built by arithmetic — `CONTENT_EDIT_JSON_MAX_BYTES`
    /// is `MAX_BLOB_API_BYTES * 6 + 64 KiB` — keeps its byte count rather than
    /// being rounded into a number this server does not actually enforce.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const UNITS: [(usize, &str); 3] = [
            (1024 * 1024 * 1024, "GiB"),
            (1024 * 1024, "MiB"),
            (1024, "KiB"),
        ];
        for (scale, unit) in UNITS {
            if self.0 >= scale && self.0 % scale == 0 {
                return write!(f, "{} {unit}", self.0 / scale);
            }
        }
        write!(f, "{} bytes", self.0)
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
            gate(apply_body_limit(mr, body_limit))
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
        let full_path = format!("{}{}", self.prefix, path);
        // Every route answers to a narrowed token's repository confinement,
        // judged from the level declared right here — so a route cannot be
        // added without it, and a handler cannot forget it
        // (card_60a80311d512).
        let gateway = full_path == crate::agent_scope::MCP_ENDPOINT_PATH;
        let method_router = method_router.layer(axum::middleware::from_fn(
            move |req: axum::extract::Request, next: axum::middleware::Next| {
                crate::agent_scope::enforce(access, gateway, req, next)
            },
        ));
        self.facts.push(RouteFact {
            method,
            path: full_path,
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

#[cfg(test)]
mod body_limit_tests {
    use super::{apply_body_limit, DeclaredBodyLimit};
    use axum::body::{to_bytes, Body, Bytes};
    use axum::http::{header, Request, StatusCode};
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    const DECLARED_LIMIT: usize = 3 * 1024 * 1024;

    fn upload_request(bytes: Vec<u8>) -> Request<Body> {
        Request::builder()
            .method("POST")
            .uri("/upload")
            .header(header::CONTENT_LENGTH, bytes.len().to_string())
            .body(Body::from(bytes))
            .unwrap()
    }

    async fn buffered_upload(_body: Bytes) -> StatusCode {
        StatusCode::NO_CONTENT
    }

    async fn raw_upload(body: Body) -> StatusCode {
        match to_bytes(body, usize::MAX).await {
            Ok(_) => StatusCode::NO_CONTENT,
            Err(_) => StatusCode::PAYLOAD_TOO_LARGE,
        }
    }

    #[tokio::test]
    async fn buffered_extractor_crosses_axum_default_below_the_declared_limit() {
        let app: Router = Router::new().route(
            "/upload",
            apply_body_limit(post(buffered_upload), DECLARED_LIMIT),
        );
        let response = app
            .oneshot(upload_request(vec![b'x'; 2 * 1024 * 1024 + 1]))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
    }

    #[tokio::test]
    async fn transport_rejects_a_raw_body_above_the_declared_limit() {
        let app: Router = Router::new().route(
            "/upload",
            apply_body_limit(post(raw_upload), DECLARED_LIMIT),
        );
        let response = app
            .oneshot(upload_request(vec![b'x'; DECLARED_LIMIT + 1]))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response.extensions().get::<DeclaredBodyLimit>(),
            Some(&DeclaredBodyLimit(DECLARED_LIMIT)),
            "the refusal has to carry the number, or the envelope layer above              has nothing to tell the client but the status"
        );
    }

    /// The other half of the boundary. Without a `Content-Length` the
    /// transport ceiling cannot answer up front — the body is refused while it
    /// is read, by the extractor — and that refusal is just as anonymous.
    #[tokio::test]
    async fn a_streamed_body_over_the_limit_carries_the_number_too() {
        let app: Router = Router::new().route(
            "/upload",
            apply_body_limit(post(buffered_upload), DECLARED_LIMIT),
        );
        let oversized = futures::stream::iter(
            (0..64).map(|_| Ok::<_, std::io::Error>(Bytes::from_static(&[b'x'; 64 * 1024]))),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/upload")
            .body(Body::from_stream(oversized))
            .unwrap();

        let response = app.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(
            response.extensions().get::<DeclaredBodyLimit>(),
            Some(&DeclaredBodyLimit(DECLARED_LIMIT))
        );
    }

    /// A `204` must not pick up a stamp: the extension is what the envelope
    /// layers key on, and a response that was never refused has no limit to
    /// report.
    #[tokio::test]
    async fn an_accepted_upload_carries_no_stamp() {
        let app: Router = Router::new().route(
            "/upload",
            apply_body_limit(post(raw_upload), DECLARED_LIMIT),
        );
        let response = app.oneshot(upload_request(vec![b'x'; 16])).await.unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert!(response.extensions().get::<DeclaredBodyLimit>().is_none());
    }
}

#[cfg(test)]
mod declared_body_limit_rendering_tests {
    use super::DeclaredBodyLimit;

    #[test]
    fn an_exact_binary_multiple_reads_as_one() {
        assert_eq!(DeclaredBodyLimit(512 * 1024 * 1024).to_string(), "512 MiB");
        assert_eq!(
            DeclaredBodyLimit(10 * 1024 * 1024 * 1024).to_string(),
            "10 GiB"
        );
        assert_eq!(DeclaredBodyLimit(64 * 1024).to_string(), "64 KiB");
    }

    /// `CONTENT_EDIT_JSON_MAX_BYTES` is `MAX_BLOB_API_BYTES * 6 + 64 KiB`.
    /// Rounding it to `6 MiB` would name a ceiling the server does not
    /// enforce, and the client would be told its request fits when it does not.
    #[test]
    fn an_arithmetic_limit_keeps_its_byte_count() {
        assert_eq!(
            DeclaredBodyLimit(6 * 1024 * 1024 + 64 * 1024 + 1).to_string(),
            "6356993 bytes"
        );
        assert_eq!(DeclaredBodyLimit(999).to_string(), "999 bytes");
    }
}
