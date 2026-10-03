//! Personal Access Token / JWT authentication middleware.
//!
//! Bridges Personal Access Tokens (PATs) to the JWT-only API handlers and
//! extracts the authenticated actor for the Git-over-HTTP endpoints.

use axum::extract::State;
use axum::http::header;
use axum::response::{IntoResponse, Response};
use sea_orm::DatabaseConnection;

use crate::error;
use crate::AppState;

/// Resolve a Personal Access Token, honouring expiry and the owner's standing.
///
/// Returns the token together with the account it belongs to, so no caller has
/// to remember to look the owner up: a PAT is a standing delegation of that
/// account's rights, and deactivating the account has to revoke it. Resolving
/// the owner here rather than at each call site is what makes that true for
/// the REST middleware, git-over-HTTP and the registry's Basic auth at once.
pub(crate) async fn resolve_pat(
    db: &DatabaseConnection,
    token: &str,
) -> anyhow::Result<
    Option<(
        rg_db::entities::access_token::Model,
        rg_db::entities::user::Model,
    )>,
> {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(token.as_bytes());
    let hash = format!("{:x}", hasher.finalize());

    let Some(tok) = rg_db::ops::token_ops::find_by_hash(db, &hash).await? else {
        return Ok(None);
    };
    if let Some(expires_at) = tok.expires_at {
        if expires_at < chrono::Utc::now() {
            return Ok(None); // expired
        }
    }
    let Some(owner) =
        rg_db::ops::user_ops::finalize_standing_credential_owner(db, tok.user_id).await?
    else {
        return Ok(None);
    };
    if !owner.is_usable() {
        tracing::warn!(
            user_id = tok.user_id,
            token_id = tok.id,
            "rejecting personal access token: account is disabled or gone"
        );
        return Ok(None);
    }
    // A bot answers to the person who owns it and has no standing of its own:
    // disabling or retiring the owner has to stop the agent too, at every
    // door this resolver serves (card_60a80311d512).
    if !rg_core::user::bots::owner_is_usable(db, &owner).await? {
        tracing::warn!(
            user_id = tok.user_id,
            token_id = tok.id,
            "rejecting personal access token: the bot's owner is disabled or gone"
        );
        return Ok(None);
    }
    // Usage time is observability, not part of the credential proof. Match the
    // SSH/deploy-key contract: record every accepted credential, but do not
    // turn a write-only failure into either an invalid-token answer or an auth
    // outage. Lookup failures above still propagate because they leave the
    // credential's validity unknown.
    if let Err(error) = rg_db::ops::token_ops::touch_last_used(db, tok.id).await {
        tracing::warn!(
            token_id = tok.id,
            user_id = tok.user_id,
            error = %format!("{error:#}"),
            "failed to update personal access token usage time"
        );
    }
    Ok(Some((tok, owner)))
}

/// Scope required when a PAT is used for a REST request.
///
/// A scope only controls which API family the token may enter. Normal user,
/// repository and administrator authorization is still enforced by handlers.
///
/// The decision is made from the path *string*, because this middleware is an
/// outer layer: it runs before axum has matched anything, so a `MatchedPath`
/// does not exist yet and the route table cannot be consulted here. That is a
/// constraint, not a licence — the level each route requires is declared in
/// `route_table::Access`, and this function has to agree with it. It drifted
/// once already, which is why `/runners/register` is named literally below.
///
/// `pub` so that agreement can be *checked* rather than reviewed:
/// `route_access_sweep_tests::every_instance_admin_route_demands_the_admin_pat_scope`
/// walks the whole table and holds the two statements against each other in
/// both directions.
pub fn required_pat_scope(path: &str) -> Option<&'static str> {
    if path.starts_with("/api-docs") {
        return None;
    }
    // Axum may expose either the original URI or the path with the nested
    // `/api/v1` prefix stripped, depending on which router layer is running.
    let path = path.strip_prefix("/api/v1").unwrap_or(path);
    // `/runners/register` is the one administrative route that does not live
    // under `/admin`: it is what hands a runner its token, so the credential
    // that may call it is an instance-admin session and its declared level is
    // `Access::InstanceAdmin`. Named here rather than inferred, and checked
    // from both sides by the sweep test — if the route stops being
    // instance-admin, this line has to go with it.
    if path.starts_with("/admin") || path == "/runners/register" {
        return Some("admin");
    }
    if path.starts_with("/users")
        || path.starts_with("/notifications")
        || path.starts_with("/auth")
        || path == "/ws/notifications"
    {
        return Some("user");
    }
    Some("repo")
}

/// Axum middleware: translate a valid Personal Access Token into an equivalent
/// `Authorization: Bearer <JWT>` so the JWT-only API handlers accept PATs.
///
/// Requests already carrying a valid JWT, or no credentials, pass through
/// unchanged. The PAT may be presented as `Bearer <pat>`, `token <pat>`, or
/// HTTP Basic auth (`user:pat`).
///
/// A PAT also leaves its [`TokenGrant`] in the request extensions and publishes
/// its narrowing to `rg-core` for the rest of the request — see
/// [`crate::agent_scope`]. Two of its rules are decided right here, before any
/// routing, because they are about the *token* and not the route:
///
/// - a token confined to MCP tools is refused everywhere except the MCP
///   endpoint and the calls that endpoint dispatches (the endpoint itself
///   decides which tools);
/// - a bot account's requests are counted against its own budget.
pub(crate) async fn pat_auth_middleware(
    State(state): State<AppState>,
    mut req: axum::extract::Request,
    next: axum::middleware::Next,
) -> Response {
    let path = req.uri().path().to_string();
    let required_scope = required_pat_scope(&path);
    let mcp_call = req
        .extensions()
        .get::<crate::agent_scope::McpToolCall>()
        .cloned();
    let grant = match pat_to_bearer_jwt(&state, req.headers(), required_scope).await {
        Ok(Some(Bridged { jwt, grant })) => {
            if let Ok(value) = format!("Bearer {jwt}").parse() {
                req.headers_mut().insert(header::AUTHORIZATION, value);
            }
            grant
        }
        Ok(None) => None,
        Err(err) => return err.into_response(),
    };
    // An inner call the MCP endpoint dispatched carries the JWT the outer
    // request was bridged to, plus the grant resolved for it then — both set
    // in-process, neither expressible by a client. The raw token never travels
    // twice; its narrowing does.
    let grant = grant.or_else(|| {
        mcp_call.as_ref().and_then(|_| {
            req.extensions()
                .get::<crate::agent_scope::TokenGrant>()
                .cloned()
        })
    });
    let Some(grant) = grant else {
        return next.run(req).await;
    };

    // Which tool an inner call may serve was decided by the MCP endpoint, the
    // only producer of `McpToolCall`, before it dispatched; what is left here is
    // the token used anywhere else.
    if mcp_call.is_none() && grant.is_mcp_only() && !crate::agent_scope::is_mcp_endpoint(&path) {
        return grant
            .deny(
                req.headers(),
                "this token may be used only through the MCP endpoint",
                serde_json::json!({
                    "reason": "mcp_only",
                    "method": req.method().as_str(),
                    "path": path,
                }),
            )
            .await
            .into_response();
    }

    // One budget per bot account, however many tokens or addresses it uses.
    // The MCP endpoint's own request is what counts for a tool call; the
    // in-process calls it makes on the agent's behalf are not counted again.
    if grant.owner().is_bot()
        && mcp_call.is_none()
        && !state
            .agent_rate_limiter
            .allow_key(&format!("agent:{}", grant.owner().id))
    {
        if let Some(c) = crate::metrics::rate_limit::BLOCKED.get() {
            c.inc();
        }
        return error::AppError::rate_limited(
            "this agent account has exhausted its request budget; try again later",
        )
        .into_response();
    }

    let context = grant.credential_context(mcp_call.map(|call| call.tool));
    req.extensions_mut().insert(grant);
    rg_core::auth::credential_context::scope(context, next.run(req)).await
}

/// What [`pat_to_bearer_jwt`] hands the middleware: the JWT to put in front of
/// the handlers, and the token behind it when it was a PAT.
struct Bridged {
    jwt: String,
    grant: Option<crate::agent_scope::TokenGrant>,
}

/// If the Authorization header carries a valid PAT (and not already a valid
/// JWT), return a freshly-minted JWT for the token's owner. Returns the
/// existing JWT for a Basic-auth request that carries one. Otherwise `None`.
async fn pat_to_bearer_jwt(
    state: &AppState,
    headers: &axum::http::HeaderMap,
    required_scope: Option<&str>,
) -> Result<Option<Bridged>, error::AppError> {
    let Some(auth) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(None);
    };

    let candidates: Vec<String> = if let Some(t) = auth
        .strip_prefix("Bearer ")
        .or_else(|| auth.strip_prefix("token "))
    {
        // A valid JWT bearer needs no translation.
        if rg_core::auth::jwt::validate_token(t, &state.jwt_secret).is_some() {
            return Ok(None);
        }
        vec![t.to_string()]
    } else if let Some(encoded) = auth.strip_prefix("Basic ") {
        use base64::Engine as _;
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return Ok(None);
        };
        let Ok(creds) = String::from_utf8(decoded) else {
            return Ok(None);
        };
        let Some((user, pass)) = creds.split_once(':') else {
            return Ok(None);
        };
        // Token may be in either the password (`user:token`) or username field.
        [pass, user]
            .into_iter()
            .filter(|c| !c.is_empty())
            .map(|c| c.to_string())
            .collect()
    } else if !auth.contains(' ') && !auth.is_empty() {
        // A bare, scheme-less credential. `cargo publish` sends the registry
        // token exactly this way — `Authorization: <token>`, no `Bearer` — so
        // without this branch the whole cargo write API answers 401 to a
        // correctly configured client (card_5a790cc6ac35).
        //
        // Deliberately narrowed to a value carrying no space, so an
        // `Authorization: <UnknownScheme> <secret>` is still ignored rather than
        // having its scheme name tried as a token.
        vec![auth.to_string()]
    } else {
        return Ok(None);
    };

    for cand in candidates {
        // A JWT carried via Basic auth: pass it through as a Bearer token.
        if rg_core::auth::jwt::validate_token(&cand, &state.jwt_secret).is_some() {
            return Ok(Some(Bridged {
                jwt: cand,
                grant: None,
            }));
        }
        if let Some((pat, owner)) = resolve_pat(&state.db, &cand)
            .await
            .map_err(error::AppError::from)?
        {
            if let Some(scope) = required_scope
                .filter(|scope| !rg_core::auth::pat_scope::has_scope(&pat.scopes, scope))
            {
                // Reaching past the token's scope is the same kind of event as
                // reaching past its repositories, and the journal says so.
                crate::agent_scope::record_scope_denial(
                    &state.db,
                    &owner,
                    pat.id,
                    headers,
                    serde_json::json!({ "reason": "scope", "required_scope": scope }),
                )
                .await;
                return Err(error::AppError::forbidden(
                    "personal access token scope denied",
                ));
            }
            // Tagged with the token it came from. The generation still goes in
            // — `session_standing_middleware` compares it, and a PAT presented
            // by a disabled account must not sail past that — but a handler
            // minting something that outlives this request needs to know the
            // credential was a PAT, whose revocation is its row going away and
            // not a generation bump (card_e4e177acd095).
            let jwt = rg_core::auth::jwt::generate_token_for_pat(
                pat.user_id,
                &owner.username,
                owner.session_version,
                pat.id,
                &state.jwt_secret,
                1,
            )
            .map_err(error::AppError::from)?;
            let grant = crate::agent_scope::TokenGrant::load(&state.db, pat, owner)
                .await
                .map_err(error::AppError::from)?;
            return Ok(Some(Bridged {
                jwt,
                grant: Some(grant),
            }));
        }
    }
    Ok(None)
}

/// Who is calling a git-over-HTTP endpoint, and through which token.
pub(crate) struct GitCredential {
    pub(crate) user_id: i64,
    /// The Personal Access Token presented, when it was one; `None` for a
    /// session JWT.
    pub(crate) grant: Option<crate::agent_scope::TokenGrant>,
}

/// Extract the authenticated caller from a git-over-HTTP request.
///
/// Supports both JWT session tokens and Personal Access Tokens (PATs),
/// presented either as `Authorization: Bearer <token>` or HTTP Basic auth
/// (`git clone https://user:<token>@host/...`). Returns `Ok(None)` for
/// anonymous access (public repos still work; private repos are then rejected)
/// and propagates failures while looking up a presented PAT.
///
/// Git is not behind the REST PAT middleware, so a PAT's narrowing travels out
/// of here with it and the git handlers apply it themselves.
pub(crate) async fn extract_git_credential(
    db: &DatabaseConnection,
    headers: &axum::http::HeaderMap,
    jwt_secret: &str,
) -> anyhow::Result<Option<GitCredential>> {
    let Some(auth_str) = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(None);
    };

    let candidates: Vec<&str> = if let Some(token) = auth_str.strip_prefix("Bearer ") {
        vec![token]
    } else if let Some(encoded) = auth_str.strip_prefix("Basic ") {
        use base64::Engine as _;
        let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(encoded) else {
            return Ok(None);
        };
        let Ok(credentials) = String::from_utf8(decoded) else {
            return Ok(None);
        };
        let Some((username, password)) = credentials.split_once(':') else {
            return Ok(None);
        };
        // Git clients carry the token in either the password (`user:token`) or
        // the username (`token:x-oauth-basic`) field — try both, JWT then PAT.
        return git_credential_from(
            db,
            jwt_secret,
            [password, username]
                .into_iter()
                .filter(|candidate| !candidate.is_empty()),
        )
        .await;
    } else {
        Vec::new()
    };
    git_credential_from(db, jwt_secret, candidates.into_iter()).await
}

async fn git_credential_from<'a>(
    db: &DatabaseConnection,
    jwt_secret: &str,
    candidates: impl Iterator<Item = &'a str>,
) -> anyhow::Result<Option<GitCredential>> {
    for candidate in candidates {
        // JWT session token first, then a PAT.
        if let Some(claims) = rg_core::auth::jwt::validate_token(candidate, jwt_secret) {
            return Ok(claims.sub.parse().ok().map(|user_id| GitCredential {
                user_id,
                grant: None,
            }));
        }
        if let Some((pat, owner)) = resolve_pat(db, candidate).await? {
            if rg_core::auth::pat_scope::has_scope(&pat.scopes, "repo") {
                let user_id = pat.user_id;
                let grant = crate::agent_scope::TokenGrant::load(db, pat, owner).await?;
                return Ok(Some(GitCredential {
                    user_id,
                    grant: Some(grant),
                }));
            }
        }
    }
    Ok(None)
}

#[cfg(test)]
mod tests {
    use super::resolve_pat;
    use sea_orm::{ActiveModelTrait, ActiveValue::NotSet, ConnectionTrait, Set};
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct CapturedLogs(Arc<Mutex<Vec<u8>>>);

    impl CapturedLogs {
        fn text(&self) -> String {
            String::from_utf8(self.0.lock().expect("log lock").clone()).expect("logs are UTF-8")
        }
    }

    impl std::io::Write for CapturedLogs {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.0.lock().expect("log lock").extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl tracing_subscriber::fmt::MakeWriter<'_> for CapturedLogs {
        type Writer = CapturedLogs;

        fn make_writer(&self) -> Self::Writer {
            self.clone()
        }
    }

    async fn pat_fixture() -> (rg_db::DatabaseConnection, String, i64) {
        use sea_orm::{ConnectOptions, Database};
        use sha2::{Digest, Sha256};

        let mut options = ConnectOptions::new("sqlite::memory:");
        options.max_connections(1);
        let db = Database::connect(options)
            .await
            .expect("connect to in-memory db");
        rg_db::run_migrations(&db).await.expect("run migrations");
        let now = chrono::Utc::now();
        rg_db::entities::user::ActiveModel {
            id: Set(1),
            username: Set("pat-touch".to_string()),
            email: Set("pat-touch@example.test".to_string()),
            password_hash: Set("x".to_string()),
            is_admin: Set(false),
            is_active: Set(true),
            created_at: Set(now),
            updated_at: Set(now),
            ..Default::default()
        }
        .insert(&db)
        .await
        .expect("insert PAT owner");

        let raw = "ifp_touch_failure_fixture".to_string();
        let hash = format!("{:x}", Sha256::digest(raw.as_bytes()));
        let token = rg_db::ops::token_ops::create(
            &db,
            rg_db::entities::access_token::ActiveModel {
                id: NotSet,
                user_id: Set(1),
                name: Set("touch failure".to_string()),
                token_hash: Set(hash),
                scopes: Set("user, repo".to_string()),
                expires_at: Set(None),
                last_used_at: Set(None),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            },
            &[],
        )
        .await
        .expect("create PAT");
        (db, raw, token.id)
    }

    /// A write-only failure cannot invalidate a credential that was already
    /// proven by healthy reads, but it must remain visible to the operator.
    #[tokio::test]
    async fn failed_last_used_touch_is_best_effort_and_logged() {
        let (db, raw, token_id) = pat_fixture().await;
        db.execute_unprepared(
            "CREATE TRIGGER fail_pat_touch BEFORE UPDATE ON access_tokens \
             BEGIN SELECT RAISE(ABORT, 'injected PAT touch failure'); END;",
        )
        .await
        .expect("arm PAT touch failure");
        let logs = CapturedLogs::default();
        let subscriber = tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_max_level(tracing::Level::WARN)
            .with_ansi(false)
            .finish();
        let _guard = tracing::subscriber::set_default(subscriber);

        let resolved = resolve_pat(&db, &raw).await.expect("resolve valid PAT");

        assert!(
            resolved.is_some(),
            "usage bookkeeping must not reject the PAT"
        );
        let stored = rg_db::ops::token_ops::find_by_id(&db, token_id)
            .await
            .expect("reload PAT")
            .expect("PAT still exists");
        assert_eq!(
            stored.last_used_at, None,
            "the injected write really failed"
        );
        let rendered = logs.text();
        assert!(
            rendered.contains("failed to update personal access token usage time"),
            "{rendered}"
        );
        assert!(
            rendered.contains("injected PAT touch failure"),
            "{rendered}"
        );
    }

    /// Owner finalization is part of the credential proof, unlike usage
    /// bookkeeping. A failed database write must therefore remain an error and
    /// must happen before `last_used_at` is touched.
    #[tokio::test]
    async fn failed_owner_finalization_is_not_an_invalid_pat() {
        let (db, raw, token_id) = pat_fixture().await;
        db.execute_unprepared(
            "CREATE TRIGGER fail_pat_owner_finalization \
             BEFORE UPDATE OF session_version ON users \
             BEGIN SELECT RAISE(ABORT, 'injected PAT owner finalization failure'); END;",
        )
        .await
        .expect("arm PAT owner-finalization failure");

        let error = resolve_pat(&db, &raw)
            .await
            .expect_err("owner-finalization failure became an invalid PAT");
        assert!(
            format!("{error:#}").contains("injected PAT owner finalization failure"),
            "unexpected error: {error:#}"
        );
        let stored = rg_db::ops::token_ops::find_by_id(&db, token_id)
            .await
            .expect("reload PAT")
            .expect("PAT still exists");
        assert_eq!(
            stored.last_used_at, None,
            "usage bookkeeping ran before mandatory owner finalization"
        );
    }

    /// SQLite triggers put each lifecycle loss inside the real conditional
    /// owner update. The resolver must publish neither its stale owner nor a
    /// successful usage timestamp after either outcome.
    #[tokio::test]
    async fn retirement_or_delete_wins_pat_owner_finalization() {
        for (index, delete) in [false, true].into_iter().enumerate() {
            let (db, raw, token_id) = pat_fixture().await;
            let mutation = if delete {
                "DELETE FROM users WHERE id = OLD.id;"
            } else {
                "UPDATE users SET deleted_at = CURRENT_TIMESTAMP, updated_at = CURRENT_TIMESTAMP \
                 WHERE id = OLD.id;"
            };
            db.execute_unprepared(&format!(
                "CREATE TRIGGER lose_pat_owner_{index} \
                 BEFORE UPDATE OF session_version ON users WHEN OLD.id = 1 \
                 BEGIN {mutation} SELECT RAISE(IGNORE); END;"
            ))
            .await
            .expect("install competing PAT-owner lifecycle mutation");

            assert!(
                resolve_pat(&db, &raw)
                    .await
                    .expect("lifecycle loss is not a database failure")
                    .is_none(),
                "PAT resolver published a stale owner after lifecycle loss"
            );
            let stored = rg_db::ops::token_ops::find_by_id(&db, token_id)
                .await
                .expect("reload PAT after lifecycle loss");
            if delete {
                assert!(
                    stored.is_none(),
                    "account deletion did not cascade to its PAT"
                );
            } else {
                assert_eq!(
                    stored
                        .expect("retirement must not delete the PAT")
                        .last_used_at,
                    None,
                    "a rejected PAT was recorded as used"
                );
            }
        }
    }
}
