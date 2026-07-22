//! Git Smart Protocol V2 HTTP handling.
//!
//! Protocol V2 over HTTP uses the same endpoints as V1, but:
//! 1. Client sends `Git-Protocol: version=2` header
//! 2. Server responds with V2 capability advertisement
//! 3. Subsequent requests use V2 command format
//!
//! Reference: <https://git-scm.com/docs/protocol-v2>

use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
    Json,
};
use tokio::io::AsyncWriteExt;

use crate::AppState;
use rg_git::protocol::v2::{handle_v2_http, ADVERTISED_CAPABILITIES};

/// Check if client wants Protocol V2 based on HTTP headers.
pub fn wants_protocol_v2(headers: &HeaderMap) -> bool {
    if let Some(git_protocol) = headers.get("Git-Protocol") {
        if let Ok(protocol) = git_protocol.to_str() {
            return protocol.contains("version=2");
        }
    }
    false
}

/// Build V2 capability advertisement synchronously.
/// Uses the same format as build_v2_capability_advertisement in lib.rs.
fn build_v2_capability_sync() -> Vec<u8> {
    let mut buf = Vec::new();

    // Smart HTTP: info/refs response starts with pkt-line wrapped # service= header
    let service_line = "# service=git-upload-pack\n";
    let service_len = service_line.len() + 4;
    buf.extend_from_slice(format!("{:04x}{}", service_len, service_line).as_bytes());
    buf.extend_from_slice(b"0000"); // flush after service header

    // Helper to write pkt-line data
    let write_pkt = |buf: &mut Vec<u8>, text: &str| {
        let payload = text.as_bytes();
        let len = payload.len() + 4 + 1; // +4 for hex header, +1 for trailing \n
        buf.extend_from_slice(format!("{:04x}{}\n", len, text).as_bytes());
    };

    write_pkt(&mut buf, "version 2");
    for capability in ADVERTISED_CAPABILITIES {
        write_pkt(&mut buf, capability);
    }
    buf.extend_from_slice(b"0000");

    buf
}

/// Handle GET /git/{owner}/{repo}/info/refs
/// Protocol V2 negotiation happens on first request with Git-Protocol header.
/// Git Smart HTTP `/info/refs` endpoint (Protocol V2).
///
/// CRITICAL: Content-Type handling (踩坑经验 #6)
///
/// Git Smart HTTP is VERY sensitive to Content-Type headers.
/// Incorrect Content-Type causes `git` client to fail with:
///   "fatal: protocol error: bad line length character"
///
/// Correct Content-Types:
/// - info/refs response:
///   `application/x-git-upload-pack-advertisement`
///   `application/x-git-receive-pack-advertisement`
/// - POST request body:
///   `application/x-git-upload-pack-request`
///   `application/x-git-receive-pack-request`
/// - POST response body:
///   `application/x-git-upload-pack-result`
///   `application/x-git-receive-pack-result`
///
/// Common mistake: using `text/plain` or wrong subtype breaks git clients.
pub async fn handle_info_refs_v2(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
) -> impl IntoResponse {
    // Check if client wants V2
    if !wants_protocol_v2(&headers) {
        // Fall back to V1 - this should be handled by the regular handler
        // In practice, the router should check the header and route accordingly
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Protocol V2 not requested",
                "hint": "Send Git-Protocol: version=2 header"
            })),
        )
            .into_response();
    }

    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": e.to_string()
            })),
        )
            .into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": e.to_string()
            })),
        )
            .into_response();
    }

    // Build repo path
    let repo_path = state.repo_root.join(&owner).join(format!("{}.git", &repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Repository not found"
            })),
        )
            .into_response();
    }

    // For info/refs, we send the capability advertisement
    // The actual refs will be sent when client sends ls-refs command
    let response_body = build_v2_capability_sync();

    (
        StatusCode::OK,
        [
            (
                "Content-Type",
                "application/x-git-upload-pack-advertisement",
            ),
            ("Cache-Control", "no-cache"),
        ],
        response_body,
    )
        .into_response()
}

/// Handle POST /git/{owner}/{repo}/git-upload-pack
/// For Protocol V2, the command processing happens here.
pub async fn handle_git_upload_pack_v2(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // Check if client wants V2
    if !wants_protocol_v2(&headers) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Protocol V2 not requested"
            })),
        )
            .into_response();
    }

    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": e.to_string()
            })),
        )
            .into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": e.to_string()
            })),
        )
            .into_response();
    }

    // Build repo path
    let repo_path = state.repo_root.join(&owner).join(format!("{}.git", &repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Repository not found"
            })),
        )
            .into_response();
    }

    let (reader, mut writer) = tokio::io::duplex(body.len() + 4096);

    // Write request body to the reader side
    if let Err(e) = writer.write_all(&body).await {
        tracing::error!(error = %e, "Failed to write request body");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Bad request"})),
        )
            .into_response();
    }
    drop(writer); // Close writer so reader gets EOF

    // Process V2 protocol
    let mut response_buf = Vec::new();

    match handle_v2_http(&repo_path, reader, &mut response_buf).await {
        Ok(()) => (
            StatusCode::OK,
            [("Content-Type", "application/x-git-upload-pack-result")],
            response_buf,
        )
            .into_response(),
        Err(e) => {
            tracing::error!(error = %e, "V2 upload-pack failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

/// Handle POST /git/{owner}/{repo}/git-receive-pack
/// For Protocol V2 push operations.
pub async fn handle_git_receive_pack_v2(
    State(state): State<AppState>,
    Path((owner, repo)): Path<(String, String)>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> impl IntoResponse {
    // Check if client wants V2
    if !wants_protocol_v2(&headers) {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Protocol V2 not requested"
            })),
        )
            .into_response();
    }

    // H-02: Validate owner/repo before constructing repository path
    if let Err(e) = rg_core::platform::validate_repo_path(&owner) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": e.to_string()
            })),
        )
            .into_response();
    }
    if let Err(e) = rg_core::platform::validate_repo_path(&repo) {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": e.to_string()
            })),
        )
            .into_response();
    }

    // Build repo path
    let repo_path = state.repo_root.join(&owner).join(format!("{}.git", &repo));

    if !repo_path.exists() {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "Repository not found"
            })),
        )
            .into_response();
    }

    // For receive-pack over V2, we still use the V1 receive-pack logic
    // because V2's fetch command is primarily for clone/fetch, not push
    // The push negotiation in V2 still uses similar mechanisms
    let (reader, mut writer) = tokio::io::duplex(body.len() + 4096);

    if let Err(e) = writer.write_all(&body).await {
        tracing::error!(error = %e, "Failed to write request body");
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Bad request"})),
        )
            .into_response();
    }
    drop(writer);

    let mut response_buf = Vec::new();

    match rg_git::protocol::receive_pack::handle_receive_pack_http(
        &repo_path,
        reader,
        &mut response_buf,
    )
    .await
    {
        Ok(_ref_updates) => {
            let body = String::from_utf8(response_buf).unwrap_or_default();
            (
                StatusCode::OK,
                [("Content-Type", "application/x-git-receive-pack-result")],
                body,
            )
                .into_response()
        }
        Err(e) => {
            tracing::error!(error = %e, "V2 receive-pack failed");
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": e.to_string()})),
            )
                .into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::build_v2_capability_sync;

    #[test]
    fn http_advertisement_matches_supported_fetch_features() {
        let output = String::from_utf8(build_v2_capability_sync()).unwrap();

        assert!(output.contains("fetch=shallow filter\n"));
    }
}
