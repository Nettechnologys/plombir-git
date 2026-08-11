//! Axum router construction: the CORS layer, the full REST/Git/OCI route table,
//! and the production vs. test router assembly.
//!
//! Every route is registered through [`RouteTable`], which has no `route()`:
//! the only way in is a method that takes an [`Access`] level. Registering a
//! route and declaring who may call it are therefore the same statement, and
//! the table of `(method, path, access)` rows that falls out of the build is
//! what the sweep test walks. See [`crate::route_table`].

use axum::http::{header, HeaderValue, Method};
use axum::routing::MethodRouter;
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::services::ServeDir;
use tower_http::trace::TraceLayer;

use crate::route_table::Access::{
    self, Foreign, InstanceAdmin, OrgAdmin, OrgRead, Public, PublicFiltered, RepoAdmin,
    RepoAuthRead, RepoOwner, RepoRead, RepoWrite, User,
};
use crate::route_table::ForeignGate::{Handler, Middleware};
use crate::route_table::{RouteFact, RouteTable, Wrap, RUNNER_AUTH_LAYER};
use crate::{
    api, git_http, handlers, metrics, middleware, oci, openapi, pat_auth, rate_limit, security, ws,
    AppState,
};

/// Sign-off for the routes whose credentials are not a ForgeKeep session, so
/// the sweep cannot drive them with one. Spelled out here rather than at each
/// call site so the whole set reads in one place.
///
/// Each one names *where* its credential is checked, not merely that it is:
/// a `Middleware` claim is compared against the layer the route was registered
/// with, a `Handler` claim against the module its handler actually lives in
/// *and* the gate function that handler has to reach. The one that made this
/// necessary declared a runner-token middleware it did not carry — see
/// [`crate::route_table::ForeignGate`].
///
/// The gate names are why there is one constant per mechanism rather than one
/// per file: `/v2/` answers everybody the same challenge and reads nothing,
/// while every other registry route goes through `require_access`; the
/// notification socket proves who is calling and the job-log socket asks the
/// repository gate on top of that. Those are different promises, and a single
/// constant would have made the weakest of them the promise for all of them.
const GIT_HTTP: Access = Foreign(Handler {
    module: "git_http.rs",
    gates: &["check_git_access"],
    note: "git-over-HTTP: PAT / HTTP-Basic, `check_git_access` gate",
});
const OCI_TOKEN: Access = Foreign(Handler {
    module: "oci.rs",
    gates: &["require_access"],
    note: "OCI registry: registry-scoped bearer token",
});
/// `GET /v2/` and `GET /v2` — the discovery challenge, and the round trip
/// `docker login` uses to find out whether its credentials were accepted.
///
/// To a caller with no credentials it is the constant `401` naming the realm,
/// derived from nothing but the request's own `Host`. To a caller presenting a
/// credential this registry recognises it is a bare `200`, because that is what
/// `docker login` reads as "these are good" and without it login can never
/// succeed. Still gateless, and still reads nothing: recognising a signature is
/// not an authorization decision, and every question about what that caller may
/// actually do is asked per request by `require_access` on the routes below.
const OCI_DISCOVERY: Access = Foreign(Handler {
    module: "oci.rs",
    gates: &[],
    note: "OCI registry: `WWW-Authenticate` challenge, or a bare `200` for a recognised credential",
});
/// The three LFS routes gate per operation and the batch endpoint gates both
/// ways, so the sign-off names all three shapes the protocol uses: the two
/// `repo_access` calls `batch` and `upload_object` make directly, and
/// `download_object`'s own `authorize_lfs_download`, which takes an LFS action
/// signature into account before falling back to the read gate.
const LFS_PROTOCOL: Access = Foreign(Handler {
    module: "api/lfs.rs",
    gates: &[
        "check_read_for",
        "check_write_for",
        "authorize_lfs_download",
    ],
    note: "Git LFS batch protocol: per-operation gate, own envelope",
});
const RUNNER_TOKEN: Access = Foreign(Middleware {
    layer: RUNNER_AUTH_LAYER,
    note: "CI runner: runner token via `authenticate_runner`",
});
const CI_JOB_TOKEN: Access = Foreign(Handler {
    module: "api/ci_oidc.rs",
    gates: &["ci_job_binding"],
    note: "CI job token minted for a running job",
});
/// The notification socket: a per-user stream, so the credential *is* the
/// authorization — the hub only ever hands a connection its own user's
/// notifications.
const WS_SESSION: Access = Foreign(Handler {
    module: "ws.rs",
    gates: &["ws_session"],
    note: "WebSocket: session from the HttpOnly cookie, `Sec-WebSocket-Protocol` or `?token=`",
});
/// The job-log socket: the ticket says who is calling, and what that user may
/// read is the shared repository gate's decision, exactly as on the REST route
/// serving the same logs.
const WS_JOB_LOG: Access = Foreign(Handler {
    module: "ws.rs",
    gates: &["check_read_for"],
    note: "WebSocket: session via `ws_session`, then the repository read gate",
});
/// Split the configured origin list into what the CORS layer will accept and
/// what it had to drop, with the reason for each drop.
///
/// A malformed entry used to disappear inside a `filter_map`: the list was
/// filtered through `HeaderValue::from_str(...).ok()` and whatever failed was
/// gone without a word. Two things were wrong with that.
///
/// The first is the silence. A dropped origin is diagnosed from the browser,
/// as a CORS failure on one frontend while the others keep working — the most
/// expensive place to debug anything, because the server said nothing.
///
/// The second is that `HeaderValue::from_str` was never the check anyone
/// thought it was. It rejects only control bytes (`b >= 32 && b != 127`), so
/// `https:// example.com`, `example.com`, `не origin` and `https://foo/bar`
/// all pass it, land in the allowlist, and then match no browser `Origin`
/// header ever — silently, and for the whole life of the process. That is the
/// same symptom, reached without a single invalid header value.
///
/// So the entries are checked as *origins* (RFC 6454: `scheme://host[:port]`,
/// nothing after the authority), which is what they are compared against.
/// `*` is passed through unchanged: it is a wildcard the operator may have
/// configured, not an origin to validate.
fn parse_cors_origins(configured: &str) -> (Vec<HeaderValue>, Vec<(String, &'static str)>) {
    let mut accepted = Vec::new();
    let mut rejected = Vec::new();

    for entry in configured
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty())
    {
        match cors_origin_defect(entry) {
            None => match HeaderValue::from_str(entry) {
                Ok(value) => accepted.push(value),
                // Unreachable for anything `cors_origin_defect` accepts, but
                // the layer takes `HeaderValue`s and this is where the
                // conversion lives — a defect here is reported, not dropped.
                Err(_) => rejected.push((entry.to_string(), "not a valid HTTP header value")),
            },
            Some(defect) => rejected.push((entry.to_string(), defect)),
        }
    }

    (accepted, rejected)
}

/// Why `entry` cannot be an origin, or `None` if it can.
fn cors_origin_defect(entry: &str) -> Option<&'static str> {
    if entry == "*" {
        return None;
    }

    let Some((scheme, authority)) = entry.split_once("://") else {
        return Some("missing a scheme — an origin looks like 'https://host[:port]'");
    };
    if scheme.is_empty()
        || !scheme.starts_with(|c: char| c.is_ascii_alphabetic())
        || !scheme
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
    {
        return Some("the scheme is not a valid URL scheme");
    }
    if authority.is_empty() {
        return Some("no host after the scheme");
    }
    if authority.contains(['/', '?', '#']) {
        return Some("an origin carries no path, query or fragment");
    }
    if authority.contains('@') {
        return Some("an origin carries no userinfo");
    }
    if !authority
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | ':' | '[' | ']' | '_'))
    {
        return Some("the host contains characters that cannot appear in an origin");
    }
    None
}

/// Build a restrictive CORS layer.
///
/// If `FORGEKEEP_CORS_ORIGINS` is set (comma-separated URLs), only those
/// origins are allowed. Otherwise, all origins are reflected (for development
/// convenience) with a warning logged.
///
/// Replaces `CorsLayer::permissive()` — restricts allowed methods and headers
/// to only what ForgeKeep needs.
fn build_cors_layer() -> CorsLayer {
    use std::time::Duration;

    let methods = [
        Method::GET,
        Method::POST,
        Method::PUT,
        Method::DELETE,
        Method::PATCH,
        Method::OPTIONS,
    ];

    let headers_list = [header::AUTHORIZATION, header::CONTENT_TYPE, header::ACCEPT];

    match std::env::var("FORGEKEEP_CORS_ORIGINS").ok() {
        Some(origins_str) if !origins_str.is_empty() => {
            let (origins, rejected) = parse_cors_origins(&origins_str);
            for (entry, defect) in &rejected {
                tracing::warn!(
                    entry = %entry,
                    reason = defect,
                    "FORGEKEEP_CORS_ORIGINS: ignoring an entry that is not a usable origin"
                );
            }

            if origins.is_empty() {
                tracing::warn!(
                    "FORGEKEEP_CORS_ORIGINS set but no valid origins parsed — CORS disabled"
                );
                CorsLayer::new()
                    .allow_methods(methods)
                    .allow_headers(headers_list)
            } else {
                tracing::info!(origins = ?origins, "CORS: allowing configured origins");
                CorsLayer::new()
                    .allow_origin(origins)
                    .allow_methods(methods)
                    .allow_headers(headers_list)
                    .allow_credentials(true)
                    .max_age(Duration::from_secs(3600))
            }
        }
        _ => {
            tracing::warn!(
                "FORGEKEEP_CORS_ORIGINS not set — CORS allows all origins (not recommended for production)"
            );
            CorsLayer::new()
                .allow_origin(tower_http::cors::AllowOrigin::mirror_request())
                .allow_methods(methods)
                .allow_headers(headers_list)
                .allow_credentials(true)
                .max_age(Duration::from_secs(3600))
        }
    }
}

/// Every sub-router the server exposes, plus the access level of every route in
/// them.
///
/// The facts travel with the routers rather than being rebuilt on the side:
/// a second enumeration is a second thing to forget to update.
pub(crate) struct Routers {
    /// `/api/v1`
    api_v1: Router<AppState>,
    /// `/git`
    git: Router<AppState>,
    /// Root-level Git Smart HTTP, health and metrics.
    root: Router<AppState>,
    /// `/v2` — OCI distribution.
    v2: Router<AppState>,
    /// OpenAPI JSON and the Swagger UI.
    docs: Router<AppState>,
    /// `(method, path, access)` for every route above.
    facts: Vec<RouteFact>,
}

/// Create the Axum router (Git + REST API + health).
///
/// `rate_limiter` is the global per-IP limiter applied to every request;
/// `auth_rate_limiter` is a separate, stricter limiter applied only to the
/// unauthenticated credential endpoints (`/users/register`, `/users/login`) to
/// blunt registration spam and password guessing independently of the global
/// limit (which is off by default).
pub(crate) fn create_router(
    state: AppState,
    rate_limiter: rate_limit::RateLimiter,
    auth_rate_limiter: rate_limit::RateLimiter,
) -> Router {
    build_router(state, rate_limiter, auth_rate_limiter)
}

/// Shared router builder used by both production and test routers.
///
/// CRITICAL: Axum `nest()` State requirement (pitfall #2)
///
/// All nested routers MUST share the same `State<AppState>` type.
/// If `git_routes` or `api_v1` use a different State type,
/// Axum will reject the route with a compile-time error.
///
/// Correct pattern (used here):
///   let git_routes = Router::new()...with_state(state.clone());
///   let api_v1 = Router::new()...with_state(state.clone());
///   Router::new().nest("/git", git_routes).nest("/api/v1", api_v1)
///
/// Wrong pattern (will not compile):
///   let git_routes = Router::new()...with_state(git_state);  // different type
///   let api_v1 = Router::new()...with_state(api_state);     // different type
///   Router::new().nest("/git", git_routes).nest("/api/v1", api_v1)  // ERROR
fn build_router(
    state: AppState,
    rate_limiter: rate_limit::RateLimiter,
    auth_rate_limiter: rate_limit::RateLimiter,
) -> Router {
    let routers = build_all_routes(&state, Some(&auth_rate_limiter));

    apply_middleware(
        with_spa_fallback(assemble(&routers), &state),
        &state,
        Some(&rate_limiter),
    )
    .with_state(state)
}

/// The server's middleware stack — the one and only copy of it.
///
/// Production and test routers used to carry two hand-written stacks kept in
/// step by a comment, and twice they were not: the maintenance gate was missing
/// from the test router (card_42d82e91dbe3), and so were the security headers
/// and the metrics layer (card_17ea3d843ca7). Both times every test stayed
/// green, because the thing that was missing was the thing that would have
/// noticed. The stack lives here now, so the two routers cannot drift: the only
/// difference either side is allowed is the `rate_limiter`, and that difference
/// is this argument.
///
/// Layer order is bottom-up — the last `.layer()` runs first.
fn apply_middleware(
    router: Router<AppState>,
    state: &AppState,
    rate_limiter: Option<&rate_limit::RateLimiter>,
) -> Router<AppState> {
    let router = router
        // Innermost, so a rejected session is still counted and traced — and so
        // rate limiting and maintenance mode both get to answer before it spends
        // a database read.
        .layer(axum::middleware::from_fn_with_state(
            state.clone(),
            api::auth::session_standing_middleware,
        ))
        .layer(axum::middleware::from_fn(
            middleware::http_metrics_middleware,
        ))
        .layer(axum::middleware::from_fn(middleware::request_id_middleware))
        .layer(TraceLayer::new_for_http().make_span_with(
            |request: &axum::http::Request<axum::body::Body>| {
                let request_id = request
                    .headers()
                    .get("x-request-id")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or("-");
                tracing::info_span!(
                    "http_request",
                    method = %request.method(),
                    uri = %request.uri(),
                    status = tracing::field::Empty,
                    request_id = %request_id,
                )
            },
        ))
        .layer(build_cors_layer());

    // The per-IP limiter reads `ConnectInfo`, which only a real `axum::serve`
    // with `into_make_service_with_connect_info` supplies; the test harness
    // serves without it, so both layers stay off there. This is the one
    // deliberate difference between the two stacks — see `build_test_router`.
    // Because it is the one difference, it is also the one layer the ordinary
    // suite cannot notice going missing; `rate_limit_mounted_tests` builds the
    // production router with a tiny budget and serves it the production way, so
    // deleting either limiter turns a test red (card_971ab86e0eaf).
    let router = match rate_limiter {
        Some(limiter) => router
            .layer(axum::middleware::from_extractor::<
                axum::extract::ConnectInfo<std::net::SocketAddr>,
            >())
            .layer(axum::middleware::from_fn_with_state(
                limiter.clone(),
                rate_limit::rate_limit_middleware,
            )),
        None => router,
    };

    // Maintenance remains the outermost behaviour gate, so it answers before
    // the request-id layer and its rejection body carries no `request_id`.
    let router = router.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        middleware::maintenance_middleware,
    ));

    // The last layer runs first and therefore sees every response, including a
    // 503 or 429 produced by the gates above. It also inserts the CSP nonce into
    // request extensions before forwarding, so the deeper SPA fallback still
    // receives exactly the nonce later written into the response header.
    router.layer(axum::middleware::from_fn(
        security::security_headers_middleware,
    ))
}

/// What answers a path no route claims: the static assets, then the SPA shell.
///
/// Shared by both routers for the same reason [`apply_middleware`] is. While
/// this belonged to production alone, the test router had no fallback at all
/// and inherited the docs sub-router's — so the two answered unmatched paths
/// differently, and the one production behaviour worth reproducing in a test
/// was the one no test could see: an unmatched path does not 404 here, it
/// returns the SPA's `index.html`, which is how a lost package-registry route
/// hands `pip` a page of HTML instead of an error (card_dd8497e4fd58).
///
/// With no bundle on disk — the ordinary test harness — `spa_index_handler`
/// answers 404 for a missing `index.html` and 500 for an unreadable one, so a
/// test that wants the production answer injects a fixture through
/// `AppState::spa_build_dir`.
fn with_spa_fallback(router: Router<AppState>, state: &AppState) -> Router<AppState> {
    let spa_build_dir = state.spa_build_dir.as_ref().clone();
    // Serves `index.html` with a per-request CSP nonce injected into every
    // `<script>` tag (H-2).
    let spa_fallback = axum::routing::get(handlers::spa_index_handler).layer(axum::Extension(
        handlers::SpaBuildDir(spa_build_dir.clone()),
    ));

    router.fallback_service(ServeDir::new(spa_build_dir).fallback(spa_fallback))
}

/// Nest every sub-router at the prefix its [`RouteFact`]s were recorded with.
///
/// Production and test routers differ in their middleware stack, never in their
/// route set — so the mounting lives in one place and both call it.
///
/// The OCI router is merged, not nested, and carries its `/v2` in full in every
/// path: a nested router cannot serve its own prefix *with* a trailing slash,
/// and `GET /v2/` is the spec's version check — see [`build_v2_routes`].
fn assemble(routers: &Routers) -> Router<AppState> {
    Router::new()
        .nest("/git", routers.git.clone())
        .nest("/api/v1", routers.api_v1.clone())
        .merge(routers.v2.clone())
        .merge(routers.root.clone())
        .merge(routers.docs.clone())
}

/// Build the OCI Distribution v2 routes (Docker/OCI container registry).
///
/// The registry authenticates with its own bearer tokens and answers in its own
/// error envelope, so every route here is signed off as [`OCI_TOKEN`].
///
/// Every path is written out in full, `/v2` included, and the table is merged
/// rather than nested. Two of the spec's endpoints end in a slash — `GET /v2/`
/// (end-1, the version check) and `POST /v2/<name>/blobs/uploads/` (end-4a, the
/// start of every push) — and that is the spelling docker, podman and
/// containerd send. Under `nest`, axum 0.8 answers the prefix *without* the
/// trailing slash: an inner `"/"` route serves `/v2` and 404s `/v2/`. There is
/// no path-normalizing layer in front of the router to absorb the difference,
/// and in production the 404 is worse than it sounds — the request falls
/// through to the SPA fallback, so a registry client is handed HTML.
///
/// Nesting would also make the recorded [`RouteFact`] a lie: the fact would
/// read `/v2/` while the router served `/v2`, and the table is what the access
/// sweep and the contract checks read.
fn build_v2_routes(state: &AppState) -> (Router<AppState>, Vec<RouteFact>) {
    // 10 GiB body limit for blob upload requests.
    let upload_limit = Wrap::body_limit(10 * 1024 * 1024 * 1024);

    let (router, facts) = RouteTable::new("")
        // API version check
        // The spec's version check: answers `401` with a
        // `WWW-Authenticate` challenge to an anonymous client, which is how a
        // registry client discovers where to get its token. Clients send the
        // trailing-slash form; the bare one is served too so a hand-typed URL
        // or a proxy that strips the slash still reaches the registry.
        .get(OCI_DISCOVERY, "/v2/", oci::api_version_check)
        .get(OCI_DISCOVERY, "/v2", oci::api_version_check)
        // Token authentication
        // Advertised as the `realm` of the challenge above — the two have to
        // stay the same path.
        .get(Public, "/v2/auth/token", oci::get_token)
        // Tags
        .get(OCI_TOKEN, "/v2/{owner}/{repo}/tags/list", oci::list_tags)
        // Manifests
        .get(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/manifests/{reference}",
            oci::get_manifest,
        )
        .head(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/manifests/{reference}",
            oci::head_manifest,
        )
        .put(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/manifests/{reference}",
            oci::put_manifest,
        )
        // Blobs
        .get(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/blobs/{digest}",
            oci::get_blob,
        )
        .head(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/blobs/{digest}",
            oci::head_blob,
        )
        // Uploads (with body size limit)
        .post_with(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/blobs/uploads/",
            oci::start_upload,
            &upload_limit,
        )
        .post_with(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/blobs/uploads",
            oci::start_upload,
            &upload_limit,
        )
        .patch_with(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/blobs/uploads/{uuid}",
            oci::chunk_upload,
            &upload_limit,
        )
        .get(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/blobs/uploads/{uuid}",
            oci::get_upload_status,
        )
        .put_with(
            OCI_TOKEN,
            "/v2/{owner}/{repo}/blobs/uploads/{uuid}",
            oci::complete_upload,
            &upload_limit,
        )
        .finish();

    (router.with_state(state.clone()), facts)
}

/// Build API docs routes with authentication required.
///
/// Layers are attached per route rather than to the sub-router. `Router::layer`
/// wraps a router's *fallback* along with its routes, and this sub-router is
/// merged into the tree — so back when the docs gate was a layer, the layered
/// fallback became the answer for every path no route claims, and an unmatched
/// URL replied `401 api docs requires authentication` instead of a 404
/// (card_dd8497e4fd58). In production the SPA fallback hid it; in the test
/// router, which had no fallback of its own, a missing route looked like an
/// authorization problem to anyone debugging one. The gate has since moved into
/// the handlers' signatures, but the shape stays: a layer here is one this
/// sub-router's four routes carry, not one the whole tree inherits.
fn build_docs_routes(
    state: &AppState,
    api_facts: &[RouteFact],
) -> (Router<AppState>, Vec<RouteFact>) {
    // The document is built here, once, because this is where the route table
    // exists: every operation's `security` is derived from the access level its
    // route declares (`openapi::stamp_security`) rather than written a third
    // time by hand. Serving it from `Bytes` keeps the per-request cost to an
    // Arc bump instead of re-rendering 287 operations.
    let (spec, stamp) = openapi::spec_json(api_facts);
    if stamp.unresolved.is_empty() {
        tracing::debug!(
            required = stamp.required,
            optional = stamp.optional,
            anonymous = stamp.anonymous,
            "openapi: security derived from the route table"
        );
    } else {
        tracing::error!(
            unresolved = stamp.unresolved.len(),
            operations = ?stamp.unresolved,
            "openapi: operations advertised at URLs no route table row matches. They are \
             published as requiring a session — the safe direction — but the document and the \
             router disagree about those URLs, so their access levels are guesses"
        );
    }
    let spec = axum::body::Bytes::from(spec);

    // The gate itself is `AuthUser`, taken by each handler — so these rows
    // declare `User` and are driven by the persona sweep like any other
    // authenticated route. They used to declare `Foreign(Middleware)` over a
    // `docs_auth_middleware` that read `Authorization: Bearer` and nothing
    // else, which bought the sweep's exemption *and* locked every browser out
    // of the one surface built for browsers (card_fb094ba6d323).
    //
    // What is left here is the PAT translation, which is not a gate: it turns a
    // Personal Access Token into the Bearer JWT `AuthUser` understands, exactly
    // as the same layer does for `/api/v1`. A request with no credentials, or
    // with a session cookie, passes through it untouched and is answered by the
    // handler's own gate.
    let pat_bridge = Wrap::plain(|mr: MethodRouter<AppState>| -> MethodRouter<AppState> {
        mr.layer(axum::middleware::from_fn_with_state(
            state.clone(),
            pat_auth::pat_auth_middleware,
        ))
    });

    // The same PAT bridge, plus the document itself. Only this one route reads
    // it, so only this one route carries it.
    let spec_bridge = Wrap::plain(|mr: MethodRouter<AppState>| -> MethodRouter<AppState> {
        mr.layer(axum::Extension(handlers::OpenApiSpec(spec.clone())))
            .layer(axum::middleware::from_fn_with_state(
                state.clone(),
                pat_auth::pat_auth_middleware,
            ))
    });

    let (router, facts) = RouteTable::new("")
        .get_with(
            User,
            "/api-docs/openapi.json",
            handlers::openapi_handler,
            &spec_bridge,
        )
        .get_with(
            User,
            "/api-docs",
            handlers::swagger_ui_root_handler,
            &pat_bridge,
        )
        .get_with(
            User,
            "/api-docs/",
            handlers::swagger_ui_root_handler,
            &pat_bridge,
        )
        .get_with(
            User,
            "/api-docs/{*tail}",
            handlers::swagger_ui_handler,
            &pat_bridge,
        )
        .finish();

    (router.with_state(state.clone()), facts)
}

/// The Maven repository layout, registered once per `groupId` depth.
///
/// Maven writes a `groupId` with one path segment per dot: `mvn` and Gradle ask
/// for `com.example:matrix-maven` at
/// `.../packages/maven/com/example/matrix-maven/…`, never at
/// `.../maven/com.example/matrix-maven/…`. Two shapes carry the whole read side
/// of the protocol:
///
/// - `<group…>/<artifact>/maven-metadata.xml` — the version list;
/// - `<group…>/<artifact>/<version>/<file>` — the artifact itself.
///
/// Axum matches a fixed number of segments, and a catch-all (`{*path}`) is not
/// an option here: it would sit above `POST .../packages/maven/publish` and the
/// rest of the generic `{pkg_type}` package API, which would then answer `405`.
/// So each shape is registered once per group depth instead, up to
/// [`MAVEN_MAX_GROUP_SEGMENTS`] — two past the longest groups in the wild
/// (`com.fasterxml.jackson.core` is four).
///
/// Depth 1 doubles as the flat spelling ForgeKeep's own API and UI use: a single
/// segment that already carries the dots joins back to the same `groupId`, so
/// `.../maven/com.example/matrix-maven/maven-metadata.xml` keeps working.
///
/// Two known edges, both harmless and both a consequence of matching on shape:
/// a SNAPSHOT's `<group…>/<artifact>/<version>/maven-metadata.xml` matches the
/// metadata shape (the static filename wins over `{m…}`) and answers an empty
/// version list, and a stored file name containing a `/` is no longer reachable
/// under `maven/` through the generic `{*file}` route. ForgeKeep publishes
/// neither.
fn maven_layout_routes(table: RouteTable) -> RouteTable {
    const METADATA: [&str; MAVEN_MAX_GROUP_SEGMENTS] = [
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/maven-metadata.xml",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/maven-metadata.xml",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/maven-metadata.xml",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/{m5}/maven-metadata.xml",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/{m5}/{m6}/maven-metadata.xml",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/{m5}/{m6}/{m7}/maven-metadata.xml",
    ];
    const FILES: [&str; MAVEN_MAX_GROUP_SEGMENTS] = [
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/{m5}",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/{m5}/{m6}",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/{m5}/{m6}/{m7}",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/{m5}/{m6}/{m7}/{m8}",
        "/repos/{owner}/{name}/packages/maven/{m1}/{m2}/{m3}/{m4}/{m5}/{m6}/{m7}/{m8}/{m9}",
    ];

    let mut table = table;
    for path in METADATA {
        table = table.get(RepoRead, path, api::packages::maven_metadata);
        // `mvn deploy` uploads its own `maven-metadata.xml` alongside the
        // artifacts. Registering the verb here is what keeps the deploy from
        // failing on a document the registry derives for itself — see
        // `maven_upload_metadata`, which accepts it without storing it.
        table = table.put(RepoWrite, path, api::packages::maven_upload_metadata);
    }
    for path in FILES {
        table = table.get(RepoRead, path, api::packages::maven_download);
        // The deploy half of the same layout: Maven PUTs each file to the very
        // URL its resolver will later GET (card_11d8655a9cd8).
        table = table.put(RepoWrite, path, api::packages::maven_upload);
    }
    table
}

/// How many segments a `groupId` may span — the number of variants of each
/// Maven shape [`maven_layout_routes`] registers.
const MAVEN_MAX_GROUP_SEGMENTS: usize = 6;

/// The Cargo sparse index (RFC 2789), registered once per prefix depth.
///
/// The index root is `.../packages/cargo/index/` — that whole URL, with a
/// `sparse+` scheme, is what a user writes into `.cargo/config.toml`. Two
/// shapes hang off it, and Cargo fetches the first one before any crate:
///
/// - `config.json` — where to download a `.crate` from. A sparse index without
///   it is not a registry, and the request used to fall through to the SPA
///   fallback, handing Cargo HTML.
/// - `{prefix…}/{crate}` — the version list. Cargo never asks for the bare
///   name: it spells the name out as a directory prefix (`1/a`, `2/ab`,
///   `3/a/abc`, `se/rd/serde`), so a crate path is two or three segments and
///   the single `{pkg}` this used to be matched none of them.
///
/// As in [`maven_layout_routes`], a catch-all is not an option — it would sit
/// above the generic `{pkg_type}` package API — so each depth is registered
/// instead. Depth 1 is ForgeKeep's own flat spelling (`index/{crate}`), which
/// its API and UI use; the layout never produces a single segment, so the two
/// cannot collide, and `config.json` is static and so wins over `{c1}`.
fn cargo_index_routes(table: RouteTable) -> RouteTable {
    const INDEX: [&str; 3] = [
        "/repos/{owner}/{name}/packages/cargo/index/{c1}",
        "/repos/{owner}/{name}/packages/cargo/index/{c1}/{c2}",
        "/repos/{owner}/{name}/packages/cargo/index/{c1}/{c2}/{c3}",
    ];

    let mut table = table
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/cargo/index/config.json",
            api::packages::cargo_index_config,
        )
        // The write API, in the shape cargo derives from `config.json`'s `api`
        // key. Reading the index worked long before any of this existed, so
        // `cargo publish` had no route to reach at all (card_5a790cc6ac35).
        .put(
            RepoWrite,
            "/repos/{owner}/{name}/packages/cargo/api/v1/crates/new",
            api::packages::cargo_publish_new,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/yank",
            api::packages::cargo_yank,
        )
        .put(
            RepoWrite,
            "/repos/{owner}/{name}/packages/cargo/api/v1/crates/{crate_name}/{version}/unyank",
            api::packages::cargo_unyank,
        );
    for path in INDEX {
        table = table.get(RepoRead, path, api::packages::cargo_sparse_index);
    }
    table
}

/// The RubyGems compact index, and the one download path a client derives.
///
/// The registry root is `.../packages/rubygems/` — that URL is what goes into
/// `gem install --source` or a `Gemfile`. What hangs off it is not a matter of
/// taste: `Gem::Source` asks for `versions` before anything else and reads the
/// answer as a protocol choice. Served, and it resolves through `info/{gem}`;
/// missing, and it falls back to the legacy Marshal index (`specs.4.8.gz`,
/// `quick/Marshal.4.8/…`), which ForgeKeep does not serve either — so the
/// client's next stop is a 404 with no explanation.
///
/// `gems/{file}` is not advertised anywhere and does not need to be: the client
/// appends it to the source URL on its own. That makes it the only path a
/// `gem_uri` can point at, and the reason the old one — `{base}/gems/…`, at the
/// instance root, where the SPA lives — handed `gem` an HTML page.
///
/// One edge, the same shape as the ones [`maven_layout_routes`] documents: a
/// gem named `versions`, `names`, `info` or `gems` shadows part of ForgeKeep's
/// own `{pkg_type}/{pkg_name}` API for that one name. RubyGems has no such gem,
/// and the protocol spelling is the one a client cannot be asked to give up.
fn rubygems_protocol_routes(table: RouteTable) -> RouteTable {
    table
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/rubygems/versions",
            api::packages::rubygems_compact_versions,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/rubygems/info/{gem_name}",
            api::packages::rubygems_compact_info,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/rubygems/names",
            api::packages::rubygems_compact_names,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/rubygems/gems/{filename}",
            api::packages::rubygems_gem_download,
        )
        // The write side. `gem push` derives this URL from the same `--host`
        // the read routes above hang off, and sends the `.gem` as the body —
        // there was no route under it to reach at all, so a gem could be
        // installed from ForgeKeep but never pushed to it (card_11a578ae1820).
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/packages/rubygems/api/v1/gems",
            api::packages::rubygems_push,
        )
}

/// Build every route the server serves, and the access level of each.
///
/// `auth_rate_limiter` is layered only onto the unauthenticated credential
/// endpoints (`/users/register`, `/users/login`). The test router passes
/// `None` so those routes carry no extra layer: the limiter middleware extracts
/// `ConnectInfo`, which the test harness (plain `oneshot`, no
/// `into_make_service_with_connect_info`) does not provide. That the two routes
/// really do carry it in production is covered by `rate_limit_mounted_tests`,
/// which builds this router with `Some(..)` and a budget of two.
pub(crate) fn build_all_routes(
    state: &AppState,
    auth_rate_limiter: Option<&rate_limit::RateLimiter>,
) -> Routers {
    // Stricter per-route limiter for the credential endpoints, keyed by the
    // same client-IP resolution as the global limiter. `layer()` returns the
    // same `MethodRouter<AppState>` type in both arms, so the attach-or-not
    // choice stays type-consistent.
    let auth_rl = Wrap::plain(|mr: MethodRouter<AppState>| -> MethodRouter<AppState> {
        match auth_rate_limiter {
            Some(limiter) => mr.layer(axum::middleware::from_fn_with_state(
                limiter.clone(),
                rate_limit::rate_limit_middleware,
            )),
            None => mr,
        }
    });
    // The runner token check. Applied per route rather than to a sub-router so
    // that every route still passes through the one table that declares it —
    // and built by the constructor that owns the layer's name, so a route
    // recorded as carrying `authenticate_runner` carries it.
    let runner_auth = Wrap::runner_auth(state);
    // The cache upload is both: a runner credential and a raised body limit.
    // It stays a credential wrapper — the limit rides along, it does not
    // replace the check the route declares.
    let runner_auth_1gb = Wrap::runner_auth_with_body_limit(state, 1024 * 1024 * 1024);
    // Raised body limits for the routes that carry an upload. Only the
    // body-carrying method of a resource takes one; a limit on its `GET`
    // sibling never applied to anything.
    let limit_101mb = Wrap::body_limit(101 * 1024 * 1024);
    let limit_10gb = Wrap::body_limit(10 * 1024 * 1024 * 1024);
    // Multipart package clients add framing around the artifact (and npm adds
    // base64 JSON on its own route). Lift Axum's hidden 2 MiB extractor limit
    // to a bounded envelope allowance; `publish_package` independently checks
    // the decoded artifact against the configured real ceiling.
    let package_envelope_limit =
        api::packages::package_upload_envelope_limit(state.package_upload_max_bytes);
    let package_envelope = Wrap::body_limit(package_envelope_limit);

    // ── Git Smart HTTP routes ──────────────────────────────────────────────
    let (git, git_facts) = RouteTable::new("/git")
        .get(
            GIT_HTTP,
            "/{owner}/{repo}/info/refs",
            git_http::handle_info_refs,
        )
        .post(
            GIT_HTTP,
            "/{owner}/{repo}/git-upload-pack",
            git_http::handle_git_upload_pack,
        )
        .post(
            GIT_HTTP,
            "/{owner}/{repo}/git-receive-pack",
            git_http::handle_git_receive_pack,
        )
        .finish();

    // ── Root-level routes ──────────────────────────────────────────────────
    // Git clients request `/{owner}/{repo}.git/info/refs` etc. — these must be
    // at root level (no `/git` prefix) for compatibility.
    let (root, root_facts) = RouteTable::new("")
        .get(
            GIT_HTTP,
            "/{owner}/{repo}/info/refs",
            git_http::handle_info_refs,
        )
        .post(
            GIT_HTTP,
            "/{owner}/{repo}/git-upload-pack",
            git_http::handle_git_upload_pack,
        )
        .post(
            GIT_HTTP,
            "/{owner}/{repo}/git-receive-pack",
            git_http::handle_git_receive_pack,
        )
        .get(Public, "/health", handlers::health)
        .get(Public, "/metrics", metrics::metrics_handler)
        .finish();

    // ── REST API routes ────────────────────────────────────────────────────
    let (api_v1, api_facts) = RouteTable::new("/api/v1")
        // ── Instance ───────────────────────────────────────────────────────
        // Public on purpose: the banner announces maintenance to the people it
        // will affect, and behind the admin gate it reached none of them
        // (card_801b8bcdb880).
        .get(Public, "/instance", api::instance::get_instance)
        // ── Users ──────────────────────────────────────────────────────────
        .post_with(Public, "/users/register", api::users::register, &auth_rl)
        .post_with(Public, "/users/login", api::users::login, &auth_rl)
        .post(User, "/users/logout", api::users::logout)
        .get(User, "/users/me", api::users::me)
        .post(
            Public,
            "/users/forgot-password",
            api::users::forgot_password,
        )
        .post(Public, "/users/reset-password", api::users::reset_password)
        // PAT
        .get(User, "/users/tokens", api::users::list_tokens)
        .post(User, "/users/tokens", api::users::create_token)
        .delete(User, "/users/tokens/{id}", api::users::delete_token)
        // SSH keys
        .get(User, "/users/ssh-keys", api::ssh_keys::list_ssh_keys)
        .post(User, "/users/ssh-keys", api::ssh_keys::create_ssh_key)
        .delete(User, "/users/ssh-keys/{id}", api::ssh_keys::delete_ssh_key)
        // Deploy keys
        .get(
            RepoAdmin,
            "/repos/{owner}/{name}/keys",
            api::deploy_keys::list_deploy_keys,
        )
        .post(
            RepoAdmin,
            "/repos/{owner}/{name}/keys",
            api::deploy_keys::create_deploy_key,
        )
        .delete(
            RepoAdmin,
            "/repos/{owner}/{name}/keys/{id}",
            api::deploy_keys::delete_deploy_key,
        )
        // MFA
        .post(User, "/users/mfa/setup", api::mfa::setup_mfa)
        .post(User, "/users/mfa/enable", api::mfa::enable_mfa)
        // The second factor of a login: the caller has a password but no
        // session yet, so this one is reachable without a token by design.
        .post(Public, "/users/mfa/verify", api::mfa::verify_mfa)
        .get(User, "/users/mfa/backup", api::mfa::get_backup_codes)
        .post(
            User,
            "/users/mfa/backup/regenerate",
            api::mfa::regenerate_backup_codes,
        )
        .post(User, "/users/mfa/disable", api::mfa::disable_mfa)
        // Passkeys (WebAuthn)
        .get(User, "/users/passkeys", api::passkeys::list_passkeys)
        .delete(User, "/users/passkeys/{id}", api::passkeys::delete_passkey)
        .post(
            User,
            "/users/passkeys/register/start",
            api::passkeys::register_start,
        )
        .post(
            User,
            "/users/passkeys/register/finish",
            api::passkeys::register_finish,
        )
        .post(
            Public,
            "/users/passkeys/login/start",
            api::passkeys::login_start,
        )
        .post(
            Public,
            "/users/passkeys/login/finish",
            api::passkeys::login_finish,
        )
        // SSO
        .get(Public, "/auth/sso/providers", api::sso::list_providers)
        .get(Public, "/auth/sso/{slug}", api::sso::authorize)
        .get(Public, "/auth/sso/{slug}/callback", api::sso::callback)
        .post(User, "/auth/sso/{slug}/refresh", api::sso::refresh_token)
        .delete(
            User,
            "/auth/sso/{slug}/unlink",
            api::sso::unlink_oauth_account,
        )
        // ── Repositories ───────────────────────────────────────────────────
        .post(User, "/repos", api::repos::create_repo)
        // Template listing & explore (must be before /repos/{owner} to avoid
        // route conflict)
        .get(
            Public,
            "/repos/templates/gitignores",
            api::repos::list_gitignore_templates,
        )
        .get(
            Public,
            "/repos/templates/licenses",
            api::repos::list_license_templates,
        )
        .get(
            Public,
            "/repos/templates/readmes",
            api::repos::list_readme_templates,
        )
        .get(
            Public,
            "/repos/templates/labels",
            api::repos::list_label_sets,
        )
        .get(PublicFiltered, "/repos/explore", api::repos::explore)
        .get(PublicFiltered, "/repos/{owner}", api::repos::list_repos)
        .get(RepoRead, "/repos/{owner}/{name}", api::repos::get_repo)
        .delete(
            RepoOwner,
            "/repos/{owner}/{name}",
            api::repos::delete_repo_handler,
        )
        // Milestones (before issues to avoid routing conflicts)
        .get(
            RepoRead,
            "/repos/{owner}/{name}/milestones",
            api::issues::list_milestones,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/milestones",
            api::issues::create_milestone,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/milestones/{id}",
            api::issues::get_milestone,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/milestones/{id}",
            api::issues::update_milestone,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/milestones/{id}",
            api::issues::delete_milestone,
        )
        // Labels
        .get(
            RepoRead,
            "/repos/{owner}/{name}/labels",
            api::labels::list_labels,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/labels",
            api::labels::create_label,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/labels/{id}",
            api::labels::get_label,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/labels/{id}",
            api::labels::update_label,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/labels/{id}",
            api::labels::delete_label,
        )
        // Issues
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issue_templates",
            api::issues::list_issue_templates,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issue_config",
            api::issues::get_issue_config,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issue_config/validate",
            api::issues::validate_issue_config,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pull_request_template",
            api::issues::get_pull_request_template,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues",
            api::issues::list_issues,
        )
        .post(
            RepoAuthRead,
            "/repos/{owner}/{name}/issues",
            api::issues::create_issue,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/{number}",
            api::issues::get_issue,
        )
        // The issue's own author may edit it with read access; `RepoWrite` is
        // what a caller who is not the author needs, which is the level a
        // stranger is measured against.
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/issues/{number}",
            api::issues::update_issue,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/{number}/labels",
            api::issues::get_issue_labels,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/{number}/comments",
            api::issues::list_comments,
        )
        .post(
            RepoAuthRead,
            "/repos/{owner}/{name}/issues/{number}/comments",
            api::issues::add_comment,
        )
        // Attachments. Uploading or deleting one takes write access unless the
        // caller authored its target or uploaded it, so `RepoWrite` is the
        // level a stranger is measured against.
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/{number}/assets",
            api::attachments::list_issue_attachments,
        )
        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/issues/{number}/assets",
            api::attachments::create_issue_attachment,
            &limit_101mb,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}",
            api::attachments::get_issue_attachment,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/issues/{number}/assets/{attachment_id}",
            api::attachments::delete_issue_attachment,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/comments/{comment_id}/assets",
            api::attachments::list_issue_comment_attachments,
        )
        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/issues/comments/{comment_id}/assets",
            api::attachments::create_issue_comment_attachment,
            &limit_101mb,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}",
            api::attachments::get_issue_comment_attachment,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/issues/comments/{comment_id}/assets/{attachment_id}",
            api::attachments::delete_issue_comment_attachment,
        )
        // ── Pull requests ──────────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls",
            api::pulls::list_prs,
        )
        .post(
            RepoAuthRead,
            "/repos/{owner}/{name}/pulls",
            api::pulls::create_pr,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}",
            api::pulls::get_pr,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}",
            api::pulls::update_pr,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}/assets",
            api::attachments::list_pull_request_attachments,
        )
        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/assets",
            api::attachments::create_pull_request_attachment,
            &limit_101mb,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}",
            api::attachments::get_pull_request_attachment,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/assets/{attachment_id}",
            api::attachments::delete_pull_request_attachment,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets",
            api::attachments::list_review_comment_attachments,
        )
        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets",
            api::attachments::create_review_comment_attachment,
            &limit_101mb,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}",
            api::attachments::get_review_comment_attachment,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/comments/{comment_id}/assets/{attachment_id}",
            api::attachments::delete_review_comment_attachment,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}/diff",
            api::pulls::get_diff,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/merge",
            api::pulls::merge_pr,
        )
        // `RepoWrite` on purpose: this is the maintainer saying an unreviewed
        // fork head may run with this repository's CI secrets, so the author of
        // the PR must not be able to grant it to themselves (card_94834ecee708).
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/ci-approval",
            api::pulls::approve_pr_ci,
        )
        .put(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/auto-merge",
            api::pulls::enable_auto_merge,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/auto-merge",
            api::pulls::disable_auto_merge,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/merge-queue",
            api::pulls::list_merge_queue,
        )
        .put(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/merge-queue",
            api::pulls::enqueue_merge_queue,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/merge-queue",
            api::pulls::cancel_merge_queue,
        )
        // PR reviews
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}/reviews",
            api::reviews::list_reviews,
        )
        .post(
            RepoAuthRead,
            "/repos/{owner}/{name}/pulls/{number}/reviews",
            api::reviews::submit_review,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}/reviews/{id}",
            api::reviews::get_review,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/reviews/{id}/dismiss",
            api::reviews::dismiss_review,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}/comments",
            api::reviews::list_review_comments,
        )
        .post(
            RepoAuthRead,
            "/repos/{owner}/{name}/pulls/{number}/comments",
            api::reviews::create_review_comment,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}/timeline",
            api::reviews::get_review_timeline,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/comments/{id}/resolution",
            api::reviews::set_thread_resolution,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/comments/{id}/suggestion/apply",
            api::reviews::apply_review_suggestion,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/suggestions/apply",
            api::reviews::apply_review_suggestions,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pulls/{number}/reviewers",
            api::reviews::list_requested_reviewers,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/reviewers",
            api::reviews::request_reviewer,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/pulls/{number}/reviewers/{username}",
            api::reviews::remove_requested_reviewer,
        )
        // ── Wiki ───────────────────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/wiki",
            api::wiki::list_pages,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/wiki",
            api::wiki::create_page,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/wiki/{title}",
            api::wiki::get_page,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/wiki/{title}",
            api::wiki::update_page,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/wiki/{title}",
            api::wiki::delete_page,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/wiki/{title}/history",
            api::wiki::list_revisions,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/wiki/{title}/revisions/{rev_id}",
            api::wiki::get_revision,
        )
        // ── Git LFS ────────────────────────────────────────────────────────
        .post(
            LFS_PROTOCOL,
            "/repos/{owner}/{name}/lfs/objects/batch",
            api::lfs::batch,
        )
        .get(
            LFS_PROTOCOL,
            "/repos/{owner}/{name}/lfs/objects/{oid}",
            api::lfs::download_object,
        )
        .put_with(
            LFS_PROTOCOL,
            "/repos/{owner}/{name}/lfs/objects/{oid}",
            api::lfs::upload_object,
            &limit_10gb,
        )
        // ── Webhooks ───────────────────────────────────────────────────────
        .get(
            RepoAdmin,
            "/repos/{owner}/{name}/hooks",
            api::webhooks::list_webhooks,
        )
        .post(
            RepoAdmin,
            "/repos/{owner}/{name}/hooks",
            api::webhooks::create_webhook,
        )
        .get(
            RepoAdmin,
            "/repos/{owner}/{name}/hooks/{id}",
            api::webhooks::get_webhook,
        )
        .patch(
            RepoAdmin,
            "/repos/{owner}/{name}/hooks/{id}",
            api::webhooks::update_webhook,
        )
        .delete(
            RepoAdmin,
            "/repos/{owner}/{name}/hooks/{id}",
            api::webhooks::delete_webhook,
        )
        .get(
            RepoAdmin,
            "/repos/{owner}/{name}/hooks/{id}/deliveries",
            api::webhooks::list_deliveries,
        )
        .post(
            RepoAdmin,
            "/repos/{owner}/{name}/hooks/{id}/deliveries/{delivery_id}/redeliver",
            api::webhooks::redeliver,
        )
        // ── CI/CD pipelines ────────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pipelines",
            api::ci::list_pipelines,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pipelines",
            api::ci::trigger_pipeline,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pipelines/{id}",
            api::ci::get_pipeline,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pipelines/{id}/retry",
            api::ci::retry_pipeline,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pipelines/{id}/cancel",
            api::ci::cancel_pipeline,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}",
            api::ci::get_job,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/pipelines/{id}/jobs/{job_id}/play",
            api::ci::play_job,
        )
        .post(
            RepoAuthRead,
            "/repos/{owner}/{name}/pipelines/{pipeline_id}/jobs/{job_id}/approve",
            api::ci_environments::approve,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/actions/environments",
            api::ci_environments::list,
        )
        .post(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/environments",
            api::ci_environments::create,
        )
        .put(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/environments/{id}",
            api::ci_environments::update,
        )
        .delete(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/environments/{id}",
            api::ci_environments::delete,
        )
        .get(
            Public,
            "/ci/oidc/.well-known/openid-configuration",
            api::ci_oidc::discovery,
        )
        .get(Public, "/ci/oidc/jwks", api::ci_oidc::jwks)
        .get(CI_JOB_TOKEN, "/ci/oidc/token", api::ci_oidc::token)
        .get(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/retention",
            api::ci_retention::get_policy,
        )
        .put(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/retention",
            api::ci_retention::update_policy,
        )
        .delete(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/retention/expired",
            api::ci_retention::cleanup,
        )
        .get(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/secrets",
            api::ci_secrets::list,
        )
        .put(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/secrets/{secret_name}",
            api::ci_secrets::put,
        )
        .delete(
            RepoAdmin,
            "/repos/{owner}/{name}/actions/secrets/{secret_name}",
            api::ci_secrets::delete,
        )
        // Repository archive download
        .get(
            RepoRead,
            "/repos/{owner}/{name}/archive/{archive}",
            api::archive::download_archive,
        )
        // ── Branch and tag protection ──────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/branches/protection",
            api::branch_protection::list_protections,
        )
        .post(
            RepoAdmin,
            "/repos/{owner}/{name}/branches/protection",
            api::branch_protection::create_protection,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/branches/protection/{id}",
            api::branch_protection::get_protection,
        )
        .patch(
            RepoAdmin,
            "/repos/{owner}/{name}/branches/protection/{id}",
            api::branch_protection::update_protection,
        )
        .delete(
            RepoAdmin,
            "/repos/{owner}/{name}/branches/protection/{id}",
            api::branch_protection::delete_protection,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/tags/protection",
            api::tag_protection::list,
        )
        .post(
            RepoAdmin,
            "/repos/{owner}/{name}/tags/protection",
            api::tag_protection::create,
        )
        .patch(
            RepoAdmin,
            "/repos/{owner}/{name}/tags/protection/{id}",
            api::tag_protection::update,
        )
        .delete(
            RepoAdmin,
            "/repos/{owner}/{name}/tags/protection/{id}",
            api::tag_protection::delete,
        )
        // ── Collaborators ──────────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/collaborators",
            api::collaborators::list_collaborators,
        )
        .post(
            RepoAdmin,
            "/repos/{owner}/{name}/collaborators",
            api::collaborators::add_collaborator,
        )
        .patch(
            RepoAdmin,
            "/repos/{owner}/{name}/collaborators/{id}",
            api::collaborators::update_permission,
        )
        .delete(
            RepoAdmin,
            "/repos/{owner}/{name}/collaborators/{id}",
            api::collaborators::remove_collaborator,
        )
        // ── Repository content ─────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/tree",
            api::repo_content::list_tree,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/blob/{*path}",
            api::repo_content::get_blob,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/log",
            api::repo_content::get_log,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/branches",
            api::repo_content::list_branches,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/tags",
            api::repo_content::list_tags,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/commits/{sha}/signature",
            api::repo_content::get_commit_signature,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/contents/{*path}",
            api::repo_content::create_or_update_file,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/contents/{*path}",
            api::repo_content::delete_file,
        )
        // ── Mirroring ──────────────────────────────────────────────────────
        .get(
            RepoWrite,
            "/repos/{owner}/{name}/mirror",
            api::mirrors::get_mirror,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/mirror",
            api::mirrors::create_mirror,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/mirror",
            api::mirrors::update_mirror,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/mirror",
            api::mirrors::delete_mirror,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/mirror/sync",
            api::mirrors::trigger_mirror_sync,
        )
        // Imports (GitHub/GitLab migration)
        .post(User, "/imports", api::imports::start_import)
        .get(User, "/imports", api::imports::list_imports)
        .get(User, "/imports/{id}", api::imports::get_import_status)
        .delete(User, "/imports/{id}", api::imports::delete_import)
        // ── Project boards ─────────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/boards",
            api::boards::list_boards,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/boards",
            api::boards::create_board,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/boards/{id}",
            api::boards::get_board,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}",
            api::boards::update_board,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}",
            api::boards::delete_board,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}/columns",
            api::boards::create_column,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}/columns/{col_id}",
            api::boards::update_column,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}/columns/{col_id}",
            api::boards::delete_column,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}/columns/{col_id}/cards",
            api::boards::create_card,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}/cards/{card_id}",
            api::boards::update_card,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}/cards/{card_id}",
            api::boards::delete_card,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}/cards/{card_id}/move",
            api::boards::move_card,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/boards/{id}/cards/reorder",
            api::boards::reorder_cards,
        )
        // ── Time tracking ──────────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/{number}/time",
            api::time_tracking::list_time_entries,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/issues/{number}/time",
            api::time_tracking::add_time,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/issues/{number}/time/total",
            api::time_tracking::total_time,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/issues/{number}/time/{id}",
            api::time_tracking::delete_time_entry,
        )
        // ── Commit statuses ────────────────────────────────────────────────
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/statuses/{sha}",
            api::repos::create_commit_status,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/commits/{sha}/statuses",
            api::repos::list_commit_statuses,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/commits/{sha}/status",
            api::repos::get_combined_status,
        )
        // ── Organizations ──────────────────────────────────────────────────
        .get(User, "/orgs", api::orgs::list_orgs)
        .post(User, "/orgs", api::orgs::create_org)
        .get(OrgRead, "/orgs/{name}", api::orgs::get_org)
        .patch(OrgAdmin, "/orgs/{name}", api::orgs::update_org)
        .delete(OrgAdmin, "/orgs/{name}", api::orgs::delete_org)
        .get(OrgRead, "/orgs/{name}/members", api::orgs::list_org_members)
        .post(OrgAdmin, "/orgs/{name}/members", api::orgs::add_org_member)
        .delete(
            OrgAdmin,
            "/orgs/{name}/members/{user_id}",
            api::orgs::remove_org_member,
        )
        .get(OrgRead, "/orgs/{name}/teams", api::orgs::list_org_teams)
        .post(OrgAdmin, "/orgs/{name}/teams", api::orgs::create_team)
        .get(OrgRead, "/orgs/{name}/teams/{team_id}", api::orgs::get_team)
        .delete(
            OrgAdmin,
            "/orgs/{name}/teams/{team_id}",
            api::orgs::delete_team,
        )
        .get(
            OrgRead,
            "/orgs/{name}/teams/{team_id}/members",
            api::orgs::list_team_members,
        )
        .post(
            OrgAdmin,
            "/orgs/{name}/teams/{team_id}/members",
            api::orgs::add_team_member,
        )
        .delete(
            OrgAdmin,
            "/orgs/{name}/teams/{team_id}/members/{user_id}",
            api::orgs::remove_team_member,
        )
        // ── Notifications ──────────────────────────────────────────────────
        .get(
            User,
            "/notifications",
            api::notifications::list_notifications,
        )
        .get(
            User,
            "/notifications/unread-count",
            api::notifications::unread_count,
        )
        .post(
            User,
            "/notifications/mark-all-read",
            api::notifications::mark_all_read,
        )
        .post(
            User,
            "/notifications/{id}/read",
            api::notifications::mark_read,
        )
        .delete(
            User,
            "/notifications/{id}",
            api::notifications::delete_notification,
        )
        // ── Star / watch ───────────────────────────────────────────────────
        .put(
            RepoAuthRead,
            "/repos/{owner}/{name}/star",
            api::repos::star_repo,
        )
        .get(
            RepoAuthRead,
            "/repos/{owner}/{name}/starred",
            api::repos::get_starred_status,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/stargazers",
            api::repos::get_stargazers,
        )
        .get(
            RepoAuthRead,
            "/repos/{owner}/{name}/watch",
            api::repos::get_watch_status,
        )
        .put(
            RepoAuthRead,
            "/repos/{owner}/{name}/watch",
            api::repos::watch_repo,
        )
        .delete(
            RepoAuthRead,
            "/repos/{owner}/{name}/watch",
            api::repos::unwatch_repo,
        )
        // ── Releases ───────────────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/releases",
            api::releases::list_releases,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/releases",
            api::releases::create_release,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/releases/{id}",
            api::releases::get_release,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/releases/{id}",
            api::releases::update_release,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/releases/{id}",
            api::releases::delete_release,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/releases/{release_id}/assets",
            api::releases::list_assets,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/releases/{release_id}/assets",
            api::releases::upload_asset,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/releases/assets/{asset_id}",
            api::releases::get_asset,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/releases/assets/{asset_id}",
            api::releases::delete_asset,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/releases/assets/{asset_id}/download",
            api::releases::download_asset,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/releases/assets/{asset_id}/attestation",
            api::releases::sign_asset_attestation,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/releases/assets/{asset_id}/attestation",
            api::releases::get_asset_attestation,
        )
        .post(
            RepoRead,
            "/repos/{owner}/{name}/releases/assets/{asset_id}/attestation/verify",
            api::releases::verify_asset_attestation,
        )
        // ── Fork / transfer ────────────────────────────────────────────────
        // Forking needs a session and read access to the source, not ownership.
        // The handler takes `RepoAuthRead` and hands the gated source model to
        // `rg_core::repo::service::fork_repo`, which no longer re-decides
        // anything of its own (card_b38bfb0f2b40).
        .post(
            RepoAuthRead,
            "/repos/{owner}/{name}/fork",
            api::repos::fork_repo_handler,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/forks",
            api::repos::list_forks_handler,
        )
        .post(
            RepoOwner,
            "/repos/{owner}/{name}/transfer",
            api::repos::transfer_repo_handler,
        )
        // ── Package registry ───────────────────────────────────────────────
        // Protocol-specific routes first (before the generic catch-all).
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages",
            api::packages::list_registries,
        )
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/packages/{pkg_type}/publish",
            api::packages::publish,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/{pkg_type}/list",
            api::packages::list_packages,
        )
        // Cargo sparse index protocol — see `cargo_index_routes`.
        .with(cargo_index_routes)
        // npm registry protocol
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/packages/npm/publish",
            api::packages::publish_npm,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/npm/list",
            api::packages::list_npm_packages,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/npm/-/npm/v1/attestations/{package_spec}",
            api::packages::npm_attestations,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/npm/{pkg_name}",
            api::packages::npm_registry_metadata,
        )
        .put(
            RepoWrite,
            "/repos/{owner}/{name}/packages/npm/{pkg_name}",
            api::packages::publish_npm_packument,
        )
        // `npm dist-tag` resolves these paths relative to the configured npm
        // registry root. They are separate from the packument route because a
        // tag is mutable while every published npm version is immutable.
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags",
            api::packages::npm_dist_tags,
        )
        .put(
            RepoWrite,
            "/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}",
            api::packages::set_npm_dist_tag,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/packages/npm/-/package/{pkg_name}/dist-tags/{tag}",
            api::packages::delete_npm_dist_tag,
        )
        // Twine's legacy upload API. Both spellings are deliberate: users copy
        // repository URLs with and without the trailing slash, and Twine POSTs
        // to exactly what it was given rather than normalizing the path.
        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/packages/pypi/legacy/",
            api::packages::pypi_legacy_upload,
            &package_envelope,
        )
        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/packages/pypi/legacy",
            api::packages::pypi_legacy_upload,
            &package_envelope,
        )
        // PyPI Simple Repository API (PEP 503)
        //
        // The spec spells both of its URLs with a trailing slash — the root
        // index is `.../simple/` and a project page is `.../simple/<name>/` —
        // and that is what pip, poetry and uv build. Axum matches paths
        // literally, so the slashed spelling has to be registered too: without
        // it the request misses the router, falls through to the SPA fallback,
        // and pip is handed HTML that is not an index. Same failure as `GET
        // /v2/` in [`build_v2_routes`], one directory up.
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/pypi/simple/",
            api::packages::pypi_simple_root_index,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/pypi/simple",
            api::packages::pypi_simple_root_index,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}/",
            api::packages::pypi_simple_index,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/pypi/simple/{pkg_name}",
            api::packages::pypi_simple_index,
        )
        // Maven repository layout — see `maven_layout_routes`.
        .with(maven_layout_routes)
        // NuGet protocol endpoints
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/nuget/index.json",
            api::packages::nuget_service_index,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/nuget/registration/{id}/index.json",
            api::packages::nuget_registration_index,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}",
            api::packages::nuget_registration_leaf,
        )
        .head(
            RepoRead,
            "/repos/{owner}/{name}/packages/nuget/registration/{id}/{version}",
            api::packages::nuget_registration_leaf,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/nuget/query",
            api::packages::nuget_search,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/nuget/autocomplete",
            api::packages::nuget_autocomplete,
        )
        // Flat container (`PackageBaseAddress/3.0.0`) — what `dotnet restore`
        // actually downloads from. The service index advertised it long before
        // anything served it (card_dba77cceec56).
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/nuget/package/{id}/index.json",
            api::packages::nuget_flat_container_index,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/nuget/package/{id}/{version}/{file}",
            api::packages::nuget_flat_container_download,
        )
        // `dotnet nuget push` sends PUT to the advertised PackagePublish
        // resource; the generic publish route is POST only, so the documented
        // push verb answered 405.
        //
        // POST is registered here too, and it is not decoration. A literal path
        // segment shadows the `{pkg_type}` one for the whole URL, so declaring
        // only PUT here would have taken `POST .../packages/nuget/publish` —
        // which every existing publisher uses — away from the generic route and
        // answered *it* 405 instead. Same reason npm spells both of its own
        // routes out rather than leaning on the generic ones.
        .post_with(
            RepoWrite,
            "/repos/{owner}/{name}/packages/nuget/publish",
            api::packages::nuget_publish,
            &package_envelope,
        )
        .put_with(
            RepoWrite,
            "/repos/{owner}/{name}/packages/nuget/publish",
            api::packages::nuget_publish,
            &package_envelope,
        )
        // RubyGems compact index — see `rubygems_protocol_routes`.
        .with(rubygems_protocol_routes)
        // RubyGems protocol endpoints
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/rubygems/api/v1/dependencies",
            api::packages::rubygems_dependencies,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/rubygems/api/v1/gems/{gem_name}",
            api::packages::rubygems_gem_info,
        )
        // Helm repository index
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/helm/index.yaml",
            api::packages::helm_index,
        )
        // Composer packages.json
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/composer/packages.json",
            api::packages::composer_packages_json,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}",
            api::packages::get_package,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/versions",
            api::packages::list_versions,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}",
            api::packages::get_version,
        )
        .delete(
            RepoWrite,
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}",
            api::packages::delete_version,
        )
        .patch(
            RepoWrite,
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/yank",
            api::packages::yank_version,
        )
        .get(
            RepoRead,
            "/repos/{owner}/{name}/packages/{pkg_type}/{pkg_name}/{version}/{*file}",
            api::packages::download_file,
        )
        // ── CI/CD runners ──────────────────────────────────────────────────
        // Registration is the one runner route a runner cannot already hold a
        // token for — it is what hands the token out. The credential is an
        // instance-admin session, so the level is `InstanceAdmin` and not
        // `RUNNER_TOKEN`: declaring it `Foreign` bought the route an
        // `Expect::Unchecked` exemption from the sweep it did not need.
        .post(InstanceAdmin, "/runners/register", api::runners::register)
        .post_with(
            RUNNER_TOKEN,
            "/runners/{id}/heartbeat",
            api::runners::heartbeat,
            &runner_auth,
        )
        .post_with(
            RUNNER_TOKEN,
            "/runners/{id}/deregister",
            api::runners::deregister,
            &runner_auth,
        )
        .get_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/poll",
            api::runners::poll_job,
            &runner_auth,
        )
        .post_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/start",
            api::runners::start_job,
            &runner_auth,
        )
        .post_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/log",
            api::runners::upload_log,
            &runner_auth,
        )
        .get_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/workspace",
            api::runners::download_workspace,
            &runner_auth,
        )
        .get_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/cache",
            api::runners::download_cache,
            &runner_auth,
        )
        .put_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/cache",
            api::runners::upload_cache,
            &runner_auth_1gb,
        )
        .post_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/finish",
            api::runners::finish_job,
            &runner_auth,
        )
        .post_with(
            RUNNER_TOKEN,
            "/runners/{id}/jobs/{job_id}/artifacts",
            api::artifacts::upload_artifact,
            &runner_auth,
        )
        // ── Artifacts ──────────────────────────────────────────────────────
        .get(
            RepoRead,
            "/repos/{owner}/{name}/pipelines/{id}/artifacts",
            api::artifacts::list_pipeline_artifacts,
        )
        .get(RepoRead, "/artifacts/{id}", api::artifacts::get_artifact)
        .get(
            RepoRead,
            "/artifacts/{id}/download",
            api::artifacts::download_artifact,
        )
        .delete(
            RepoWrite,
            "/artifacts/{id}",
            api::artifacts::delete_artifact,
        )
        // ── Instance administration ────────────────────────────────────────
        .get(
            InstanceAdmin,
            "/admin/runners",
            api::runners::list_runners_admin,
        )
        .get(
            InstanceAdmin,
            "/admin/runners/{id}",
            api::runners::get_runner_admin,
        )
        .delete(
            InstanceAdmin,
            "/admin/runners/{id}",
            api::runners::delete_runner_admin,
        )
        .get(InstanceAdmin, "/admin/users", api::admin::list_users)
        .get(InstanceAdmin, "/admin/users/{id}", api::admin::get_user)
        .patch(InstanceAdmin, "/admin/users/{id}", api::admin::update_user)
        .delete(InstanceAdmin, "/admin/users/{id}", api::admin::delete_user)
        .post(
            InstanceAdmin,
            "/admin/users/{id}/unlock",
            api::admin::unlock_user,
        )
        .get(InstanceAdmin, "/admin/orgs", api::admin::list_orgs)
        .get(InstanceAdmin, "/admin/orgs/{name}", api::admin::get_org)
        .delete(InstanceAdmin, "/admin/orgs/{name}", api::admin::delete_org)
        // Admin SSO
        .get(
            InstanceAdmin,
            "/admin/sso/providers",
            api::admin::list_sso_providers,
        )
        .post(
            InstanceAdmin,
            "/admin/sso/providers",
            api::admin::create_sso_provider,
        )
        .get(
            InstanceAdmin,
            "/admin/sso/providers/{id}",
            api::admin::get_sso_provider,
        )
        .patch(
            InstanceAdmin,
            "/admin/sso/providers/{id}",
            api::admin::update_sso_provider,
        )
        .delete(
            InstanceAdmin,
            "/admin/sso/providers/{id}",
            api::admin::delete_sso_provider,
        )
        .post(
            InstanceAdmin,
            "/admin/sso/providers/{id}/test",
            api::admin::test_sso_provider_connection,
        )
        // Audit logs (admin only)
        .get(
            InstanceAdmin,
            "/admin/audit/logs",
            api::audit::list_audit_logs,
        )
        .get(
            InstanceAdmin,
            "/admin/audit/logs/{id}",
            api::audit::get_audit_log,
        )
        .get(
            InstanceAdmin,
            "/admin/login-attempts",
            api::audit::list_login_attempts,
        )
        // Admin instance settings
        .get(InstanceAdmin, "/admin/settings", api::admin::get_settings)
        .patch(
            InstanceAdmin,
            "/admin/settings",
            api::admin::update_settings,
        )
        // ── Global search ──────────────────────────────────────────────────
        .get(PublicFiltered, "/search", api::search::search)
        // ── External CI/CD webhook ─────────────────────────────────────────
        .post(
            RepoWrite,
            "/repos/{owner}/{name}/webhooks/external/ci",
            api::webhooks_external::external_ci_webhook,
        )
        // ── AI agent endpoints ─────────────────────────────────────────────
        .get(
            RepoRead,
            "/ai/repos/{owner}/{name}/summary",
            api::ai::ai_repo_summary,
        )
        .get(
            RepoRead,
            "/ai/repos/{owner}/{name}/issues",
            api::ai::ai_list_issues,
        )
        .get(
            RepoRead,
            "/ai/repos/{owner}/{name}/prs",
            api::ai::ai_list_prs,
        )
        .get(
            RepoRead,
            "/ai/repos/{owner}/{name}/tree",
            api::ai::ai_repo_tree,
        )
        .get(
            RepoRead,
            "/ai/repos/{owner}/{name}/search/code",
            api::ai::ai_search_code,
        )
        // The write half of the pair above: `ai_search_code` reads `code_fts`,
        // this is the only thing that fills it over HTTP. `RepoWrite` because
        // indexing replaces the repository's whole snapshot.
        .post(
            RepoWrite,
            "/ai/repos/{owner}/{name}/index",
            api::ai::ai_index_repository,
        )
        // ── WebSocket ──────────────────────────────────────────────────────
        .get(
            WS_SESSION,
            "/ws/notifications",
            ws::ws_notifications_handler,
        )
        .get(WS_JOB_LOG, "/ws/job/{job_id}", ws::ws_job_log_handler)
        .finish();

    // Accept Personal Access Tokens on the REST API by translating them to a
    // Bearer JWT before the (JWT-only) handlers run.
    let api_v1 = api_v1.layer(axum::middleware::from_fn_with_state(
        state.clone(),
        pat_auth::pat_auth_middleware,
    ));

    let (v2, v2_facts) = build_v2_routes(state);
    // The docs router is built last because it needs the REST table: every
    // documented path is mounted under `/api/v1`, and the access level each of
    // them declares is what the published `security` is derived from.
    let (docs, docs_facts) = build_docs_routes(state, &api_facts);

    let facts = api_facts
        .into_iter()
        .chain(git_facts)
        .chain(root_facts)
        .chain(v2_facts)
        .chain(docs_facts)
        .collect();

    Routers {
        api_v1,
        git,
        root,
        v2,
        docs,
        facts,
    }
}

/// Create the Axum router for testing (no rate limiter, no static file serving).
pub(crate) fn build_test_router(state: AppState) -> Router {
    build_test_router_with_facts(state).0
}

/// The test router together with the access level of every route in it.
///
/// The sweep test drives the same router the other integration tests use, and
/// reads the declarations out of the very build that produced it — there is no
/// second enumeration to fall out of step.
pub(crate) fn build_test_router_with_facts(state: AppState) -> (Router, Vec<RouteFact>) {
    // No auth limiter in tests: the limiter middleware extracts ConnectInfo,
    // which the test harness does not supply. Passing None skips that layer.
    let routers = build_all_routes(&state, None);
    let facts = routers.facts.clone();

    // Same stack as production, minus the rate limiter — see `apply_middleware`.
    // The maintenance gate is part of it, which is only safe now that the
    // settings live in this `AppState` instead of a process-global: while they
    // were global, a test that switched the mode on switched it on for every
    // other test in the binary (card_08bab0b46e40).
    let router = apply_middleware(with_spa_fallback(assemble(&routers), &state), &state, None)
        .with_state(state);

    (router, facts)
}

#[cfg(test)]
mod cors_origin_tests {
    use super::parse_cors_origins;

    fn rejected_entries(configured: &str) -> Vec<String> {
        parse_cors_origins(configured)
            .1
            .into_iter()
            .map(|(entry, _)| entry)
            .collect()
    }

    fn accepted_entries(configured: &str) -> Vec<String> {
        parse_cors_origins(configured)
            .0
            .into_iter()
            .map(|value| value.to_str().unwrap().to_string())
            .collect()
    }

    /// The defect: one bad entry in the list used to vanish, leaving the
    /// operator with a CORS failure on exactly one frontend and no log line.
    #[test]
    fn a_malformed_entry_is_dropped_and_named() {
        let configured = "https://ok.test, не origin";

        assert_eq!(accepted_entries(configured), vec!["https://ok.test"]);
        assert_eq!(rejected_entries(configured), vec!["не origin"]);
    }

    /// A fully valid list stays quiet — a warning that fires on healthy config
    /// is a warning nobody reads.
    #[test]
    fn a_valid_list_reports_nothing() {
        let configured = "https://app.example.com, http://localhost:5173, https://[::1]:8443";

        assert_eq!(
            accepted_entries(configured),
            vec![
                "https://app.example.com",
                "http://localhost:5173",
                "https://[::1]:8443"
            ]
        );
        assert!(rejected_entries(configured).is_empty());
    }

    /// The entries `HeaderValue::from_str` waved through. Each of these used to
    /// enter the allowlist and then match no browser `Origin` header for the
    /// life of the process, which is the same failure with no invalid header
    /// value anywhere in sight.
    #[test]
    fn entries_that_are_valid_header_values_but_not_origins_are_rejected() {
        for entry in [
            "example.com",
            "https://foo.test/some/path",
            "https:// example.test",
            "https://user@example.test",
            "https://example.test?x=1",
            "://example.test",
        ] {
            assert_eq!(
                rejected_entries(entry),
                vec![entry.to_string()],
                "'{entry}' is not an origin and must be reported, not silently kept"
            );
        }
    }

    /// Blank entries are formatting, not typos — a trailing comma must not
    /// produce a warning.
    #[test]
    fn empty_entries_are_neither_accepted_nor_reported() {
        let configured = "https://ok.test,, ,";

        assert_eq!(accepted_entries(configured), vec!["https://ok.test"]);
        assert!(rejected_entries(configured).is_empty());
    }

    /// A wildcard is a configuration choice, not an origin to parse.
    #[test]
    fn a_wildcard_passes_through_unchanged() {
        assert_eq!(accepted_entries("*"), vec!["*"]);
        assert!(rejected_entries("*").is_empty());
    }
}
