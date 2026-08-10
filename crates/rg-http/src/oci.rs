//! OCI Distribution HTTP handlers.
//!
//! Implements OCI Distribution Spec v1.0 endpoints at `/v2/`.
//! Each handler follows the OCI error response format (RFC 7807).

use anyhow::Context as _;
use axum::{
    body::Body,
    extract::{Path, Query, State},
    http::{header, HeaderMap, HeaderName, StatusCode},
    response::{IntoResponse, Response},
    Json,
};
use sea_orm::DatabaseConnection;
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWriteExt};

use rg_core::auth::jwt;
use rg_core::auth::oci_token::{
    build_www_authenticate, generate_oci_token, validate_oci_token, ParsedScope,
};
use rg_core::package_registry::oci::{
    acquire_publication_lease, error_codes, is_client_digest_fault, media_types, publish_blob,
    release_publication_lease, BlobSource, ErrorDetail, ErrorResponse, OciPublicationBusy,
    ParsedManifest, PublicationLease, Reference, StoredManifest, TagListResponse, API_VERSION,
};

use crate::api::repo_access;
use crate::error::AppError;
use crate::AppState;

// Docker distribution custom headers
const DOCKER_CONTENT_DIGEST: HeaderName = HeaderName::from_static("docker-content-digest");
const DOCKER_UPLOAD_UUID: HeaderName = HeaderName::from_static("docker-upload-uuid");
const RANGE: HeaderName = HeaderName::from_static("range");
const DOCKER_API_VERSION: HeaderName = HeaderName::from_static("docker-distribution-api-version");

const OCI_UPLOAD_STORAGE_HINT: &str =
    "chunked OCI uploads are staged in `_oci_uploads/` under the `[server].repo_root` \
     directory; that directory must be writable by the user running forgekeep";

/// The `sub` an OCI token minted without credentials carries. It is a literal,
/// not a username: no account may hold it, and `token_subject` reads it as
/// "there is nobody behind this token", not as "look this account up".
const ANONYMOUS_SUBJECT: &str = "anonymous";

/// The `service` every challenge advertises and every token is minted for. It
/// is one value in three places (the version check, the refusal challenge, the
/// token's `aud`), so it is one constant.
const REGISTRY_SERVICE: &str = "forgekeep-registry";

// ── helpers ──────────────────────────────────────────────────

/// Build an OCI `{errors:[{code,message}]}` envelope.
///
/// `message` is the only diagnostic a `docker push` / `pull` ever prints, and
/// this path does not go through `AppError`, so nothing else logs the cause.
/// Callers must therefore pass `&format!("{e:#}")`, never `&e.to_string()` —
/// the latter prints the outermost `.context(...)` alone and drops the io /
/// digest / db error underneath it (card_a997f30c142c).
fn oci_err(status: StatusCode, code: &str, message: &str) -> Response {
    (status, oci_error_body(code, message)).into_response()
}

/// The envelope on its own, for the responses that carry a header of their own
/// alongside it (see [`oci_unauthorized`]).
fn oci_error_body(code: &str, message: &str) -> Json<ErrorResponse> {
    Json(ErrorResponse {
        errors: vec![ErrorDetail {
            code: code.to_string(),
            message: message.to_string(),
            detail: None,
        }],
    })
}

fn oci_not_found(code: &str, message: &str) -> Response {
    oci_err(StatusCode::NOT_FOUND, code, message)
}

/// One actionable line for a filesystem failure on a local OCI blob.
///
/// `OciStorage::blob_local_path` derives the file from the digest inside the
/// storage backend, so neither the request nor the OCI error envelope ever
/// names it — a `docker pull` against a repo_root whose bind-mount lost its
/// permissions reports `failed to stat blob`, or a bare errno, and the operator
/// has no file to go and look at.
fn blob_path_error(what: &str, path: &std::path::Path, error: &std::io::Error) -> String {
    rg_core::platform::fs::describe_path_error(
        what,
        path,
        error,
        rg_core::platform::fs::BLOB_STORAGE_HINT,
    )
}

/// Classify a DB-layer error into the HTTP status for an OCI response, while
/// leaving the OCI error-envelope untouched.
///
/// The registry can't route errors through `AppError` the way the JSON API
/// does: docker/podman expect the OCI-conformant `{errors:[{code,message}]}`
/// body that `oci_err` emits, and swapping in the `AppError` JSON would break
/// the protocol. So instead of converting the error, we classify only the
/// *status*: a connection-level `sea_orm::DbErr` (pool closed / acquire timeout
/// / dropped connection) is a transient, retryable outage → 503; everything
/// else stays 500. The outage predicate is shared with the JSON API via
/// `AppError::is_db_outage`, so a database outage on `/v2/...` classifies
/// identically to one on the rest of the API.
///
/// Implemented for both error shapes the DB sites surface: `find_oci_repo` /
/// `check_access` / `find_or_create_oci_repo` return `anyhow::Result` (a
/// `DbErr` wrapped through `.context()`), while the `rg_db::ops::oci_ops::*`
/// helpers return `Result<_, sea_orm::DbErr>` directly.
trait OciDbStatus {
    fn oci_status(&self) -> StatusCode;
}

impl OciDbStatus for sea_orm::DbErr {
    fn oci_status(&self) -> StatusCode {
        if crate::error::AppError::is_db_outage(self) {
            StatusCode::SERVICE_UNAVAILABLE
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

impl OciDbStatus for anyhow::Error {
    fn oci_status(&self) -> StatusCode {
        // A concurrent push of the same digest still holds its publication
        // lease. Nothing is wrong with this request and nothing is wrong with
        // the registry — the key is simply being written by somebody else right
        // now, and the client should come back. `docker push` retries a `503`
        // and gives up on the `500` this would otherwise be.
        if let Some(busy) = self.downcast_ref::<OciPublicationBusy>() {
            tracing::warn!(
                storage_key = %busy.key,
                waited_seconds = busy.waited_seconds,
                "OCI key is being published by a concurrent push, returning 503"
            );
            return StatusCode::SERVICE_UNAVAILABLE;
        }
        // `downcast_ref` sees through any `.context()` layers to the original
        // `DbErr`, mirroring `From<anyhow::Error> for AppError`.
        match self.downcast_ref::<sea_orm::DbErr>() {
            Some(db_err) => db_err.oci_status(),
            None => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

/// Status for a DB-touching error, preserving the OCI envelope: connection-level
/// DB outages become 503, all else 500. See [`OciDbStatus`].
fn oci_status_for<E: OciDbStatus>(e: &E) -> StatusCode {
    e.oci_status()
}

/// A `401` that says where to go next.
///
/// Every OCI `401` except the one from `GET /v2/` used to leave without a
/// `WWW-Authenticate`, which RFC 7235 requires of any `401` and which the
/// distribution spec builds its whole auth flow on. Docker survives it by
/// caching the challenge from the version-check ping and deriving the scope
/// from the operation it is attempting; a client that instead reads the
/// challenge off the request that failed — which is what the spec describes —
/// has nowhere to read it from (card_87107a1e40bd).
///
/// The scope is the one the refused operation needs, not the catalog scope the
/// version check advertises: a client takes the scope out of the challenge and
/// asks the token endpoint for exactly that, so a wrong scope here buys a token
/// that is refused again.
fn oci_unauthorized(headers: &HeaderMap, scope: &str, message: &str) -> Response {
    let challenge = www_authenticate(
        &format!("{}/v2/auth/token", get_base_url(headers)),
        REGISTRY_SERVICE,
        scope,
    );
    (
        StatusCode::UNAUTHORIZED,
        [(header::WWW_AUTHENTICATE, challenge.as_str())],
        oci_error_body(error_codes::UNAUTHORIZED, message),
    )
        .into_response()
}

/// The `Authorization: Bearer` token this request carries, whichever kind.
fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    headers
        .get(header::AUTHORIZATION)?
        .to_str()
        .ok()?
        .strip_prefix("Bearer ")
}

/// The ForgeKeep user behind a normal user JWT in `Authorization: Bearer`.
///
/// Deliberately *not* `api::auth::extract_user_id`: that one also accepts the
/// browser session cookie, and the registry is not a browser surface. An OCI
/// scoped token yields nothing here — it names a scope, not a user, and used to
/// be reported as the sentinel user `0`, a user id that exists in no database
/// and only ever reached a permission check by accident.
fn bearer_user_id(headers: &HeaderMap, jwt_secret: &str) -> Option<i64> {
    let claims = jwt::validate_token(bearer_token(headers)?, jwt_secret)?;
    claims.sub.parse::<i64>().ok().filter(|uid| *uid > 0)
}

/// Fold the shared repository gate's verdict into the boolean this protocol
/// needs, without losing the difference the gate draws: a denial is "not
/// allowed", a check that could not *run* stays an error. Collapsing the second
/// into the first is what makes a database outage look like a credentials
/// problem — the registry then answers 401, and docker comes straight back for
/// another token instead of backing off.
fn granted(decision: Result<(), AppError>) -> Result<bool, AppError> {
    match decision {
        Ok(()) => Ok(true),
        Err(error) if repo_access::is_access_denial(&error) => Ok(false),
        Err(error) => Err(error),
    }
}

/// Who an OCI scoped token was minted for, as far as the *present* is
/// concerned.
///
/// A scoped token names its subject by username, which is not something the
/// repository gate takes and not something the revocation middleware can read
/// (`api::auth::session_standing_middleware` parses `sub` as a user id, so an
/// OCI token is invisible to it). Resolving the name back to a live account is
/// therefore this file's job, and it is the same question
/// `api::lfs::signer_still_stands` asks of a signed URL's actor.
enum TokenSubject {
    /// The token names no account at all — issued to `anonymous`, so the gate
    /// answers it as it answers a caller with no credentials.
    Anonymous,
    /// A live account the gate can be asked about.
    User(i64),
    /// The named account is gone or deactivated. The token keeps its signature
    /// and its scope; it no longer has anybody behind it.
    Gone,
}

/// Resolve a scoped token's `sub` into the actor the repository gate takes.
///
/// Fails closed on a denial ([`TokenSubject::Gone`]) and keeps a *failed*
/// lookup an error, so a database outage stays a 503 instead of collapsing into
/// the 401 that sends docker straight back for another token.
async fn token_subject(state: &AppState, sub: &str) -> Result<TokenSubject, AppError> {
    if sub == ANONYMOUS_SUBJECT {
        return Ok(TokenSubject::Anonymous);
    }
    match rg_db::ops::user_ops::find_by_username(&state.db, sub).await {
        Ok(Some(user)) if user.is_usable() => Ok(TokenSubject::User(user.id)),
        Ok(_) => {
            tracing::warn!(
                subject = sub,
                "rejecting an OCI scoped token: the account behind it is disabled or gone"
            );
            Ok(TokenSubject::Gone)
        }
        Err(error) => {
            tracing::error!(
                subject = sub,
                error = %format!("{error:#}"),
                "could not verify the account behind an OCI scoped token"
            );
            Err(AppError::from(error))
        }
    }
}

/// Check if the request has access to perform an OCI repo action.
///
/// The registry decides *who* is calling — a ForgeKeep user JWT, an OCI scoped
/// bearer token, or nobody — and the shared repository gate in
/// `api::repo_access` decides what that caller may do. That holds for the
/// scoped token too: it names its caller by username rather than by id, so
/// resolving it costs a lookup ([`token_subject`]), but the permission decision
/// is still the gate's and is still taken now rather than read off a capability
/// minted up to five minutes ago.
async fn check_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    required_action: &str,
) -> Result<(bool, Option<i64>), AppError> {
    let repo_model = match rg_core::repo::service::find_repo_by_owner_name(&state.db, owner, repo)
        .await
        .map_err(AppError::from)?
    {
        Some(repo) => repo,
        None => return Ok((false, None)),
    };

    // A normal ForgeKeep JWT is a user, so the answer is the same one the REST
    // API would give that user for this repository.
    if let Some(uid) = bearer_user_id(headers, &state.jwt_secret) {
        let allowed = match required_action {
            "pull" => granted(repo_access::check_read_for(state, &repo_model, Some(uid)).await)?,
            "push" => granted(repo_access::check_write_for(state, &repo_model, Some(uid)).await)?,
            _ => false,
        };
        return Ok((allowed, allowed.then_some(uid)));
    }

    // An OCI scoped token. Its scope is a *capability*: `get_token` ran this
    // same gate to mint it, and the scope string is that answer, frozen. What
    // it does not carry is the answer's shelf life — drop the collaborator or
    // deactivate the account and the token keeps saying `pull,push` for the
    // rest of its 300 seconds, which is time enough to push a tag into a
    // private registry. So the scope only decides whether this token is *about*
    // this repository and this action; whether that is still allowed is asked
    // again, of the gate, against the account the token names.
    if let Some(claims) =
        bearer_token(headers).and_then(|token| validate_oci_token(token, &state.jwt_secret))
    {
        let scoped_for_this = claims
            .scope
            .iter()
            .flat_map(|s| s.split_whitespace())
            .any(|s| {
                ParsedScope::parse(s).is_some_and(|parsed| {
                    parsed.matches_repo(owner, repo) && parsed.has_action(required_action)
                })
            });

        if scoped_for_this {
            let actor = match token_subject(state, &claims.sub).await? {
                TokenSubject::Anonymous => None,
                TokenSubject::User(uid) => Some(uid),
                // Nobody behind the token: it grants nothing of its own. The
                // request carries on as an unauthenticated one, so a public
                // repository still answers a pull — the token cannot leave its
                // holder worse off than presenting no credentials at all.
                TokenSubject::Gone => {
                    return anonymous_pull(state, &repo_model, required_action).await
                }
            };

            let allowed = match required_action {
                "pull" => granted(repo_access::check_read_for(state, &repo_model, actor).await)?,
                "push" => granted(repo_access::check_write_for(state, &repo_model, actor).await)?,
                _ => false,
            };
            if allowed {
                return Ok((true, actor));
            }
            // Denied on this token's own terms; the public-pull fallback below
            // is the only thing that can still admit it.
        }
    }

    anonymous_pull(state, &repo_model, required_action).await
}

/// What this repository owes a caller with no credentials: a public repository
/// answers a pull, everything else is a denial.
async fn anonymous_pull(
    state: &AppState,
    repo_model: &rg_db::entities::repository::Model,
    required_action: &str,
) -> Result<(bool, Option<i64>), AppError> {
    if required_action == "pull" {
        let allowed = granted(repo_access::check_read_for(state, repo_model, None).await)?;
        return Ok((allowed, None));
    }

    Ok((false, None))
}

async fn require_access(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    required_action: &str,
) -> Result<Option<i64>, Response> {
    match check_access(state, headers, owner, repo, required_action).await {
        Ok((true, user_id)) => Ok(user_id),
        Ok((false, _)) => Err(oci_unauthorized(
            headers,
            &format!("repository:{owner}/{repo}:{required_action}"),
            "authentication required",
        )),
        // The gate's own status (503 on a database outage, 500 otherwise)
        // inside the OCI envelope docker expects.
        Err(e) => Err(oci_err(e.status(), "UNKNOWN", &e.to_string())),
    }
}

/// Resolve owner/repo from OCI namespace string.
/// In ForgeKeep, the OCI name is always "{owner}/{repo}".
fn parse_namespace(name: &str) -> Option<(&str, &str)> {
    let parts: Vec<&str> = name.splitn(2, '/').collect();
    if parts.len() == 2 {
        Some((parts[0], parts[1]))
    } else {
        None
    }
}

/// Build the WWW-Authenticate header for Docker auth challenge.
fn www_authenticate(realm: &str, service: &str, scope: &str) -> String {
    build_www_authenticate(realm, service, scope)
}

// ── API Version Check ────────────────────────────────────────

/// `GET /v2/` — API version check, and the only place a client can find out
/// that its credentials were accepted.
///
/// This endpoint answers two different questions depending on what the request
/// carries, and it used to answer only the first. Without credentials it is
/// "where do I get a token": `401` plus the challenge naming the realm. *With*
/// credentials it is "are these good": the distribution spec makes a `200` the
/// answer, and `docker login` is defined as exactly that round trip — ping,
/// read the challenge, fetch a token, ping again, and treat a `200` as success.
///
/// Answering `401` unconditionally therefore meant `docker login` could not
/// succeed against this registry at all, which in turn meant no client could
/// hold credentials for a private repository: every push was refused and every
/// private pull was anonymous. It was never noticed because the registry's
/// tests speak the wire protocol themselves and never had a login to perform;
/// the live-client test that found it is `oci_live_client_tests`.
pub async fn api_version_check(State(state): State<AppState>, headers: HeaderMap) -> Response {
    if authenticated_caller(&state, &headers) {
        return (StatusCode::OK, [(DOCKER_API_VERSION, API_VERSION)]).into_response();
    }

    // The realm is not decoration: a client does not guess where to get its
    // token, it reads this path out of the challenge and goes there. It has to
    // be the path `build_v2_routes` registers for `oci::get_token`.
    let realm = format!("{}/v2/auth/token", get_base_url(&headers));

    (
        StatusCode::UNAUTHORIZED,
        [
            (DOCKER_API_VERSION, API_VERSION),
            (
                header::WWW_AUTHENTICATE,
                www_authenticate(&realm, REGISTRY_SERVICE, "registry:catalog:*").as_str(),
            ),
        ],
    )
        .into_response()
}

/// Does this request carry a credential this registry issued or accepts?
///
/// Deliberately **not** an authorization decision, and deliberately not a
/// database read. The `200` it feeds grants nothing — it says "your credentials
/// were recognised", which is the one thing `docker login` needs and the only
/// thing it can act on. Whether the account behind them may still read or write
/// a given repository is asked per request, of the repository gate, by
/// `check_access`; putting a second, weaker copy of that question here would
/// only create somewhere for the two answers to drift apart.
///
/// So a signature is enough: an OCI token was minted by `get_token` *after*
/// `authenticate_basic` accepted a password or a PAT, and it expires in five
/// minutes. A deactivated account holding an unexpired one sees its `docker
/// login` succeed and then every pull and push refused — which is the correct
/// outcome, arrived at by the check that decides something.
///
/// An anonymous scoped token does not count: `get_token` mints one for a caller
/// who presented nothing, so honouring it would report a successful login for a
/// password the registry never accepted.
fn authenticated_caller(state: &AppState, headers: &HeaderMap) -> bool {
    if bearer_user_id(headers, &state.jwt_secret).is_some() {
        return true;
    }
    bearer_token(headers)
        .and_then(|token| validate_oci_token(token, &state.jwt_secret))
        .is_some_and(|claims| claims.sub != ANONYMOUS_SUBJECT)
}

// ── Token Endpoint ─────────────────────────────────────
//
// `GET /v2/auth/token` — OCI Distribution token endpoint.
//
// Query parameters:
//   - `service`: The service name (must match `aud` in token)
//   - `scope`: Requested scope (e.g., `repository:alice/hello:pull,push`)
//   - `offline_token`: (optional) for refreshing
//   - `client_id`: (optional) client identifier
//
// Authentication:
//   - Anonymous: returns token with limited scope (public pull)
//   - Basic Auth: validates username/password, returns full scope token
//
/// Who the registry decided is behind a `/v2/auth/token` request.
enum BasicIdentity {
    /// The request carries no usable credentials — a missing header or a
    /// malformed Basic value. This is the public-pull path.
    Anonymous,
    Authenticated {
        username: String,
        user_id: i64,
    },
    /// The request supplied a syntactically valid credential which was not
    /// accepted. Unknown users, wrong passwords, and PATs without `repo` all
    /// land here and receive the same OCI 401 response, so this state cannot
    /// enumerate accounts while also never posing as anonymous access.
    Rejected,
    /// The password was right and the account carries a second factor. Kept
    /// apart from `Anonymous` because it is the one refusal that may be
    /// explained: reaching it takes the correct password, so the answer tells a
    /// guesser nothing, while an owner who just switched MFA on would otherwise
    /// watch `docker login` degrade into anonymous pulls with no reason given.
    SecondFactorRequired,
}

/// Resolve Basic-auth credentials from the request headers.
///
/// Returns the authenticated account when valid Docker-login credentials are
/// present, or [`BasicIdentity::Anonymous`] when the request carries none that
/// are usable.
///
/// Two credentials are accepted, in this order: a personal access token in
/// either Basic field, then the account's password. The token goes first
/// because it is the credential an account with MFA has left here — and because
/// running it through the password path would file a valid token as a failed
/// password, so five `docker pull`s would lock the account out of the forge.
///
/// A credential check that *failed* is `Err`, never the anonymous default: an
/// unreachable database or a stored hash the verifier cannot parse says nothing
/// about who is calling, and answering "anonymous" mints a token whose scope was
/// narrowed for a reason the client can't see. Docker then bounces off the first
/// pull with a 401 — the one answer that sends it straight back to this endpoint
/// — and loops instead of surfacing the outage.
async fn authenticate_basic(
    db: &DatabaseConnection,
    headers: &HeaderMap,
) -> anyhow::Result<BasicIdentity> {
    let anonymous = || Ok(BasicIdentity::Anonymous);

    let Some(auth_header) = headers.get(header::AUTHORIZATION) else {
        return anonymous();
    };
    let Ok(auth_str) = auth_header.to_str() else {
        return anonymous();
    };
    let Some(b64) = auth_str.strip_prefix("Basic ") else {
        return anonymous();
    };
    use base64::Engine as _;
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64) else {
        return anonymous();
    };
    let Ok(creds) = std::str::from_utf8(&decoded) else {
        return anonymous();
    };
    let parts: Vec<&str> = creds.splitn(2, ':').collect();
    let [user, pass] = parts[..] else {
        return anonymous();
    };

    // A personal access token may arrive in either field: `docker login -u me
    // -p <token>` puts it in the password, and some clients carry it as the
    // username. Same two candidates, same order, as git-over-HTTP.
    for candidate in [pass, user] {
        if candidate.is_empty() {
            continue;
        }
        let Some((token, owner)) = crate::pat_auth::resolve_pat(db, candidate)
            .await
            .with_context(|| format!("registry basic auth: resolving a token for '{user}'"))?
        else {
            continue;
        };
        // A token that resolves is the answer either way. Falling through to
        // the password path on a scope refusal would hash the token, fail, and
        // record the failure as a brute-force strike against its owner.
        if !rg_core::auth::pat_scope::has_scope(&token.scopes, "repo") {
            tracing::warn!(
                user_id = owner.id,
                token_id = token.id,
                "registry basic auth: token lacks the 'repo' scope"
            );
            return Ok(BasicIdentity::Rejected);
        }
        return Ok(BasicIdentity::Authenticated {
            username: owner.username,
            user_id: owner.id,
        });
    }

    // Verify unconditionally — an unknown username must cost the same Argon2
    // work as a real one, or the response time enumerates accounts. (A failed
    // *lookup* returns early without that work, which leaks nothing: the
    // database is either up for every account or down for all of them.)
    let found = rg_db::ops::user_ops::find_by_username(db, user)
        .await
        .with_context(|| format!("registry basic auth: looking up '{user}'"))?;
    let password_ok = rg_core::auth::password::verify_password_or_dummy(
        pass,
        found.as_ref().map(|u| u.password_hash.as_str()),
    )
    .with_context(|| format!("registry basic auth: verifying the password of '{user}'"))?;

    // Settled after the hash, so neither a deactivated nor a locked account is
    // distinguishable from a wrong password by how fast the registry says no.
    // The same helper runs on the SSH password door: without it `docker login`
    // was an unmetered, unlogged place to guess passwords the web login stops
    // after five tries.
    let (ip_address, user_agent) = crate::api::audit::extract_ip_and_ua(headers);
    let attempt = rg_core::auth::lockout::settle_password_attempt(
        db,
        found.as_ref(),
        password_ok,
        rg_core::auth::lockout::AttemptOrigin {
            login: user,
            channel: "registry",
            ip_address: ip_address.as_deref(),
            user_agent: user_agent.as_deref(),
        },
    )
    .await;

    match attempt {
        rg_core::auth::lockout::PasswordAttempt::Accepted => {
            let user_id = found
                .as_ref()
                .map(|found| found.id)
                .expect("an accepted password attempt resolved to an account");
            Ok(BasicIdentity::Authenticated {
                username: user.to_string(),
                user_id,
            })
        }
        rg_core::auth::lockout::PasswordAttempt::SecondFactorRequired => {
            Ok(BasicIdentity::SecondFactorRequired)
        }
        rg_core::auth::lockout::PasswordAttempt::Rejected { .. } => Ok(BasicIdentity::Rejected),
    }
}

/// Resolve a single `repository:...` scope request into the granted scope
/// string, or `Ok(None)` when the repo is missing or the caller has no access.
///
/// A permission query that *failed* is reported as `Err`, not folded into
/// "no access": silently narrowing the scope hands the client a token that
/// then bounces off every pull with a 401, so docker re-runs the auth flow in a
/// loop instead of seeing the outage and backing off.
async fn grant_repository_scope(
    state: &AppState,
    parsed: &ParsedScope,
    authenticated_user_id: Option<i64>,
) -> Result<Option<String>, AppError> {
    let Some((scope_owner, scope_repo)) = parse_namespace(&parsed.name) else {
        return Ok(None);
    };
    let repo_model =
        match rg_core::repo::service::find_repo_by_owner_name(&state.db, scope_owner, scope_repo)
            .await
            .map_err(AppError::from)?
        {
            Some(repo) => repo,
            None => return Ok(None),
        };

    // Minting a scope *is* an access decision — it hands out a capability the
    // pull/push handlers then trust without re-deriving it — so it is taken by
    // the same gate, against the same user, as a request on the REST API.
    let mut allowed_actions = Vec::new();
    if parsed.has_action("pull")
        && granted(repo_access::check_read_for(state, &repo_model, authenticated_user_id).await)?
    {
        allowed_actions.push("pull");
    }
    if parsed.has_action("push")
        && authenticated_user_id.is_some()
        && granted(repo_access::check_write_for(state, &repo_model, authenticated_user_id).await)?
    {
        allowed_actions.push("push");
    }

    Ok(if allowed_actions.is_empty() {
        None
    } else {
        Some(format!(
            "repository:{}:{}",
            parsed.name,
            allowed_actions.join(",")
        ))
    })
}

/// Evaluate every requested scope against the caller's permissions and return
/// the subset of scope strings that are actually granted.
async fn resolve_granted_scopes(
    state: &AppState,
    scope: &str,
    authenticated_user_id: Option<i64>,
) -> Result<Vec<String>, AppError> {
    let mut granted_scopes = Vec::new();
    for scope_part in scope.split_whitespace() {
        let Some(parsed) = ParsedScope::parse(scope_part) else {
            continue;
        };

        if parsed.scope_type == "repository" {
            if let Some(scope) =
                grant_repository_scope(state, &parsed, authenticated_user_id).await?
            {
                granted_scopes.push(scope);
            }
        } else if parsed.scope_type == "registry"
            && parsed.name == "catalog"
            && authenticated_user_id.is_some()
        {
            granted_scopes.push(scope_part.to_string());
        }
    }
    Ok(granted_scopes)
}

/// `GET /v2/auth/token` — issue an OCI Bearer token.
pub async fn get_token(
    State(state): State<AppState>,
    headers: HeaderMap,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let _service = params
        .get("service")
        .cloned()
        .unwrap_or_else(|| "forgekeep-registry".to_string());
    let scope = params.get("scope").cloned().unwrap_or_default();

    let (username, authenticated_user_id) = match authenticate_basic(&state.db, &headers).await {
        Ok(BasicIdentity::Anonymous) => (ANONYMOUS_SUBJECT.to_string(), None),
        Ok(BasicIdentity::Authenticated { username, user_id }) => (username, Some(user_id)),
        Ok(BasicIdentity::Rejected) => {
            return oci_err(
                StatusCode::UNAUTHORIZED,
                error_codes::UNAUTHORIZED,
                "invalid credentials",
            );
        }
        Ok(BasicIdentity::SecondFactorRequired) => {
            // The one refusal the registry states out loud, and the only place
            // the owner can be told: `docker login` prints this message, and
            // without it the password that still works in the browser would
            // simply stop granting scope here with no explanation.
            return oci_err(
                StatusCode::UNAUTHORIZED,
                error_codes::UNAUTHORIZED,
                "this account requires a second factor: authenticate with a personal \
                 access token instead of your password",
            );
        }
        Err(e) => {
            // Nothing downstream logs this — `oci_err` only writes the client's
            // envelope — and the message is deliberately generic: the caller
            // gets a status it can back off on, the operator gets the account
            // and the cause.
            tracing::error!(error = %format!("{e:#}"), "registry basic auth could not be evaluated");
            return oci_err(
                oci_status_for(&e),
                "UNKNOWN",
                "authentication is temporarily unavailable",
            );
        }
    };

    // A token minted while the permission lookups were failing would carry a
    // silently narrowed scope, and the client would spend the next pull being
    // told 401 — the one answer that makes it come straight back here. Report
    // the outage instead so docker/podman can back off and retry.
    let granted_scope = match resolve_granted_scopes(&state, &scope, authenticated_user_id).await {
        Ok(scopes) => scopes.join(" "),
        Err(e) => return oci_err(e.status(), "UNKNOWN", &e.to_string()),
    };

    // Generate token (TTL: 300s for normal, 60s for anonymous)
    let ttl = if username == ANONYMOUS_SUBJECT {
        60
    } else {
        300
    };
    let token = match generate_oci_token(&username, &granted_scope, &state.jwt_secret, ttl) {
        Ok(t) => t,
        Err(e) => {
            tracing::error!("Failed to generate OCI token: {}", e);
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                "token generation failed",
            );
        }
    };

    // Return token response (OCI Distribution Spec format)
    Json(serde_json::json!({
        "token": token,
        "expires_in": ttl,
        "issued_at": chrono::Utc::now().to_rfc3339(),
    }))
    .into_response()
}

// ── Tags ─────────────────────────────────────────────────────

/// `GET /v2/{owner}/{repo}/tags/list` — list tags.
pub async fn list_tags(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "pull").await {
        return resp;
    }

    let oci_repo = match find_oci_repo(&state.db, &owner, &repo).await {
        Ok(Some(r)) => r,
        Ok(None) => return oci_not_found(error_codes::NAME_UNKNOWN, "repository not found"),
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    let tags = match rg_db::ops::oci_ops::list_tags(&state.db, oci_repo.id).await {
        Ok(t) => t,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    (
        StatusCode::OK,
        Json(TagListResponse {
            name: format!("{owner}/{repo}"),
            tags,
        }),
    )
        .into_response()
}

// ── Manifest ─────────────────────────────────────────────────

/// `HEAD /v2/{owner}/{repo}/manifests/{reference}` — check manifest existence.
pub async fn head_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
) -> Response {
    get_manifest_impl(State(state), headers, Path((owner, repo, reference)), true).await
}

/// `GET /v2/{owner}/{repo}/manifests/{reference}` — pull manifest.
pub async fn get_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
) -> Response {
    get_manifest_impl(State(state), headers, Path((owner, repo, reference)), false).await
}

async fn get_manifest_impl(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
    head_only: bool,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "pull").await {
        return resp;
    }

    let oci_repo = match find_oci_repo(&state.db, &owner, &repo).await {
        Ok(Some(r)) => r,
        Ok(None) => return oci_not_found(error_codes::NAME_UNKNOWN, "repository not found"),
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    let rf = Reference::parse(&reference);

    // Look up manifest
    let manifest = match &rf {
        Reference::Digest(d) => {
            rg_db::ops::oci_ops::find_manifest_by_digest(&state.db, oci_repo.id, d).await
        }
        Reference::Tag(t) => {
            rg_db::ops::oci_ops::find_manifest_by_tag(&state.db, oci_repo.id, t).await
        }
    };

    let manifest = match manifest {
        Ok(Some(m)) => m,
        Ok(None) => return oci_not_found(error_codes::MANIFEST_UNKNOWN, "manifest not found"),
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    // Compute digest in Docker format
    let docker_digest = format!(
        "{}:{}",
        manifest.digest.split(':').next().unwrap_or("sha256"),
        manifest
            .digest
            .split(':')
            .nth(1)
            .unwrap_or(&manifest.digest)
    );

    if head_only {
        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, manifest.media_type.as_str()),
                (header::CONTENT_LENGTH, manifest.size.to_string().as_str()),
                (DOCKER_CONTENT_DIGEST, docker_digest.as_str()),
            ],
            String::new(),
        )
            .into_response()
    } else {
        (
            StatusCode::OK,
            [
                (header::CONTENT_TYPE, manifest.media_type.as_str()),
                (DOCKER_CONTENT_DIGEST, docker_digest.as_str()),
            ],
            manifest.manifest_json,
        )
            .into_response()
    }
}

/// `PUT /v2/{owner}/{repo}/manifests/{reference}` — push manifest.
pub async fn put_manifest(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, reference)): Path<(String, String, String)>,
    body: String,
) -> Response {
    let user_id = match require_access(&state, &headers, &owner, &repo, "push").await {
        Ok(user_id) => user_id,
        Err(resp) => return resp,
    };

    let oci_repo = match find_oci_repo(&state.db, &owner, &repo).await {
        Ok(Some(r)) => r,
        Ok(None) => return oci_not_found(error_codes::NAME_UNKNOWN, "repository not found"),
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    // Validate media type
    let content_type = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if !media_types::MANIFEST_TYPES.contains(&content_type) {
        // Try OCI types too
        if content_type != media_types::OCI_MANIFEST_V1
            && content_type != media_types::OCI_INDEX_V1
            && content_type != media_types::MANIFEST_V2
            && content_type != media_types::MANIFEST_LIST_V2
        {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::MANIFEST_INVALID,
                "unsupported manifest media type",
            );
        }
    }

    // Parse manifest
    let parsed = match ParsedManifest::parse(body.as_bytes()) {
        Ok(p) => p,
        Err(e) => {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::MANIFEST_INVALID,
                &format!("invalid manifest: {e}"),
            );
        }
    };

    let referenced_blobs = parsed.referenced_blobs();

    // Verify all referenced blobs exist
    for blob_digest in &referenced_blobs {
        let exists = match state
            .oci_storage
            .blob_exists(&owner, &repo, blob_digest)
            .await
        {
            Ok(exists) => exists,
            Err(error) => {
                return oci_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    &format!("{error:#}"),
                );
            }
        };
        if !exists {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::MANIFEST_BLOB_UNKNOWN,
                &format!("blob {} not found", blob_digest),
            );
        }
    }

    // Store manifest on disk, under a lease on the key it is stored at.
    //
    // `store_manifest` deduplicates the same way `finalize_upload` does, and
    // `published` is decided the same non-atomic way — so two concurrent first
    // pushes of one manifest digest can hand the loser's rollback a key the
    // winner's row already points at. The lease is what makes `published` mean
    // what the rollback below reads it as.
    let manifest_key = match state
        .oci_storage
        .manifest_storage_key(&owner, &repo, &parsed.digest)
    {
        Ok(key) => key,
        Err(e) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{e:#}"),
            );
        }
    };
    let lease = match acquire_publication_lease(&state.db, &manifest_key).await {
        Ok(lease) => lease,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };
    let response = put_manifest_under_lease(
        &state,
        &owner,
        &repo,
        &reference,
        &parsed,
        content_type,
        &body,
        oci_repo.id,
        user_id,
        &referenced_blobs,
        &lease,
    )
    .await;
    release_publication_lease(&state.db, &lease).await;
    response
}

/// Store the manifest object and record the row that publishes it, with this
/// request holding the lease on the manifest's content-addressed key.
#[allow(clippy::too_many_arguments)]
async fn put_manifest_under_lease(
    state: &AppState,
    owner: &str,
    repo: &str,
    reference: &str,
    parsed: &ParsedManifest,
    content_type: &str,
    body: &str,
    oci_repo_id: i64,
    user_id: Option<i64>,
    referenced_blobs: &[String],
    lease: &PublicationLease,
) -> Response {
    let stored_manifest = match state
        .oci_storage
        .store_manifest(owner, repo, &parsed.digest, body.as_bytes())
        .await
    {
        Ok(stored) => stored,
        Err(e) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{e:#}"),
            );
        }
    };

    let rf = Reference::parse(reference);
    let tag = if rf.is_tag() {
        match &rf {
            Reference::Tag(t) => Some(t.as_str()),
            _ => None,
        }
    } else {
        None
    };

    // Insert or update manifest in DB. Digest references are content-addressed;
    // tag writes keep lookup, replacement and blob-reference claims in the same
    // transaction, so no database failure can turn into an INSERT or leave a
    // tag deleted after a failed move.
    let manifest_write = if let Some(tag) = tag {
        rg_db::ops::oci_ops::upsert_tag_manifest(
            &state.db,
            oci_repo_id,
            tag,
            &parsed.digest,
            content_type,
            parsed.size as i64,
            body,
            parsed.manifest.schema_version as i32,
            user_id,
            referenced_blobs,
        )
        .await
        .map(|_| ())
    } else {
        rg_db::ops::oci_ops::insert_digest_manifest(
            &state.db,
            oci_repo_id,
            &parsed.digest,
            content_type,
            parsed.size as i64,
            body,
            parsed.manifest.schema_version as i32,
            user_id,
            referenced_blobs,
        )
        .await
        .map(|_| ())
    };

    match manifest_write {
        Ok(_) => {}
        Err(e) => {
            tracing::error!(
                owner,
                repo,
                reference,
                digest = %parsed.digest,
                error = %format!("{e:#}"),
                "failed to record OCI manifest"
            );
            rollback_unrecorded_manifest(
                state,
                owner,
                repo,
                oci_repo_id,
                &parsed.digest,
                &stored_manifest,
                lease,
            )
            .await;
            return oci_err(oci_status_for(&e), "UNKNOWN", "failed to record manifest");
        }
    }

    let docker_digest = format!(
        "{}:{}",
        parsed.digest.split(':').next().unwrap_or("sha256"),
        parsed.digest.split(':').nth(1).unwrap_or(&parsed.digest),
    );

    (
        StatusCode::CREATED,
        [
            (DOCKER_CONTENT_DIGEST, docker_digest.as_str()),
            (
                header::LOCATION,
                format!(
                    "/v2/{owner}/{repo}/manifests/{digest}",
                    digest = parsed.digest
                )
                .as_str(),
            ),
        ],
        String::new(),
    )
        .into_response()
}

/// Remove a manifest object published by a request whose database transaction failed.
///
/// Three things have to hold before the delete is safe, and each is a way this
/// could destroy a live manifest:
///
/// * the object was published by *this* request — a content-addressed retry may
///   find bytes from an earlier successful push, and those are never ours;
/// * the lease on the key was granted, not taken over from a holder that may
///   still be running;
/// * no manifest row currently claims the digest, which covers the request that
///   committed the same digest while this one was failing.
///
/// Anything short of all three keeps the object. An orphan costs disk; deleting
/// a manifest a live row points at makes an image unpullable with its metadata
/// intact.
async fn rollback_unrecorded_manifest(
    state: &AppState,
    owner: &str,
    repo: &str,
    oci_repo_id: i64,
    digest: &str,
    stored: &StoredManifest,
    lease: &PublicationLease,
) {
    if !stored.published {
        return;
    }
    if !lease.exclusive() {
        tracing::warn!(
            %owner,
            %repo,
            %digest,
            storage_path = %stored.storage_path,
            "orphaned OCI manifest: this request published under a taken-over lease and cannot prove the stored object is its own — it stays in storage rather than risk deleting a live manifest"
        );
        return;
    }

    match rg_db::ops::oci_ops::find_manifest_by_digest(&state.db, oci_repo_id, digest).await {
        Ok(Some(_)) => return,
        Ok(None) => {}
        Err(error) => {
            tracing::warn!(
                %owner,
                %repo,
                %digest,
                storage_path = %stored.storage_path,
                error = %error,
                "possibly orphaned OCI manifest: the database recheck failed, so rollback kept the object"
            );
            return;
        }
    }

    let cleanup = state
        .oci_storage
        .discard_published_object(&stored.storage_path)
        .await
        .err()
        .map(|error| format!("{error:#}"));
    if let Some(reason) = cleanup {
        tracing::warn!(
            %owner,
            %repo,
            %digest,
            storage_path = %stored.storage_path,
            error = %reason,
            "orphaned OCI manifest: the oci_manifests row was not created and the rollback delete failed too"
        );
    }
}

// ── Blob ─────────────────────────────────────────────────────

/// `HEAD /v2/{owner}/{repo}/blobs/{digest}` — check blob existence.
pub async fn head_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, digest)): Path<(String, String, String)>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "pull").await {
        return resp;
    }

    let exists = match state.oci_storage.blob_exists(&owner, &repo, &digest).await {
        Ok(exists) => exists,
        Err(error) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{error:#}"),
            );
        }
    };
    if exists {
        // The metadata row is the registry's source of truth for the size. Bytes
        // without that row are an inconsistent object, not a zero-byte blob.
        let oci_repo = match find_oci_repo(&state.db, &owner, &repo).await {
            Ok(Some(oci_repo)) => oci_repo,
            Ok(None) => {
                return oci_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    &format!(
                        "OCI repository metadata is missing for {owner}/{repo}, but blob {digest} exists in storage"
                    ),
                );
            }
            Err(error) => {
                return oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}"));
            }
        };
        let size = match rg_db::ops::oci_ops::find_blob(&state.db, oci_repo.id, &digest).await {
            Ok(Some(blob)) => blob.size,
            Ok(None) => {
                return oci_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    &format!(
                        "OCI blob metadata is missing for {owner}/{repo}@{digest}, but the blob exists in storage"
                    ),
                );
            }
            Err(error) => {
                return oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}"));
            }
        };
        let size_hdr = size.to_string();
        (
            StatusCode::OK,
            [
                (header::CONTENT_LENGTH, size_hdr.as_str()),
                (DOCKER_CONTENT_DIGEST, digest.as_str()),
            ],
            String::new(),
        )
            .into_response()
    } else {
        oci_not_found(error_codes::BLOB_UNKNOWN, "blob not found")
    }
}

/// `GET /v2/{owner}/{repo}/blobs/{digest}` — pull blob (download layer).
/// Streams the blob file directly — never loads the entire blob into memory.
pub async fn get_blob(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, digest)): Path<(String, String, String)>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "pull").await {
        return resp;
    }

    match state.oci_storage.blob_local_path(&owner, &repo, &digest) {
        Ok(Some(path)) => match tokio::fs::File::open(&path).await {
            Ok(file) => {
                let size = match file.metadata().await {
                    Ok(m) => m.len(),
                    Err(error) => {
                        let message = blob_path_error("OCI blob", &path, &error);
                        // This handler keeps the OCI error envelope and so does
                        // not route through `AppError` — nothing else logs it.
                        tracing::error!(error = %message, "failed to stat OCI blob");
                        return oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &message);
                    }
                };
                let stream = tokio_util::io::ReaderStream::new(file);
                let stream_body =
                    http_body_util::StreamBody::new(futures::StreamExt::map(stream, |item| {
                        item.map(http_body::Frame::data)
                    }));
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "application/octet-stream"),
                        (header::CONTENT_LENGTH, size.to_string().as_str()),
                        (DOCKER_CONTENT_DIGEST, digest.as_str()),
                    ],
                    Body::new(stream_body),
                )
                    .into_response()
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                oci_not_found(error_codes::BLOB_UNKNOWN, "blob not found")
            }
            Err(error) => {
                let message = blob_path_error("OCI blob", &path, &error);
                tracing::error!(error = %message, "failed to open OCI blob");
                oci_err(StatusCode::INTERNAL_SERVER_ERROR, "UNKNOWN", &message)
            }
        },
        Ok(None) => match state.oci_storage.read_blob(&owner, &repo, &digest).await {
            Ok(data) => {
                // Remote (non-local) OCI storage returns the whole blob as a
                // `Vec`; the local-path branch above already streams from disk.
                // Serve the buffer as a backpressure-sensitive, idle-guarded
                // stream so a slow/stalled client can't pin the blob-sized `Vec`
                // in server memory until the kernel resets the dead connection
                // (card_444e03f1ca15).
                let len = data.len();
                (
                    StatusCode::OK,
                    [
                        (header::CONTENT_TYPE, "application/octet-stream"),
                        (header::CONTENT_LENGTH, len.to_string().as_str()),
                        (DOCKER_CONTENT_DIGEST, digest.as_str()),
                    ],
                    crate::http_stream::buffered_body_with_idle(data, state.git_idle_timeout_secs),
                )
                    .into_response()
            }
            // Only a blob the registry genuinely does not have is BLOB_UNKNOWN.
            // Discarding the error made a blob store that is present but
            // unreachable indistinguishable from a missing layer, and `docker
            // pull` reports that as a broken image — sending the operator to
            // inspect the manifest while the storage is what is down.
            Err(error) if error.downcast_ref::<rg_core::error::NotFound>().is_some() => {
                oci_not_found(error_codes::BLOB_UNKNOWN, "blob not found")
            }
            Err(error) => oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}")),
        },
        Err(error) => oci_err(
            StatusCode::BAD_REQUEST,
            error_codes::DIGEST_INVALID,
            &format!("{error:#}"),
        ),
    }
}

// ── Upload ───────────────────────────────────────────────────

/// `POST /v2/{owner}/{repo}/blobs/uploads/` — start a blob upload session.
pub async fn start_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo)): Path<(String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "push").await {
        return resp;
    }

    // Check for cross-repo mount
    if let (Some(mount), Some(from)) = (params.get("mount"), params.get("from")) {
        return handle_mount(&state, &headers, &owner, &repo, mount, from).await;
    }

    let oci_repo = match find_or_create_oci_repo(&state.db, &owner, &repo).await {
        Ok(r) => r,
        Err(e) => return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    };

    match state.oci_storage.create_upload(&owner, &repo).await {
        Ok((uuid, upload_path)) => {
            // Record upload in DB
            if let Err(e) =
                rg_db::ops::oci_ops::create_upload(&state.db, oci_repo.id, &uuid, &upload_path)
                    .await
            {
                return oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}"));
            }

            let location = format!("/v2/{owner}/{repo}/blobs/uploads/{uuid}");
            (
                StatusCode::ACCEPTED,
                [
                    (header::LOCATION, location.as_str()),
                    (RANGE, "0-0"),
                    (DOCKER_UPLOAD_UUID, uuid.as_str()),
                ],
                String::new(),
            )
                .into_response()
        }
        Err(e) => oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!("{e:#}"),
        ),
    }
}

/// Handle cross-repository blob mount.
async fn handle_mount(
    state: &AppState,
    headers: &HeaderMap,
    owner: &str,
    repo: &str,
    mount_digest: &str,
    from: &str,
) -> Response {
    let (from_owner, from_repo) = match parse_namespace(from) {
        Some(p) => p,
        None => {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::NAME_INVALID,
                "invalid mount source",
            );
        }
    };

    if let Err(resp) = require_access(state, headers, from_owner, from_repo, "pull").await {
        return resp;
    }
    if let Err(resp) = require_access(state, headers, owner, repo, "push").await {
        return resp;
    }

    // Check source blob exists. A backend that cannot answer is not the same as
    // an answer of "no": `unwrap_or(false)` turned an unreachable blob store
    // into a 404, which tells the pushing client the source image is missing
    // layers. `head_blob` and `put_manifest` already keep the two apart.
    match state
        .oci_storage
        .blob_exists(from_owner, from_repo, mount_digest)
        .await
    {
        Ok(true) => {}
        Ok(false) => {
            return oci_not_found(error_codes::BLOB_UNKNOWN, "mount source blob not found");
        }
        Err(error) => {
            return oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}"));
        }
    }

    // The row that owns the mounted blob must exist before bytes are copied.
    // Otherwise a repository-row failure leaves an object behind that no OCI
    // read or reclamation path can discover.
    let oci_repo = match find_or_create_oci_repo(&state.db, owner, repo).await {
        Ok(repo) => repo,
        Err(error) => return oci_err(oci_status_for(&error), "UNKNOWN", &format!("{error:#}")),
    };

    // Copy blob file via hardlink (or fallback to streaming copy) — avoids
    // memory copy. Same publication protocol as a finalized upload: a mount
    // publishes into the destination repository's content-addressed key, so a
    // concurrent push of that digest is racing for the same bytes.
    match publish_blob(
        &state.db,
        &state.oci_storage,
        oci_repo.id,
        owner,
        repo,
        mount_digest,
        BlobSource::Mount {
            from_owner,
            from_repo,
        },
    )
    .await
    {
        Ok(blob) => {
            let digest = blob.digest;
            let location = format!("/v2/{owner}/{repo}/blobs/{digest}");
            (
                StatusCode::CREATED,
                [
                    (header::LOCATION, location.as_str()),
                    (DOCKER_CONTENT_DIGEST, digest.as_str()),
                ],
                String::new(),
            )
                .into_response()
        }
        // A copy that could not run and a row that could not be written are
        // both the registry's fault, but a database outage and a key another
        // push is still holding are retryable and a flat `500` says they are
        // not — ask the error which.
        Err(e) => oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}")),
    }
}

/// Fetch an upload session and re-anchor it to the repository the caller was
/// authorized for.
///
/// `{uuid}` is an instance-wide key: `oci_upload` rows are found by it alone,
/// while `require_access` only ever proves something about the `{owner}/{repo}`
/// in the path. Resolving the session without tying the two together let a
/// caller with `push` on *any* repository name somebody else's active session
/// and drive the writes that follow — the progress row and, on finalize, the
/// row's deletion. The bytes never crossed over (the staging path is built from
/// `{owner}/{repo}`), so what leaked was not content but control: the victim's
/// client was handed a `Range` for a session it no longer owned.
///
/// A session in another repository answers exactly like one that never existed
/// — `404 BLOB_UPLOAD_UNKNOWN` — because the alternative confirms the uuid to
/// whoever guessed it, and OCI clients already know that code.
///
/// It is a named helper rather than an inline comparison because
/// `global_id_anchor_guard` can read a call and cannot read an `if`.
async fn upload_in_repo(
    state: &AppState,
    owner: &str,
    repo: &str,
    uuid: &str,
) -> Result<rg_db::entities::oci_upload::Model, Response> {
    let unknown = || oci_not_found(error_codes::BLOB_UPLOAD_UNKNOWN, "upload session not found");

    // No `oci_repository` row means no session can belong here — including the
    // case where the repository itself is gone. `find_oci_repo` never creates.
    let oci_repo = match find_oci_repo(&state.db, owner, repo).await {
        Ok(Some(oci_repo)) => oci_repo,
        Ok(None) => return Err(unknown()),
        Err(e) => return Err(oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}"))),
    };

    match rg_db::ops::oci_ops::find_upload(&state.db, uuid).await {
        Ok(Some(upload))
            if upload.oci_repository_id == oci_repo.id
                && upload.expires_at > chrono::Utc::now() =>
        {
            Ok(upload)
        }
        Ok(_) => Err(unknown()),
        Err(e) => Err(oci_err(oci_status_for(&e), "UNKNOWN", &format!("{e:#}"))),
    }
}

/// `GET /v2/{owner}/{repo}/blobs/uploads/{uuid}` — report resumable-upload state.
///
/// Distribution clients use this endpoint after a `416` to recover the
/// registry's acknowledged offset. The row is re-read under the same staging
/// lock as `PATCH`, so a status response cannot observe an append before its
/// progress write or a progress write after a concurrent session removal.
pub async fn get_upload_status(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, uuid)): Path<(String, String, String)>,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "push").await {
        return resp;
    }

    // Refuse an unknown, expired or differently-anchored session before
    // deriving and opening a path from the caller-supplied uuid.
    if let Err(resp) = upload_in_repo(&state, &owner, &repo, &uuid).await {
        return resp;
    }

    let file_path = state.oci_storage.upload_file(&owner, &repo, &uuid);
    let _upload_guard = match acquire_upload_file_lock(&file_path).await {
        Ok(guard) => guard,
        Err(error) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{error:#}"),
            );
        }
    };
    let upload = match upload_in_repo(&state, &owner, &repo, &uuid).await {
        Ok(upload) => upload,
        Err(resp) => return resp,
    };
    let staged_size = match staged_upload_size(&file_path).await {
        Ok(size) => size,
        Err(error) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{error:#}"),
            );
        }
    };
    if staged_size != upload.bytes_uploaded {
        return oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!(
                "the staged upload does not match this session: it recorded {} byte(s), but {} \
                 holds {staged_size} byte(s)",
                upload.bytes_uploaded,
                file_path.display()
            ),
        );
    }

    upload_progress_response(
        StatusCode::NO_CONTENT,
        &owner,
        &repo,
        &uuid,
        upload.bytes_uploaded,
    )
}

/// `PATCH /v2/{owner}/{repo}/blobs/uploads/{uuid}` — chunked upload.
/// Streams the request body directly to the upload file—never buffers
/// the entire chunk in memory.
pub async fn chunk_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, uuid)): Path<(String, String, String)>,
    body: Body,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "push").await {
        return resp;
    }

    // Verify the upload session exists *in the gated repository*.
    match upload_in_repo(&state, &owner, &repo, &uuid).await {
        Ok(_) => {}
        Err(resp) => return resp,
    }

    let file_path = state.oci_storage.upload_file(&owner, &repo, &uuid);
    let _upload_guard = match acquire_upload_file_lock(&file_path).await {
        Ok(guard) => guard,
        Err(error) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{error:#}"),
            );
        }
    };
    // A retry may have waited behind the request whose response it lost. The
    // model fetched before the lock is then stale, so refresh the acknowledged
    // offset while the staging file is exclusively ours.
    let upload = match upload_in_repo(&state, &owner, &repo, &uuid).await {
        Ok(upload) => upload,
        Err(resp) => return resp,
    };
    let requested_range = match parse_upload_content_range(&headers) {
        Ok(range) => range,
        Err(message) => {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::BLOB_UPLOAD_INVALID,
                message,
            );
        }
    };

    if let Some(range) = requested_range {
        if content_length(&headers) != Some(range.len()) {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::SIZE_INVALID,
                "Content-Length must match the inclusive Content-Range",
            );
        }
    }

    // The row is the acknowledged offset; the file is what finalization will
    // hash. Refuse to resume from either one when they disagree. Otherwise a
    // request that is correct for the row can silently append to bytes the row
    // never acknowledged, recreating the very doubled staging file this range
    // check is meant to prevent.
    let staged_size = match staged_upload_size(&file_path).await {
        Ok(size) => size,
        Err(error) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{error:#}"),
            );
        }
    };
    let recorded_size = upload.bytes_uploaded;
    if staged_size != recorded_size {
        return oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!(
                "the staged upload does not match this session: it recorded {recorded_size} \
                 byte(s), but {} holds {staged_size} byte(s)",
                file_path.display()
            ),
        );
    }

    match requested_range {
        // A range wholly inside acknowledged bytes is a retry. Compare the
        // body to those bytes rather than trusting coordinates alone: a
        // different payload under an old range is out of order, not an
        // idempotent request.
        Some(range) if range.end < recorded_size => {
            return match body_matches_staged_range(body, &file_path, range).await {
                Ok(true) => upload_progress_response(
                    StatusCode::ACCEPTED,
                    &owner,
                    &repo,
                    &uuid,
                    recorded_size,
                ),
                Ok(false) => upload_progress_response(
                    StatusCode::RANGE_NOT_SATISFIABLE,
                    &owner,
                    &repo,
                    &uuid,
                    recorded_size,
                ),
                Err(error) => oci_err(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "UNKNOWN",
                    &format!("{error:#}"),
                ),
            };
        }
        // New bytes must start at the first byte the session has not accepted.
        // This catches both a gap and a partial overlap before the body can
        // touch staging.
        Some(range) if range.start != recorded_size => {
            return upload_progress_response(
                StatusCode::RANGE_NOT_SATISFIABLE,
                &owner,
                &repo,
                &uuid,
                recorded_size,
            );
        }
        // Legacy streaming PATCHes omit Content-Range. Keep the one-shot form
        // real clients use, but only for an empty upload: without coordinates
        // a later request cannot distinguish a new chunk from a retry.
        None if recorded_size != 0 => {
            return upload_progress_response(
                StatusCode::RANGE_NOT_SATISFIABLE,
                &owner,
                &repo,
                &uuid,
                recorded_size,
            );
        }
        Some(_) | None => {}
    }

    // The request is the next chunk. Stream only after every offset check, so
    // a refusal cannot change the staging file.
    match stream_body_to_file(body, &file_path).await {
        Ok(staged) => {
            let total_size = staged.total;
            if let Some(range) = requested_range {
                if staged.written != range.len() || total_size != range.end + 1 {
                    return oci_err(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        "UNKNOWN",
                        &format!(
                            "the accepted OCI chunk did not land at its declared range: \
                             Content-Range {}-{}, appended {} byte(s), staging now holds \
                             {total_size} byte(s)",
                            range.start, range.end, staged.written
                        ),
                    );
                }
            }

            // The `Range` below tells the client where to resume from. Reporting
            // it while the session row still holds the old offset hands the
            // client a position the server does not agree with, so a failed
            // progress write has to fail the chunk rather than be swallowed.
            match rg_db::ops::oci_ops::update_upload_progress(
                &state.db,
                upload.oci_repository_id,
                &uuid,
                total_size,
            )
            .await
            {
                Ok(0) => {
                    // The row was there a moment ago and matched this
                    // repository, so nothing was written because the session
                    // has since gone (expiry sweep, a concurrent finalize).
                    // Reporting a `Range` off a row that no longer exists is
                    // the same lie as reporting one off a stale row.
                    return oci_not_found(
                        error_codes::BLOB_UPLOAD_UNKNOWN,
                        "upload session not found",
                    );
                }
                Ok(_) => {}
                Err(e) => {
                    return oci_err(
                        oci_status_for(&e),
                        "UNKNOWN",
                        &format!("failed to record upload progress for session {uuid}: {e}"),
                    );
                }
            }

            upload_progress_response(StatusCode::ACCEPTED, &owner, &repo, &uuid, total_size)
        }
        Err(e) => oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!("{e:#}"),
        ),
    }
}

#[derive(Clone, Copy)]
struct UploadContentRange {
    start: i64,
    end: i64,
}

impl UploadContentRange {
    fn len(self) -> i64 {
        self.end - self.start + 1
    }
}

/// OCI chunk ranges are inclusive and deliberately do not carry HTTP's
/// `bytes ` prefix: the wire grammar is exactly `<decimal>-<decimal>`.
fn parse_upload_content_range(
    headers: &HeaderMap,
) -> Result<Option<UploadContentRange>, &'static str> {
    let Some(value) = headers.get(header::CONTENT_RANGE) else {
        return Ok(None);
    };
    let value = value
        .to_str()
        .map_err(|_| "Content-Range must contain ASCII decimal offsets")?;
    let Some((start, end)) = value.split_once('-') else {
        return Err("Content-Range must have the form <start>-<end>");
    };
    if start.is_empty()
        || end.is_empty()
        || !start.bytes().all(|byte| byte.is_ascii_digit())
        || !end.bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err("Content-Range must have the form <start>-<end>");
    }
    let start = start
        .parse::<i64>()
        .map_err(|_| "Content-Range start is too large")?;
    let end = end
        .parse::<i64>()
        .map_err(|_| "Content-Range end is too large")?;
    if end < start {
        return Err("Content-Range end must not precede its start");
    }
    end.checked_sub(start)
        .and_then(|width| width.checked_add(1))
        .ok_or("Content-Range is too large")?;
    Ok(Some(UploadContentRange { start, end }))
}

fn content_length(headers: &HeaderMap) -> Option<i64> {
    headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
}

fn upload_progress_response(
    status: StatusCode,
    owner: &str,
    repo: &str,
    uuid: &str,
    total_size: i64,
) -> Response {
    let range_end = total_size.saturating_sub(1).max(0);
    let location = format!("/v2/{owner}/{repo}/blobs/uploads/{uuid}");
    (
        status,
        [
            (header::LOCATION, location.as_str()),
            (RANGE, format!("0-{range_end}").as_str()),
            (DOCKER_UPLOAD_UUID, uuid),
        ],
        String::new(),
    )
        .into_response()
}

fn upload_file_error(file_path: &std::path::Path, error: &std::io::Error) -> anyhow::Error {
    rg_core::platform::fs::path_error("OCI upload file", file_path, error, OCI_UPLOAD_STORAGE_HINT)
}

/// Cross-task/process exclusion for one live staging file.
///
/// The range check, append and progress update are one protocol operation. If
/// two handlers check the same old offset before either writes, append mode
/// faithfully serializes their individual `write_all` calls but still stores
/// both bodies. An advisory lock on the data file makes the second handler
/// refresh the session only after the first has acknowledged its bytes.
struct UploadFileLock {
    _file: std::fs::File,
}

async fn acquire_upload_file_lock(file_path: &std::path::Path) -> anyhow::Result<UploadFileLock> {
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(file_path)
        .await
        .map_err(|error| upload_file_error(file_path, &error))?
        .into_std()
        .await;
    loop {
        match fs2::FileExt::try_lock_exclusive(&file) {
            Ok(()) => return Ok(UploadFileLock { _file: file }),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                // An async wait keeps a slow first upload from occupying a
                // Tokio blocking-pool thread per retry.
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "failed to lock OCI upload file {} for a range update: {error}",
                    file_path.display()
                ));
            }
        }
    }
}

async fn staged_upload_size(file_path: &std::path::Path) -> anyhow::Result<i64> {
    let metadata = tokio::fs::metadata(file_path)
        .await
        .map_err(|error| upload_file_error(file_path, &error))?;
    Ok(metadata.len() as i64)
}

/// Compare a retried request with an already-staged inclusive range without
/// buffering the chunk. A byte-identical retry is safe to acknowledge; a
/// different or differently-sized body is an out-of-order request.
async fn body_matches_staged_range(
    body: Body,
    file_path: &std::path::Path,
    range: UploadContentRange,
) -> anyhow::Result<bool> {
    let mut file = tokio::fs::File::open(file_path)
        .await
        .map_err(|error| upload_file_error(file_path, &error))?;
    file.seek(std::io::SeekFrom::Start(range.start as u64))
        .await
        .map_err(|error| upload_file_error(file_path, &error))?;

    use futures::StreamExt;
    let mut stream = body.into_data_stream();
    let mut remaining = range.len();
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|error| anyhow::anyhow!("body stream error: {error}"))?;
        if data.len() as i64 > remaining {
            return Ok(false);
        }
        let mut staged = vec![0_u8; data.len()];
        file.read_exact(&mut staged)
            .await
            .map_err(|error| upload_file_error(file_path, &error))?;
        if staged.as_slice() != data.as_ref() {
            return Ok(false);
        }
        remaining -= data.len() as i64;
    }
    Ok(remaining == 0)
}

/// `PUT /v2/{owner}/{repo}/blobs/uploads/{uuid}?digest=sha256:...` — finalize upload.
/// If the body contains data (single-chunk upload), streams it to the upload file first.
pub async fn complete_upload(
    State(state): State<AppState>,
    headers: HeaderMap,
    Path((owner, repo, uuid)): Path<(String, String, String)>,
    Query(params): Query<std::collections::HashMap<String, String>>,
    body: Body,
) -> Response {
    if let Err(resp) = require_access(&state, &headers, &owner, &repo, "push").await {
        return resp;
    }
    let expected_digest = match params.get("digest") {
        Some(d) => d.clone(),
        None => {
            return oci_err(
                StatusCode::BAD_REQUEST,
                error_codes::DIGEST_INVALID,
                "digest parameter required",
            );
        }
    };

    // The session this finalizes must belong to the gated repository. The
    // staging file below is keyed by `{owner}/{repo}` and so was never anybody
    // else's, but the row is not: finalizing ends by deleting the session, and
    // keyed on the uuid alone that delete landed on whichever repository held
    // it — a stranger's `docker push` cancelled from outside.
    let upload = match upload_in_repo(&state, &owner, &repo, &uuid).await {
        Ok(upload) => upload,
        Err(resp) => return resp,
    };

    // If body is provided (single-chunk upload), stream it to the upload file first
    // Check by reading the first frame: if there's data, stream the rest
    let file_path = state.oci_storage.upload_file(&owner, &repo, &uuid);
    let staged = match stream_body_to_file(body, &file_path).await {
        Ok(staged) => staged,
        Err(e) => {
            return oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!("{e:#}"),
            );
        }
    };

    // What the session says should be on disk, against what is. Every byte that
    // reached the staging file came through a `PATCH` that recorded its new
    // offset before answering, or through the append just above, so the two
    // numbers are the same number on every push that works.
    //
    // They are not merely a sanity check. Finalizing hashes whatever the file
    // holds, and a file holding something other than what the session received
    // hashes to something other than the digest — which is indistinguishable,
    // at the point where the error is classified, from a client that named the
    // wrong digest for its layer. So the registry answered `400 digest invalid`
    // for a staging file that had gone missing or been written twice: a failure
    // of ours, reported as corrupt input, to a client that does not retry a
    // `4xx`. Measured here rather than inferred later, because after the hash
    // the evidence is gone.
    let recorded = upload.bytes_uploaded;
    let staged_is_accountable = staged.existed && staged.total == recorded + staged.written;

    // The repository the session was anchored to, not a second lookup that
    // could resolve — or create — a different one.
    let oci_repo_id = upload.oci_repository_id;

    // Finalize and publish: hash the staged file, move it to its
    // content-addressed key, and record the row that makes it reachable — all
    // under a lease on that key, so a failure here can tell its own bytes from
    // bytes a concurrent push of the same digest is entitled to. Answering
    // `201 Created` without the row is how a push "succeeds" into a repository
    // whose very next `HEAD .../blobs/` returns 404, so the failure has to
    // reach the client either way.
    match publish_blob(
        &state.db,
        &state.oci_storage,
        oci_repo_id,
        &owner,
        &repo,
        &expected_digest,
        BlobSource::Upload { uuid: &uuid },
    )
    .await
    {
        Ok(blob) => {
            let digest = blob.digest;
            // Clean up upload session. The blob is committed at this point, so a
            // failure here leaks a session row rather than losing data — worth a
            // warning, not a failed push.
            if let Err(e) = rg_db::ops::oci_ops::delete_upload(&state.db, oci_repo_id, &uuid).await
            {
                tracing::warn!(
                    upload_uuid = %uuid,
                    error = %format!("{e:#}"),
                    "failed to delete the OCI upload session after committing its blob; \
                     the session row is orphaned"
                );
            }

            let location = format!("/v2/{owner}/{repo}/blobs/{digest}");
            (
                StatusCode::CREATED,
                [
                    (header::LOCATION, location.as_str()),
                    (DOCKER_CONTENT_DIGEST, digest.as_str()),
                ],
                String::new(),
            )
                .into_response()
        }
        // A digest fault over bytes the session cannot account for is not the
        // client's fault, whatever the error says. The client sent `recorded +
        // written` bytes and the registry hashed something else, so the layer
        // it named was never what got hashed — blaming the digest here is
        // blaming the one party that did nothing wrong.
        //
        // Loud on both channels on purpose. The client gets a 5xx it will
        // retry, and the operator gets the three numbers, because the reason
        // the staging file diverged is not visible from inside this handler and
        // the next occurrence is the only place it can be read off.
        Err(e) if is_client_digest_fault(&e) && !staged_is_accountable => {
            tracing::error!(
                upload_uuid = %uuid,
                staging_file = %file_path.display(),
                staging_file_existed = staged.existed,
                recorded_bytes = recorded,
                appended_bytes = staged.written,
                staged_bytes = staged.total,
                error = %format!("{e:#}"),
                "OCI blob finalize hashed a staging file that does not match its upload session; \
                 answering 500 rather than blaming the client's digest"
            );
            oci_err(
                StatusCode::INTERNAL_SERVER_ERROR,
                "UNKNOWN",
                &format!(
                    "the staged upload does not match this session: it recorded {recorded} \
                     byte(s), this request appended {}, and the staging file {} {} byte(s). \
                     The digest cannot be blamed for bytes that changed behind the session: {e:#}",
                    staged.written,
                    if staged.existed {
                        "held"
                    } else {
                        "was not there and was recreated holding"
                    },
                    staged.total,
                ),
            )
        }
        // Finalizing fails for three unrelated reasons and only one of them is
        // the client's: a digest that does not match the bytes. An unreadable
        // staging file (`_oci_uploads/` gone or not writable) and a blob store
        // that refuses the publish are both ours. `docker push` does not retry
        // a 400 — it prints `digest invalid` and stops — so answering one for a
        // broken `repo_root` tells the operator to go inspect an image that was
        // never the problem. Ask the error which side is at fault.
        Err(e) if is_client_digest_fault(&e) => oci_err(
            StatusCode::BAD_REQUEST,
            error_codes::DIGEST_INVALID,
            &format!("{e:#}"),
        ),
        Err(e) => oci_err(
            StatusCode::INTERNAL_SERVER_ERROR,
            "UNKNOWN",
            &format!("{e:#}"),
        ),
    }
}

// ── stream helper ──────────────────────────────────────────────

/// What one append to a staging file did, and what it found there.
///
/// `total` alone was the whole return value, and it is the one number that
/// cannot answer the question the caller has to ask: whether the bytes on disk
/// are the bytes this session accounts for. A staging file that vanished
/// between two requests and one that was written twice both come back with a
/// `total` that reads perfectly well on its own — see the invariant in
/// [`complete_upload`].
struct StagedWrite {
    /// Whether the staging file was already there when this request opened it.
    ///
    /// `POST .../blobs/uploads/` creates it empty, so by the time any other
    /// handler touches it the answer is always `true` on a healthy instance.
    /// A `false` here means the file went away under an open session.
    existed: bool,
    /// Bytes this request appended.
    written: i64,
    /// Size of the staging file after the append.
    total: i64,
}

/// Stream an Axum `Body` to a file, appending to any existing content.
/// Never buffers the entire body in memory—each frame is written directly.
/// Returns what the append did and what it found — see [`StagedWrite`].
///
/// This is the write path of every `docker push`: the staging path is derived
/// from `repo_root` plus a generated upload UUID, so a bare `?` on the io error
/// hands the client an errno and nothing else. Every failure names the file.
async fn stream_body_to_file(
    body: Body,
    file_path: &std::path::Path,
) -> anyhow::Result<StagedWrite> {
    let staged = |error: &std::io::Error| upload_file_error(file_path, error);

    // Read before the open, not after: `create(true)` below is what makes the
    // difference invisible, and it has to stay — a monolithic `PUT` with no
    // preceding `PATCH` is a legal push, and on an instance whose staging tree
    // was wiped the honest answer is still the one finalizing gives, not an
    // errno from here.
    let existed = tokio::fs::try_exists(file_path).await.unwrap_or(false);

    let mut file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(file_path)
        .await
        .map_err(|error| staged(&error))?;

    use futures::StreamExt;
    let mut stream = body.into_data_stream();
    let mut written = 0_i64;
    while let Some(chunk) = stream.next().await {
        let data = chunk.map_err(|e| anyhow::anyhow!("body stream error: {}", e))?;
        file.write_all(&data)
            .await
            .map_err(|error| staged(&error))?;
        written += data.len() as i64;
    }

    // `tokio::fs::File` buffers, and `write_all` returns once the bytes are
    // queued for the blocking pool — not once they are in the file. `metadata`
    // asks the *file*, so without this flush the size below is whatever had
    // landed by then: under load a chunk reads back as the offset before it,
    // and that number is both the `Range` the client resumes from and the
    // `bytes_uploaded` recorded for the session. The client then re-sends bytes
    // it already sent, and the push dies at the digest — as the client's fault.
    file.flush().await.map_err(|error| staged(&error))?;

    let total = file.metadata().await.map_err(|error| staged(&error))?.len() as i64;
    Ok(StagedWrite {
        existed,
        written,
        total,
    })
}

// ── DB helpers ────────────────────────────────────────────────

/// Find an OCI repository, auto-creating if it doesn't exist.
/// Uses the ForgeKeep repo as the owner.
async fn find_oci_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
) -> anyhow::Result<Option<rg_db::entities::oci_repository::Model>> {
    // Look up the ForgeKeep repository
    let forgekeep_repo = rg_core::repo::service::find_repo_by_owner_name(db, owner, repo).await?;
    match forgekeep_repo {
        Some(r) => {
            let oci_repo = rg_db::ops::oci_ops::find_repo_by_id(db, r.id).await?;
            Ok(oci_repo)
        }
        None => Ok(None),
    }
}

/// The `oci_repository` row for `owner/repo`, created on the first push.
///
/// The namespace is `owner/repo` — the path the client asked for — and it is
/// resolved as such. It used to be resolved against *the caller* instead
/// whenever one was known (`find_by_owner_and_name(actor_id, repo)`), which is
/// only the same query when the caller happens to be the owner. A collaborator
/// pushing the first image of a repository they do not own looked up
/// `their-id/repo`, found nothing, and got `500 repository not found` for a
/// push the gate had just allowed. Nobody noticed because the actor reaching
/// here was almost always `None`: docker authenticates with a scoped token, and
/// that branch of `check_access` returned no actor at all.
///
/// `owner_id` on the row means the namespace's owner, so it comes off the
/// repository, not off whoever happens to be pushing.
async fn find_or_create_oci_repo(
    db: &DatabaseConnection,
    owner: &str,
    repo: &str,
) -> anyhow::Result<rg_db::entities::oci_repository::Model> {
    let forgekeep_repo = rg_core::repo::service::find_repo_by_owner_name(db, owner, repo)
        .await?
        .ok_or_else(|| anyhow::anyhow!("repository {}/{} not found", owner, repo))?;

    let namespace = format!("{}/{}", owner, repo);
    rg_db::ops::oci_ops::find_or_create_repo(
        db,
        forgekeep_repo.id,
        &namespace,
        forgekeep_repo.owner_id,
    )
    .await
    .map_err(Into::into)
}

fn get_base_url(headers: &HeaderMap) -> String {
    headers
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(|host| {
            let scheme = if host.starts_with("localhost") || host.starts_with("127.") {
                "http"
            } else {
                "https"
            };
            format!("{}://{}", scheme, host)
        })
        .unwrap_or_else(|| "http://localhost".into())
}

#[cfg(test)]
mod blob_path_error_tests {
    use super::*;

    /// `docker pull` prints the OCI envelope message and nothing else, and this
    /// handler does not route through `AppError`, so a blob whose file is
    /// unreadable used to reach the operator as `failed to stat blob` — no
    /// path, no errno — or as an errno with the path stripped off.
    #[test]
    fn blob_failure_names_the_file_and_the_remedy() {
        let temp = tempfile::tempdir().unwrap();
        let missing = temp.path().join("blobs").join("sha256").join("deadbeef");
        let error = std::fs::File::open(&missing).unwrap_err();

        let rendered = blob_path_error("OCI blob", &missing, &error);

        assert!(
            rendered.contains(&missing.display().to_string()),
            "{rendered}"
        );
        assert!(rendered.contains("OCI blob"), "{rendered}");
        assert!(rendered.contains("[server].repo_root"), "{rendered}");
    }
}
