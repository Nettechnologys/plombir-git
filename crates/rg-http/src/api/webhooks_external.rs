//! External CI/CD webhook receiver.
//!
//! Receives webhook events from Jenkins, GitHub Actions, or any generic CI/CD
//! system and re-publishes them as commit status updates.
//!
//! POST /api/v1/repos/{owner}/{name}/webhooks/external/ci
//!
//! Expected JSON body:
//! ```json
//! {
//!   "context": "jenkins/my-pipeline",
//!   "state": "success" | "failure" | "pending" | "error",
//!   "description": "Build #42 passed",
//!   "target_url": "https://jenkins.example.com/job/42"
//! }
//! ```

use axum::{
    body::Bytes,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

use crate::api::repo_access::RepoWrite;
use crate::AppState;

/// The ceiling on one inbound external-CI webhook body.
///
/// The payload is a commit status — `context`, `state`, and two optional short
/// strings — so a legitimate request is measured in hundreds of bytes. The
/// route used to declare nothing, which is not "no limit": `raw_body: Bytes`
/// buffers, so it silently ran on Axum's 2 MiB `DefaultBodyLimit`, and any
/// account holding `RepoWrite` could make the server hold 2 MiB, run
/// HMAC-SHA256 over all of it, and hand it to serde. 64 KiB is the same order
/// as `api::artifacts::ARTIFACT_METADATA_MAX_BYTES`, the other small
/// declared-envelope route, and leaves several orders of magnitude of room over
/// what any CI system sends here.
pub(crate) const EXTERNAL_CI_WEBHOOK_MAX_BYTES: usize = 64 * 1024;

/// HTTP header carrying the hex HMAC-SHA256 signature of the raw webhook body,
/// in the `sha256=<hex>` form Plombir Git also emits on *outgoing* webhooks
/// (`rg-core/src/webhook/service.rs`). Same scheme in both directions.
const SIGNATURE_HEADER: &str = "X-Hub-Signature-256";

/// Verify the `X-Hub-Signature-256` header against the raw request body using
/// HMAC-SHA256 with `secret`, comparing in **constant time**.
///
/// Mirrors the signature Plombir Git produces for outgoing deliveries, so a repo
/// can be pointed at its own inbound endpoint symmetrically. Returns a short,
/// caller-facing reason on failure (logged, never sent verbatim to the client).
fn verify_hub_signature(
    secret: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), &'static str> {
    use hmac::{Hmac, Mac};
    use sha2::Sha256;
    type HmacSha256 = Hmac<Sha256>;

    let header = headers
        .get(SIGNATURE_HEADER)
        .and_then(|v| v.to_str().ok())
        .ok_or("missing signature header")?;
    let hex_sig = header
        .strip_prefix("sha256=")
        .ok_or("malformed signature (expected sha256=<hex>)")?;
    let provided = hex::decode(hex_sig).map_err(|_| "signature is not valid hex")?;

    let mut mac =
        HmacSha256::new_from_slice(secret.as_bytes()).map_err(|_| "HMAC key init failed")?;
    mac.update(body);
    // `verify_slice` is a constant-time comparison (guards against timing oracles).
    mac.verify_slice(&provided)
        .map_err(|_| "signature mismatch")
}

#[derive(Debug, Deserialize, ToSchema)]
pub struct ExternalCiWebhook {
    pub context: String,
    pub state: String, // "success", "failure", "pending", "error"
    pub description: Option<String>,
    pub target_url: Option<String>,
}

#[derive(Debug, Serialize, ToSchema)]
pub struct ExternalCiResponse {
    pub id: i64,
    pub status: String,
}

/// POST /api/v1/repos/{owner}/{name}/webhooks/external/ci
#[utoipa::path(
    post,
    path = "/repos/{owner}/{name}/webhooks/external/ci",
    tag = "Repositories",
    params(
        ("owner" = String, Path, description = "Repository owner"),
        ("name" = String, Path, description = "Repository name"),
    ),
    request_body = ExternalCiWebhook,
    responses(
        (status = 200, description = "Commit status created", body = ExternalCiResponse),
        (status = 400, description = "Invalid state or input"),
        (status = 401, description = "Authentication required"),
        (status = 403, description = "Write access to the repository required"),
        (status = 404, description = "Repository not found"),
        (status = 413, description = "Payload above the 64 KiB request-body ceiling"),
    ),
)]
pub async fn external_ci_webhook(
    State(state): State<AppState>,
    headers: HeaderMap,
    // The gate. Writing a commit status is a write on the repository named in
    // the path, so this endpoint owes exactly what `POST /repos/{owner}/{name}/
    // statuses/{sha}` owes — it used to authenticate and stop there, which let
    // any account paint any repository's commits, private ones included.
    // The shared HMAC secret below is not a substitute: it is optional and
    // instance-wide, so every CI wired to this server knows it.
    RepoWrite { repo, actor_id }: RepoWrite,
    // Raw body: we must verify the HMAC over the exact bytes the client sent,
    // so JSON parsing is deferred until after the signature check.
    raw_body: Bytes,
) -> impl IntoResponse {
    // Defense-in-depth: when an inbound-webhook secret is configured, require a
    // valid HMAC-SHA256 signature over the raw body. Opt-in — with no secret the
    // endpoint relies on the access gate alone.
    if let Some(secret) = state.external_webhook_secret.as_ref() {
        if let Err(reason) = verify_hub_signature(secret, &headers, &raw_body) {
            tracing::warn!(
                reason,
                repo_id = repo.id,
                repo = %repo.name,
                "external CI webhook rejected: HMAC signature verification failed"
            );
            return crate::error::AppError::unauthorized("invalid webhook signature")
                .into_response();
        }
    }

    // Parse the JSON body (after the signature check, over the same raw bytes).
    let body: ExternalCiWebhook = match serde_json::from_slice(&raw_body) {
        Ok(parsed) => parsed,
        Err(e) => {
            return crate::error::AppError::bad_request(format!("invalid JSON body: {e}"))
                .into_response();
        }
    };

    // Validate state
    let valid_states = ["success", "failure", "pending", "error"];
    if !valid_states.contains(&body.state.as_str()) {
        return crate::error::AppError::bad_request(format!(
            "invalid state '{}': must be one of {:?}",
            body.state, valid_states
        ))
        .into_response();
    }

    // Create commit status (without sha — will be associated later via push).
    // Keep this path behind the core service so a repository cascade that wins
    // after `RepoWrite` is still the same typed 404 as the ordinary status API.
    let status = match rg_core::repo::service::create_commit_status(
        &state.db,
        repo.id,
        "", // empty sha — callers should set via a follow-up webhook or API
        &body.state,
        &body.context,
        body.description.as_deref(),
        body.target_url.as_deref(),
        actor_id,
    )
    .await
    {
        Ok(s) => s,
        Err(e) => return crate::error::AppError::from(e).into_response(),
    };

    (
        StatusCode::OK,
        Json(ExternalCiResponse {
            id: status.id,
            status: status.state,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compute the `sha256=<hex>` header value the way an external sender would.
    fn sign(secret: &str, body: &[u8]) -> String {
        use hmac::{Hmac, Mac};
        use sha2::Sha256;
        let mut mac = Hmac::<Sha256>::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        format!("sha256={}", hex::encode(mac.finalize().into_bytes()))
    }

    fn headers_with_sig(sig: &str) -> HeaderMap {
        let mut h = HeaderMap::new();
        h.insert(SIGNATURE_HEADER, sig.parse().unwrap());
        h
    }

    #[test]
    fn accepts_a_correct_signature() {
        let secret = "shared-webhook-secret";
        let body = br#"{"context":"ci","state":"success"}"#;
        let headers = headers_with_sig(&sign(secret, body));
        assert!(verify_hub_signature(secret, &headers, body).is_ok());
    }

    #[test]
    fn rejects_a_wrong_signature() {
        let body = br#"{"context":"ci","state":"success"}"#;
        // Signature computed with a different secret.
        let headers = headers_with_sig(&sign("attacker-secret", body));
        assert!(verify_hub_signature("shared-webhook-secret", &headers, body).is_err());
    }

    #[test]
    fn rejects_a_tampered_body() {
        let secret = "shared-webhook-secret";
        let signed_body = br#"{"context":"ci","state":"success"}"#;
        let headers = headers_with_sig(&sign(secret, signed_body));
        // Same signature, but the delivered body differs by one byte.
        let tampered = br#"{"context":"ci","state":"failure"}"#;
        assert!(verify_hub_signature(secret, &headers, tampered).is_err());
    }

    #[test]
    fn rejects_a_missing_header() {
        let headers = HeaderMap::new();
        assert_eq!(
            verify_hub_signature("s", &headers, b"body"),
            Err("missing signature header")
        );
    }

    #[test]
    fn rejects_a_malformed_header() {
        // No `sha256=` prefix.
        let headers = headers_with_sig("deadbeef");
        assert!(verify_hub_signature("s", &headers, b"body").is_err());
        // Prefix present but the payload is not hex.
        let headers = headers_with_sig("sha256=not-hex!!");
        assert_eq!(
            verify_hub_signature("s", &headers, b"body"),
            Err("signature is not valid hex")
        );
    }
}
