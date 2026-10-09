use crate::api::access_audit::{grant_actor, record_grant};
use crate::api::repo_access::RepoAdmin;
use crate::{error::AppError, AppState};
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

#[derive(Debug, Deserialize, ToSchema)]
pub struct PutSecretRequest {
    pub value: String,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct SecretResponse {
    pub name: String,
    pub created_at: chrono::DateTime<chrono::Utc>,
    pub updated_at: chrono::DateTime<chrono::Utc>,
}

fn response(secret: rg_db::entities::ci_secret::Model) -> SecretResponse {
    SecretResponse {
        name: secret.name,
        created_at: secret.created_at,
        updated_at: secret.updated_at,
    }
}

pub(crate) fn valid_secret_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('_' | 'A'..='Z'))
        && chars.all(|ch| matches!(ch, '_' | 'A'..='Z' | '0'..='9'))
        && name.len() <= 100
        // The `[A-Z_][A-Z0-9_]*` check above is narrower than, and therefore
        // implies, the shared `[A-Za-z_][A-Za-z0-9_]*` environment-name shape.
        // Stated here too so a future widening cannot silently admit a name
        // that `execve`/`getenv` reads as something else (`PATH=/tmp` reads
        // back as `PATH`) — security audit finding #2, `=`-name follow-up.
        && rg_core::ci::valid_environment_name(name)
        // The runner's own names and every name that would reconfigure the
        // host `docker` CLI the secret passes through (`LD_PRELOAD`,
        // `DOCKER_HOST`, proxies…): security audit finding #2.
        && !rg_core::ci::is_reserved_ci_variable(name)
}

#[utoipa::path(get, path = "/repos/{owner}/{name}/actions/secrets", tag = "CI/CD", params(("owner" = String, Path), ("name" = String, Path)), responses((status = 200, body = [SecretResponse]), (status = 403, body = serde_json::Value)))]
pub async fn list(
    State(state): State<AppState>,
    Path((_, _)): Path<(String, String)>,
    RepoAdmin { repo, .. }: RepoAdmin,
) -> impl IntoResponse {
    match rg_db::ops::ci_secret_ops::list_by_repo(&state.db, repo.id).await {
        Ok(items) => (
            StatusCode::OK,
            Json(items.into_iter().map(response).collect::<Vec<_>>()),
        )
            .into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(put, path = "/repos/{owner}/{name}/actions/secrets/{secret_name}", tag = "CI/CD", request_body = PutSecretRequest, params(("owner" = String, Path), ("name" = String, Path), ("secret_name" = String, Path)), responses((status = 201, body = SecretResponse), (status = 400, body = serde_json::Value), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn put(
    State(state): State<AppState>,
    Path((owner, _, secret_name)): Path<(String, String, String)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
    Json(body): Json<PutSecretRequest>,
) -> impl IntoResponse {
    // A CI secret is repository-scoped, so it goes through the repository
    // journal rather than the account one: every job of this repository reads
    // it, which makes writing one a change to what a push can reach.
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    if !valid_secret_name(&secret_name) {
        return AppError::bad_request("secret names must match [A-Z_][A-Z0-9_]*, be at most 100 characters, and not use reserved CI names or names that configure the host (PATH, HOME, LD_*, DYLD_*, DOCKER_*, the Go runtime knobs GODEBUG/GOTRACEBACK/GOMEMLIMIT/GOMAXPROCS/GOGC/GOTMPDIR/GOENV/GORACE, SSL_CERT_*, *_PROXY, LC_*, TMPDIR, LANG)").into_response();
    }
    if body.value.len() < 4 || body.value.len() > 65_536 {
        return AppError::bad_request("secret value must contain 4-65536 bytes").into_response();
    }
    let key = rg_core::auth::encryption::derive_key(&state.encryption_key);
    let encrypted = match rg_core::auth::encryption::encrypt(&body.value, &key) {
        Ok(v) => v,
        Err(e) => return AppError::from(e).into_response(),
    };
    match rg_db::ops::ci_secret_ops::upsert(&state.db, repo.id, &secret_name, &encrypted, actor_id)
        .await
    {
        Ok(Some(item)) => {
            // The name, and whether this replaced a value that was already
            // there — a rotation and a first write are different events to
            // whoever is reading. Never `body.value`, and never `encrypted`:
            // the ciphertext is the secret to anyone holding the instance key.
            let rotated = item.updated_at != item.created_at;
            record_grant(
                &state,
                &audit_actor,
                "repo.set_ci_secret",
                &owner,
                &repo,
                &headers,
                serde_json::json!({ "secret": item.name, "rotated": rotated }),
            )
            .await;
            (StatusCode::CREATED, Json(response(item))).into_response()
        }
        Ok(None) => AppError::not_found("CI secret not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[utoipa::path(delete, path = "/repos/{owner}/{name}/actions/secrets/{secret_name}", tag = "CI/CD", params(("owner" = String, Path), ("name" = String, Path), ("secret_name" = String, Path)), responses((status = 204), (status = 403, body = serde_json::Value), (status = 404, body = serde_json::Value)))]
pub async fn delete(
    State(state): State<AppState>,
    Path((owner, _, secret_name)): Path<(String, String, String)>,
    RepoAdmin { repo, actor_id }: RepoAdmin,
    headers: HeaderMap,
) -> impl IntoResponse {
    let audit_actor = match grant_actor(&state, actor_id).await {
        Ok(actor) => actor,
        Err(error) => return error.into_response(),
    };
    match rg_db::ops::ci_secret_ops::delete_by_repo_and_name(&state.db, repo.id, &secret_name).await
    {
        Ok(true) => {
            record_grant(
                &state,
                &audit_actor,
                "repo.remove_ci_secret",
                &owner,
                &repo,
                &headers,
                serde_json::json!({ "secret": secret_name }),
            )
            .await;
            StatusCode::NO_CONTENT.into_response()
        }
        Ok(false) => AppError::not_found("CI secret not found").into_response(),
        Err(e) => AppError::from(e).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_secret_names() {
        assert!(valid_secret_name("DEPLOY_TOKEN_2"));
        assert!(!valid_secret_name("deploy_token"));
        assert!(!valid_secret_name("CI_JOB_TOKEN"));
        assert!(!valid_secret_name("CI_REPOSITORY"));
        assert!(!valid_secret_name("CI_REPOSITORY_OWNER"));
    }

    /// A secret is read into the host `docker` CLI's environment before it
    /// reaches the container, so a name that configures that CLI — or the
    /// loader underneath it — is refused at creation (security audit finding #2).
    #[test]
    fn refuses_names_that_configure_the_runner_host() {
        for name in [
            "PATH",
            "HOME",
            "TMPDIR",
            "LANG",
            "LC_ALL",
            "LD_PRELOAD",
            "LD_LIBRARY_PATH",
            "DYLD_INSERT_LIBRARIES",
            "DOCKER_HOST",
            "DOCKER_CONFIG",
            "DOCKER_CERT_PATH",
            "DOCKER_TLS_VERIFY",
            "GODEBUG",
            "GOTRACEBACK",
            "SSL_CERT_FILE",
            "SSL_CERT_DIR",
            "HTTP_PROXY",
            "HTTPS_PROXY",
            "NO_PROXY",
            "ALL_PROXY",
        ] {
            assert!(!valid_secret_name(name), "{name} must be refused");
        }
        assert!(valid_secret_name("AWS_SECRET_ACCESS_KEY"));
        assert!(valid_secret_name("PROXY_PASSWORD"));
        // Go toolchain/application names pass: they never reach the Docker CLI
        // process (security audit finding #2, follow-up review).
        assert!(valid_secret_name("GOFLAGS"));
        assert!(valid_secret_name("GOPROXY"));
        assert!(valid_secret_name("GOOGLE_APPLICATION_CREDENTIALS"));
        // And the shape the shared predicate enforces is implied here: a name
        // with `=` (or any other non-NAME byte) never passes.
        for name in [
            "PATH=/tmp",
            "HTTPS_PROXY=http://attacker:8080",
            "LD_PRELOAD=/workspace/evil.so",
        ] {
            assert!(!valid_secret_name(name), "{name} must be refused");
        }
    }
}
